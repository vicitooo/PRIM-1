"""Finite output probes for a root-owned, empty production preview on CDP 9333.

The caller attests the running process is --expected-exe; the driver verifies
both staged and frozen images against --build-manifest, and checks the public
bootstrap runtime/work paths.
No 9222, private invoke/xterm hooks, app stop/reload, or AI harnesses. The load
phase leaves nine Generic Terminals in two rooms for inspection. The separate
stall phase leaves one; its optional busy loop blocks the renderer for six
seconds, while the original debugger pause remains the default.
Parser-ACK latency is not paint latency or total PTY latency.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
import ntpath
from pathlib import Path
import re
import sys
import time
import traceback
import uuid
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cdp import CdpClient, list_targets
from websocket import WebSocketTimeoutException


LINES, LINE_BYTES = 4096, 512  # Exactly 2 MiB printable ASCII; CRLF/END are extra.
BUCKETS = [16, 50, 100, 250, 1000, 2000, 5000]
BUSY_LOOP_SCRIPT = """(()=>{
  if(window.__busyLoopReceipt)throw Error('busy-loop fixture already exists');
  const receipt=window.__busyLoopReceipt={token:__TOKEN__,scheduled_epoch_ms:Date.now()};
  setTimeout(()=>{
    const start_epoch_ms=Date.now(),start_ms=performance.now();
    while(performance.now()-start_ms<6000){}
    Object.assign(receipt,{start_epoch_ms,end_epoch_ms:Date.now(),duration_ms:performance.now()-start_ms});
  },0);
  return {scheduled_epoch_ms:receipt.scheduled_epoch_ms};
})()"""


def require(value, message):
    if not value:
        raise RuntimeError(message)


def normalized(value):
    value = str(value).removeprefix("\\\\?\\")
    require(re.match(r"^[A-Za-z]:[\\/]", value), "absolute local DOS path required")
    require(not any(p in (".", "..") for p in re.split(r"[\\/]", value)), "ambiguous path")
    return ntpath.normcase(ntpath.normpath(value))


def verify_build_images(frozen_exe, staged_exe, digest):
    require(isinstance(digest, str) and re.fullmatch(r"[0-9a-fA-F]{64}", digest), "invalid manifest hash")
    for label, path in (("frozen", frozen_exe), ("staged", staged_exe)):
        normalized(path)
        require(hashlib.sha256(Path(path).read_bytes()).hexdigest() == digest.lower(), label + " executable hash differs")
    return {"frozen_exe": str(frozen_exe), "staged_exe": str(staged_exe), "sha256": digest.lower()}


def wait(label, check, seconds=15):
    deadline = time.monotonic() + seconds
    while True:
        value = check()
        if value:
            return value
        require(time.monotonic() < deadline, "timeout: " + label)
        time.sleep(.2)


def validate_metrics(data):
    require(isinstance(data.get("epoch"), str) and data["epoch"], "missing renderer epoch")
    for key in ("parser_ack_tickets", "max_ms", "over_250_ms", "over_2000_ms",
                "over_5000_ms", "observer_entries", "observer_max_quiet_ms", "current_observing_runs",
                "current_bytes", "peak_bytes", "current_items", "peak_items"):
        require(type(data.get(key)) is int and data[key] >= 0, "invalid metric: " + key)
    require(data.get("latency_bucket_upper_bounds_ms") == BUCKETS, "unknown latency buckets")
    hist = data.get("latency_histogram")
    require(isinstance(hist, list) and len(hist) == 8 and all(type(n) is int and n >= 0 for n in hist), "invalid histogram")
    require(sum(hist) == data["parser_ack_tickets"], "histogram/ticket count mismatch")
    for key, start in (("over_250_ms", 4), ("over_2000_ms", 6), ("over_5000_ms", 7)):
        require(data[key] == sum(hist[start:]), "histogram threshold mismatch: " + key)
    require(data["current_bytes"] <= data["peak_bytes"] and data["current_items"] <= data["peak_items"], "invalid ledger peaks")
    return data


def metrics_delta(before, after):
    require(before["epoch"] == after["epoch"], "renderer epoch changed during fixture")
    result = {}
    for key in ("parser_ack_tickets", "observer_entries", "over_250_ms", "over_2000_ms", "over_5000_ms"):
        result[key] = after[key] - before[key]
        require(result[key] >= 0, "metric counter decreased: " + key)
    result["latency_histogram"] = [b - a for a, b in zip(before["latency_histogram"], after["latency_histogram"])]
    require(all(n >= 0 for n in result["latency_histogram"]), "histogram counter decreased")
    return result


def ps_quote(value):
    return "'" + str(value).replace("'", "''") + "'"


def debugger_command(client, method, seconds=.75, pause_receipts=None):
    """Bound the whole command, including an ongoing stream of CDP events."""
    message_id = client._next_id
    client._next_id += 1
    deadline = time.monotonic() + seconds
    client.ws.settimeout(seconds)
    client.ws.send(json.dumps({"id": message_id, "method": method}))
    replied, paused = False, method != "Debugger.pause"
    while time.monotonic() < deadline:
        client.ws.settimeout(max(.001, deadline - time.monotonic()))
        try:
            message = json.loads(client.ws.recv())
        except WebSocketTimeoutException:
            break
        if message.get("id") == message_id:
            require("error" not in message, method + " refused")
            replied = True
        if message.get("method") == "Debugger.paused":
            paused = True
            if pause_receipts is not None and len(pause_receipts) < 4:
                params = message.get("params", {})
                frames = params.get("callFrames", [])
                pause_receipts.append({
                    "reason": str(params.get("reason", ""))[:128],
                    "call_frames": [{
                        "function": str(frame.get("functionName", ""))[:128],
                        "url": str(frame.get("url", ""))[:384],
                        "line": frame.get("location", {}).get("lineNumber"),
                        "column": frame.get("location", {}).get("columnNumber"),
                    } for frame in frames[:8]],
                    "omitted_frames": max(0, len(frames) - 8),
                })
        if replied and paused:
            return True
    raise RuntimeError(method + " response timed out")


def output_script(token, ordinal, delay_ms, paced, completion=None):
    require(re.fullmatch(r"[0-9a-f]{10}", token) and 1 <= ordinal <= 9, "invalid fixture marker")
    require(0 <= delay_ms <= 15000, "fixture delay exceeds bound")
    # Split the marker literal so echoed shell input cannot count as output.
    script = "$olPrefix='OL_'+'" + token + "'+'_P" + str(ordinal) + "_';"
    script += "Start-Sleep -Milliseconds " + str(delay_ms) + ";"
    script += "for($olN=1;$olN -le 4096;$olN++){[Console]::WriteLine(($olPrefix+('{0:D4}' -f $olN)+'|').PadRight(512,'x'));"
    if paced:
        script += "if(($olN % 64) -eq 0){Start-Sleep -Milliseconds 100};"
    script += "};[Console]::WriteLine($olPrefix+'END');"
    if completion is not None:
        script += "[IO.File]::WriteAllText(" + ps_quote(completion) + "," + ps_quote(token) + ");"
    return script


def validate_busy_loop_timing(receipt):
    browser, completion = receipt["browser"], receipt.get("completion")
    require(6000 <= browser["duration_ms"] < 8000, "busy loop duration outside six-to-eight-second bound")
    require(abs(browser["end_epoch_ms"] - browser["start_epoch_ms"] - browser["duration_ms"]) < 100,
            "browser wall clock changed during busy loop")
    require(abs(receipt["reply_epoch_ms"] - receipt["request_epoch_ms"] - receipt["seconds"] * 1000) < 100,
            "host wall clock changed during busy loop")
    require(receipt["request_epoch_ms"] <= browser["start_epoch_ms"]
            < browser["end_epoch_ms"] <= receipt["reply_epoch_ms"],
            "browser interval is outside host request/reply interval")
    require(receipt["receipt_read_epoch_ms"] >= browser["end_epoch_ms"],
            "receipt read was requested before ordinary task completed")
    require(completion is not None, "native completion was not observed during busy loop")
    require(browser["start_epoch_ms"] + 20 <= completion["mtime_epoch_ms"]
            <= completion["observed_epoch_ms"] <= browser["end_epoch_ms"] - 20,
            "native completion is not strictly inside busy loop interval")


def busy_loop_command(client, completion, token, receipt):
    """Schedule an ordinary timer task; no inspector evaluation holds the loop.

    Poll the file for 6.2 s after scheduling returns, then read the receipt once.
    Both public evaluations share a nine-second deadline. No Debugger commands.
    """
    started = time.monotonic()
    deadline = started + 9

    def evaluate(expression):
        require(time.monotonic() < deadline, "busy loop evaluation deadline elapsed")
        message_id = client._next_id
        client._next_id += 1
        client.ws.settimeout(max(.001, deadline - time.monotonic()))
        client.ws.send(json.dumps({"id": message_id, "method": "Runtime.evaluate", "params": {
            "expression": expression, "returnByValue": True, "allowUnsafeEvalBlockedByCSP": True}}))
        while time.monotonic() < deadline:
            client.ws.settimeout(max(.001, deadline - time.monotonic()))
            try:
                message = json.loads(client.ws.recv())
            except WebSocketTimeoutException:
                break
            if message.get("id") != message_id:
                continue
            require("error" not in message, "busy loop evaluation refused")
            details = message.get("result", {})
            require("exceptionDetails" not in details, "busy loop evaluation threw")
            return details["result"]["value"]
        raise RuntimeError("busy loop response timed out; task is self-terminating")

    receipt["request_epoch_ms"] = time.time_ns() // 1_000_000
    receipt["scheduled"] = evaluate(BUSY_LOOP_SCRIPT.replace("__TOKEN__", json.dumps(token)))
    wait_until = time.monotonic() + 6.2
    require(wait_until < deadline, "scheduling reply left insufficient quiet wait time")
    while time.monotonic() < wait_until:
        try:
            if "completion" not in receipt and completion.exists() and completion.stat().st_size <= 64:
                if completion.read_text(encoding="utf-8-sig") == token:
                    receipt["completion"] = {"observed_epoch_ms": time.time_ns() / 1_000_000,
                        "mtime_epoch_ms": completion.stat().st_mtime_ns / 1_000_000,
                        "seconds_since_request": time.monotonic() - started}
        except (FileNotFoundError, PermissionError):
            pass  # The bounded next poll can see a just-created, still-open file.
        time.sleep(.05)
    receipt["receipt_read_epoch_ms"] = time.time_ns() // 1_000_000
    receipt["browser"] = evaluate("(()=>{const r=window.__busyLoopReceipt;if(!r||r.token!=="
        + json.dumps(token) + ")throw Error('busy-loop receipt identity differs');return r})()")
    receipt["reply_epoch_ms"] = time.time_ns() // 1_000_000
    receipt["seconds"] = time.monotonic() - started
    require("duration_ms" in receipt["browser"], "ordinary busy-loop task did not complete before receipt read")
    validate_busy_loop_timing(receipt)
    receipt["native_completed_while_blocked"] = True


def observer_script(bindings):
    # Bounded state: nine counters + 4096 marker bits and 128 carry chars each;
    # the first 128 output identities globally, without their payloads.
    # Neither raw chunks nor full lifecycle/gap events are retained.
    return """(async()=>{
      if(window.__outputLoadWalk)throw Error('fixture observer already exists');
      const bindings=__BINDINGS__;
      const w=window.__outputLoadWalk={panes:{},unknownOutput:0,unknownGap:0,
        outputTrace:[],outputTraceOmitted:0};
      for(const [sid,b] of Object.entries(bindings))w.panes[sid]={...b,events:0,chars:0,
        markers:0,maxMarker:0,end:false,gaps:0,dropped:0,wrongRun:0,exits:0,
        lastSequence:null,sequenceRegressions:0,carry:'',seen:new Uint8Array(4096)};
      w.unlisten=await window.__TAURI__.event.listen('runtime://event',({payload:p})=>{
        if(!['session_output','ui_output_gap','session_exit'].includes(p.event))return;
        if(p.event==='session_output'){
          if(w.outputTrace.length<128)w.outputTrace.push({
            session_id:p.identity?.session_id,run_id:p.identity?.run_id,
            generation:p.identity?.generation,sequence:p.identity?.sequence,
            synthetic:p.synthetic});
          else w.outputTraceOmitted++;
        }
        const s=w.panes[p.identity?.session_id];
        if(!s){if(p.event==='session_output')w.unknownOutput++;if(p.event==='ui_output_gap')w.unknownGap++;return}
        if(p.identity.run_id!==s.run_id||p.identity.generation!==s.generation){s.wrongRun++;return}
        if(p.event==='ui_output_gap'){s.gaps++;s.dropped+=p.dropped_events;return}
        if(p.event==='session_exit'){s.exits++;return}
        if(s.lastSequence!==null&&p.identity.sequence<=s.lastSequence)s.sequenceRegressions++;
        s.lastSequence=p.identity.sequence;s.events++;s.chars+=p.chunk.length;
        const text=s.carry+p.chunk;s.carry=text.slice(-128);
        for(const m of text.matchAll(new RegExp(s.prefix+'([0-9]{4})\\\\|','g'))){
          const n=Number(m[1]);if(n>=1&&n<=4096&&!s.seen[n-1]){s.seen[n-1]=1;s.markers++;s.maxMarker=Math.max(s.maxMarker,n)}
        }
        if(text.includes(s.prefix+'END'))s.end=true;
      });
      w.snapshot=()=>({unknownOutput:w.unknownOutput,unknownGap:w.unknownGap,
        outputTrace:w.outputTrace,outputTraceOmitted:w.outputTraceOmitted,
        panes:Object.fromEntries(Object.entries(w.panes).map(([sid,{seen,carry,...s}])=>[sid,s]))});
      return true;
    })()""".replace("__BINDINGS__", json.dumps(bindings))


class Walk:
    def __init__(self, args, out, result, journal):
        self.args, self.out, self.result, self.journal = args, out, result, journal
        self.client = None
        self.rooms, self.panes = {}, {}
        self.token = uuid.uuid4().hex[:10]

    def js(self, expression):
        return self.client.evaluate(expression)

    def invoke(self, name, args=None):
        return self.js("window.__TAURI__.core.invoke(" + json.dumps(name) + "," + json.dumps(args or {}) + ")")

    def record(self, name, value):
        item = {"utc": datetime.now(timezone.utc).isoformat(), "name": name, "value": value}
        self.result["steps"].append(item)
        self.journal.write(json.dumps(item) + "\n")
        self.journal.flush()

    def state(self):
        state = self.invoke("bootstrap")
        require(normalized(state["runtime_dir"]) == normalized(self.args.expected_runtime), "wrong scratch runtime")
        require(normalized(state["workspace_preference"]) == normalized(self.args.expected_work), "wrong scratch work root")
        require({s["session_id"] for s in state["sessions"]} == set(self.panes), "fixture session set changed / initial scratch not empty")
        require({r["room_id"] for r in state["rooms"]} == set(self.rooms), "fixture room set changed / initial scratch not empty")
        for room in state["rooms"]:
            require(set(room["member_ids"]) == {sid for sid, p in self.panes.items() if p["room_id"] == room["room_id"]}, "fixture membership changed")
        for s in state["sessions"]:
            expected = self.panes[s["session_id"]]
            require(s["driver"] == "generic_terminal" and s["permission_profile"] == "normal", "non-normal Generic Terminal refused")
            require(normalized(s["working_dir"]) == normalized(self.args.expected_work), "pane left scratch work root")
            if "run_id" in expected:
                require(s["running"] and s["run_id"] == expected["run_id"] and s["generation"] == expected["generation"], "fixture run changed")
        return state

    def metrics(self):
        return validate_metrics(self.invoke("output_credit_metrics"))

    def observation(self):
        data = self.js("window.__outputLoadWalk.snapshot()")
        require(not data["unknownOutput"] and not data["unknownGap"], "output/gap from an unexpected session")
        require(all(not p["wrongRun"] and not p["exits"] and not p["sequenceRegressions"] for p in data["panes"].values()), "fixture output identity/order/lifecycle changed")
        return data

    def screenshot(self, name):
        response = self.client.send("Page.captureScreenshot", {"format": "png"})
        require("error" not in response, "screenshot failed")
        with (self.out / (name + ".png")).open("xb") as stream:
            stream.write(base64.b64decode(response["result"]["data"]))
        return name + ".png"

    def visible(self):
        panes = self.js("[...document.querySelectorAll('[data-session-card]')].filter(c=>!c.hidden&&!c.inert&&c.getClientRects().length).map(c=>({session_id:c.dataset.sessionCard,label:document.querySelector('[data-session-tab=\"'+c.dataset.sessionCard+'\"]')?.textContent.trim()}))")
        require(len(panes) == 1 and panes[0]["session_id"] in self.panes, "expected the product's single visible owned terminal")
        return {"visible_terminal_count": len(panes), "visible_terminals": panes}

    def select(self, sid):
        self.state()
        label = self.rooms[self.panes[sid]["room_id"]]
        self.js("document.querySelector('#context-rooms-chip').click()")
        self.js("(()=>{const cards=[...document.querySelectorAll('.rooms-overview-card:not(.rooms-overview-new)')].filter(e=>e.querySelector('h3')?.textContent===" + json.dumps(label) + ");if(cards.length!==1)throw Error('fixture room missing');cards[0].click()})()")
        wait("fixture tab", lambda: self.js("Boolean(document.querySelector('[data-session-tab=\"" + sid + "\"]'))"))
        self.js("document.querySelector('[data-session-tab=\"" + sid + "\"]').click()")
        wait("visible selected terminal", lambda: self.js("(()=>{const c=document.querySelector('[data-session-card=\"" + sid + "\"]');return Boolean(c&&!c.hidden&&!c.inert&&c.getClientRects().length&&document.querySelector('[data-session-tab=\"" + sid + "\"]').getAttribute('aria-selected')==='true')})()"))

    def create(self, sizes):
        for group, count in enumerate(sizes, 1):
            self.state()
            label = "Output load " + self.token + " / " + str(group)
            rid = self.invoke("create_room", {"request": {"label": label, "member_ids": [], "brief_on_join": False}})["room_id"]
            self.rooms[rid] = label
            for _ in range(count):
                self.state()
                ordinal = len(self.panes) + 1
                pane = self.invoke("create_session", {"request": {"label": "Output fixture " + str(ordinal), "driver": "generic_terminal", "permission_profile": "normal", "room_id": rid}})
                sid = pane["session_id"]
                self.panes[sid] = {"room_id": rid, "ordinal": ordinal, "prefix": "OL_" + self.token + "_P" + str(ordinal) + "_"}
                self.select(sid)
                started = self.invoke("start_session", {"request": {"session_id": sid}})
                require(started["running"] and started["run_id"], "Generic Terminal did not start")
                self.panes[sid].update(run_id=started["run_id"], generation=started["generation"])
        self.state()
        self.record("owned_fixture", {"rooms": self.rooms, "panes": self.panes})
        self.js(observer_script(self.panes))

    def input(self, sid, script):
        self.state()
        self.invoke("send_input", {"request": {"session_id": sid, "input": script + "\r"}})

    def connect(self):
        for path in (self.args.expected_runtime, self.args.expected_work, self.args.expected_exe):
            normalized(path)
        runtime, work = normalized(self.args.expected_runtime), normalized(self.args.expected_work)
        require(not (runtime == work or runtime.startswith(work + "\\") or work.startswith(runtime + "\\")), "runtime/work paths overlap")
        require(Path(self.args.expected_work).is_dir(), "missing expected scratch work directory")
        manifest = json.loads(self.args.build_manifest.read_text(encoding="utf-8-sig"))
        require(re.fullmatch(r"[0-9a-fA-F]{40}", manifest["source_commit"]), "manifest needs full source SHA")
        require(manifest.get("app_identifier") == "io.prim1.v2", "manifest must identify isolated v2 app")
        exe = manifest.get("frozen_exe", manifest.get("exe_path"))
        digest = manifest.get("sha256", manifest.get("exe_sha256"))
        images = verify_build_images(exe, self.args.expected_exe, digest)
        self.result["build"] = {"source_commit": manifest["source_commit"], **images, "app_identifier": manifest["app_identifier"]}
        targets = [t for t in list_targets(9333) if t.get("type") == "page"]
        require(len(targets) == 1, "ambiguous production preview page")
        url = urlparse(targets[0]["webSocketDebuggerUrl"])
        require(url.hostname in ("localhost", "127.0.0.1", "::1") and url.port == 9333, "wrong preview endpoint")
        self.ws_url = targets[0]["webSocketDebuggerUrl"]
        self.client = CdpClient(self.ws_url)
        self.state()  # Empty owned sets above deliberately refuse any prior fixtures.
        self.record("empty_scratch", {"runtime": self.args.expected_runtime, "work": self.args.expected_work, "port": 9333})
        self.metrics()  # Fail before creating anything if the public command is absent.

    def load(self):
        self.create((5, 4))
        before = self.metrics()
        self.record("metrics_before", before)
        ids = list(self.panes)
        self.record("load_mode", {"paced": self.args.paced_load, "printable_bytes_per_pane": LINES * LINE_BYTES})
        # All dispatches target a common finite start time. No gate files or
        # open-ended shell loops. Optional pacing aids cross-room observation.
        start = time.monotonic() + 15
        for sid in ids:
            delay = int((start - time.monotonic()) * 1000)
            require(delay >= 1000, "dispatch exhausted common-start window")
            self.input(sid, output_script(self.token, self.panes[sid]["ordinal"], delay, self.args.paced_load))
        deadline, switched, next_visit = time.monotonic() + 90, 0, 0
        previous_room = self.panes[ids[-1]]["room_id"]
        while True:
            observation = self.observation()
            panes = list(observation["panes"].values())
            require(not any(p["gaps"] for p in panes), "normal load exposed a public output gap")
            if all(p["end"] for p in panes):
                break
            require(time.monotonic() < deadline, "nine-pane completion timed out")
            if any(p["markers"] for p in panes) and time.monotonic() >= next_visit and switched < 12:
                sid = ids[0 if switched % 2 == 0 else 5]
                active_before = sum(p["markers"] > 0 and not p["end"] for p in panes)
                self.select(sid)
                after_visit = self.observation()
                active_after = sum(p["markers"] > 0 and not p["end"] for p in after_visit["panes"].values())
                rid = self.panes[sid]["room_id"]
                switched += 1
                self.record("room_switch", {"session_id": sid, "room_id": rid, "cross_room": rid != previous_room, "active_before": active_before, "active_after": active_after, **self.visible()})
                previous_room, next_visit = rid, time.monotonic() + .4
            time.sleep(.15)
        time.sleep(.3)
        self.state()
        observation = self.observation()
        require(all(p["end"] and p["markers"] == LINES and not p["gaps"] for p in observation["panes"].values()), "missing numbered markers/END or output gaps")
        effective = [s for s in self.result["steps"] if s["name"] == "room_switch" and s["value"]["cross_room"] and s["value"]["active_before"] and s["value"]["active_after"]]
        overlap = {"status": "MEASURED" if len(effective) >= 2 else "UNMEASURED",
                   "qualified_crossings": len(effective),
                   "meaning": "At least two actual room crossings with active output before and after each click."}
        if len(effective) < 2:
            overlap["reason"] = "Output finished before two overlapping room crossings could be observed; no overlap claim."
        self.result["room_switch_overlap"] = overlap
        self.record("room_switch_overlap", overlap)
        if self.args.paced_load:
            require(len(effective) >= 2, "paced load did not permit two overlapping cross-room switches")
        self.record("completed_streams", observation)
        after = wait("parser-ACK ledger drains", lambda: (m if not m["current_bytes"] and not m["current_items"] else None) if (m := self.metrics()) else None, 15)
        self.record("metrics_after", after)
        self.record("metrics_delta", metrics_delta(before, after))
        require(after["parser_ack_tickets"] > before["parser_ack_tickets"], "no parser-ACK samples observed")
        for index in (0, 4, 8):
            self.select(ids[index])
            self.record("pane_screenshot", {"session_id": ids[index], "screenshot": self.screenshot("pane-" + str(index + 1)), **self.visible()})
        self.js("document.querySelector('#context-rooms-chip').click()")
        self.record("rooms_screenshot", self.screenshot("two-rooms"))
        self.result["assertions_passed"] = True

    def stall(self):
        self.create((1,))
        sid = next(iter(self.panes))
        before = self.metrics()
        self.record("metrics_before", before)
        completion = Path(self.args.expected_work) / ("output-load-" + self.token + ".complete")
        require(not completion.exists(), "completion marker already exists")
        if self.args.stall_mode == "busy-loop":
            timer_client = CdpClient(self.ws_url)
            receipt = {"completion_file": str(completion), "native_completed_while_blocked": False}
            try:
                self.input(sid, output_script(self.token, 1, 2000, False, completion))
                busy_loop_command(timer_client, completion, self.token, receipt)
            finally:
                timer_client.close()
                self.record("busy_loop", receipt)
        else:
            self.debugger_stall(sid, completion)
        wait("public output gap after renderer resumes", lambda: self.observation()["panes"][sid]["gaps"] > 0, 20)
        self.state()
        after = self.metrics()
        delta = metrics_delta(before, after)
        self.record("metrics_after", after)
        self.record("metrics_delta", delta)
        require(delta["observer_entries"] > 0, "stall did not enter bounded observation mode")
        if self.js("document.querySelector('.app-shell').dataset.log") != "open":
            self.js("document.querySelector('#corner-menu [data-corner=log]').click()")
        wait("visible output gap notice", lambda: self.js("document.querySelector('#system-terminal').textContent.includes('UI output gap')"))
        self.record("stall_gap", {"observation": self.observation(), "screenshot": self.screenshot("stall-gap")})
        self.result["assertions_passed"] = True

    def debugger_stall(self, sid, completion):
        # A separate short-timeout CDP connection bounds pause/resume response
        # waiting. It never evaluates JS while paused or changes product APIs.
        timer_client = CdpClient(self.ws_url)
        paused_at, completed_at, resumed = None, None, False
        pause_receipts = []
        try:
            debugger_command(timer_client, "Debugger.enable", pause_receipts=pause_receipts)
            self.input(sid, output_script(self.token, 1, 2000, False, completion))
            paused_at = time.monotonic()
            debugger_command(timer_client, "Debugger.pause", pause_receipts=pause_receipts)
            until = paused_at + 5.5
            while time.monotonic() < until:
                if completion.exists() and completion.stat().st_size <= 64 and completion.read_text(encoding="utf-8-sig") == self.token:
                    completed_at = time.monotonic()
                time.sleep(.1)
        finally:
            # Always attempt resume, including a pause timeout whose command may
            # have succeeded. No JS, filesystem scan, or screenshot precedes it.
            resume_error = None
            try:
                if paused_at is not None:
                    resumed = debugger_command(timer_client, "Debugger.resume", pause_receipts=pause_receipts)
            except Exception as error:
                resume_error = repr(error)
            finally:
                elapsed = None if paused_at is None else time.monotonic() - paused_at
                try:
                    debugger_command(timer_client, "Debugger.disable", pause_receipts=pause_receipts)
                except Exception as error:
                    resume_error = resume_error or repr(error)
                finally:
                    timer_client.close()
                self.record("pause_resume", {"resume_acknowledged": resumed, "seconds": elapsed,
                    "native_completed_while_paused": completed_at is not None,
                    "completion_file": str(completion), "error": resume_error,
                    "debugger_pauses": pause_receipts})
            require(resume_error is None, "debugger cleanup failed: " + str(resume_error))
        require(resumed and elapsed is not None and elapsed < 8, "renderer resume was not acknowledged within eight seconds")
        require(completed_at is not None, "native finite output did not complete while renderer was paused")

    def cleanup(self):
        if self.client:
            for name, action in (
                ("final_metrics", self.metrics),
                ("final_observation", lambda: self.js("window.__outputLoadWalk?.snapshot()??null")),
                ("unlisten", lambda: self.js("(async()=>{await window.__outputLoadWalk?.unlisten?.();delete window.__outputLoadWalk;if(window.__busyLoopReceipt?.token===" + json.dumps(self.token) + ")delete window.__busyLoopReceipt;return true})()")),
            ):
                try:
                    self.record(name, action())
                except Exception as error:
                    self.result.setdefault("cleanup_errors", []).append({"action": name, "error": repr(error)})
            self.client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--phase", choices=("load", "stall"), required=True, help="each phase requires its own empty scratch preview")
    parser.add_argument("--paced-load", action="store_true", help="optional sustained stream (100 ms per 64 lines); default load is an unpaced finite burst")
    parser.add_argument("--stall-mode", choices=("debugger", "busy-loop"), default="debugger", help="stall only: original debugger pause, or one self-terminating six-second JS loop")
    parser.add_argument("--expected-runtime", required=True)
    parser.add_argument("--expected-work", required=True)
    parser.add_argument("--expected-exe", required=True, help="caller-attested running production executable")
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--out-parent", type=Path, required=True)
    args = parser.parse_args()
    if args.paced_load and args.phase != "load":
        parser.error("--paced-load applies only to --phase load")
    if args.stall_mode != "debugger" and args.phase != "stall":
        parser.error("--stall-mode busy-loop applies only to --phase stall")
    out = args.out_parent / ("output-load-" + args.phase + "-" + datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S%fZ"))
    out.mkdir()
    result = {"phase": args.phase, "steps": [], "acceptance_claim": "none",
              "stall_mode": args.stall_mode if args.phase == "stall" else None,
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "printable_bytes_per_pane": LINES * LINE_BYTES,
              "layout": "Nine active parsers in two rooms (5+4), with one selected terminal visible at a time; no nine-visible-pane or 3x3 layout/paint claim." if args.phase == "load" else "One Generic Terminal with its existing single-pane UI.",
              "limits": "Public event END/marker coverage is not a paint measurement. ACK timing is native dispatch to exact parser-ACK validation, ticket-weighted; current/peak bytes include recovery allowance, not heap/RSS. Root must attest the running frozen exe and inspect screenshots. No installed app, harness, app lifecycle, or private hooks."}
    with (out / "steps.jsonl").open("x", encoding="utf-8") as journal:
        walk = Walk(args, out, result, journal)
        try:
            walk.connect()
            getattr(walk, args.phase)()
        except Exception:
            result["error"] = traceback.format_exc()
        finally:
            walk.cleanup()
            result["finished_utc"] = datetime.now(timezone.utc).isoformat()
            with (out / "result.json").open("x", encoding="utf-8") as stream:
                json.dump(result, stream, indent=2)
            print(out)
    return 0 if result.get("assertions_passed") and not result.get("cleanup_errors") else 1


if __name__ == "__main__":
    raise SystemExit(main())
