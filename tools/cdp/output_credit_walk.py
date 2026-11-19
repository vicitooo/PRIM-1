"""Bounded delayed-ACK/contrast walk on a ROOT-LAUNCHED production preview.

Port 9333 only; fresh empty scratch; Generic Terminal only. No app lifecycle,
private xterm access, native process access, ledger-cap or paint/perf claim.
The root must attest the running process against the supplied frozen manifest.
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


def require(value, message):
    if not value:
        raise RuntimeError(message)


def normalized(value):
    value = str(value).removeprefix("\\\\?\\")
    require(re.match(r"^[A-Za-z]:[\\/]", value), "absolute local DOS path required")
    require(not any(p in (".", "..") for p in re.split(r"[\\/]", value)), "ambiguous path")
    return ntpath.normcase(ntpath.normpath(value))


def wait(label, check, seconds=15):
    deadline = time.monotonic() + seconds
    while True:
        value = check()
        if value:
            return value
        require(time.monotonic() < deadline, "timeout: " + label)
        time.sleep(.2)


class Walk:
    def __init__(self, args, out, result, journal):
        self.args, self.out, self.result, self.journal = args, out, result, journal
        self.client, self.saved, self.room_id = None, None, None
        self.ids, self.bindings = [], {}
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
        require(all(s["driver"] == "generic_terminal" for s in state["sessions"]), "non-Terminal pane refused")
        require({s["session_id"] for s in state["sessions"]} == set(self.ids), "fixture session set changed")
        require({r["room_id"] for r in state["rooms"]} == ({self.room_id} if self.room_id else set()), "fixture room set changed")
        return state

    def session(self, sid, active=True):
        snapshot = next(s for s in self.state()["sessions"] if s["session_id"] == sid)
        if active:
            require(snapshot["running"] and [snapshot["run_id"], snapshot["generation"]] == self.bindings[sid], "fixture run changed")
        return snapshot

    def screenshot(self, name):
        response = self.client.send("Page.captureScreenshot", {"format": "png"})
        require("error" not in response, "screenshot failed")
        with (self.out / (name + ".png")).open("xb") as stream:
            stream.write(base64.b64decode(response["result"]["data"]))
        return name + ".png"

    def theme(self, name):
        self.state()
        self.js("document.querySelector('#corner-menu [data-corner=themes]').click()" if self.js("document.querySelector('.app-shell').dataset.view") != "themes" else "true")
        self.js("document.querySelector('[data-theme-pick=" + name + "]').click()")
        require(self.js("document.documentElement.dataset.theme") == name, "theme gallery did not apply")

    def select(self, sid):
        self.state()
        self.js("document.querySelector('#context-rooms-chip').click()")
        self.js("(() => {const cards=[...document.querySelectorAll('.rooms-overview-card:not(.rooms-overview-new)')].filter(e=>e.querySelector('h3')?.textContent===" + json.dumps(self.result["room_label"]) + ");if(cards.length!==1)throw Error('fixture room missing');cards[0].click()})()")
        wait("fixture tab", lambda: self.js("Boolean(document.querySelector('[data-session-tab=\"" + sid + "\"]'))"))
        self.js("document.querySelector('[data-session-tab=\"" + sid + "\"]').click()")
        wait("visible selected terminal", lambda: self.js("(() => {const c=document.querySelector('[data-session-card=\"" + sid + "\"]');return c&&!c.hidden&&!c.inert&&c.getClientRects().length&&document.querySelector('[data-session-tab=\"" + sid + "\"]').getAttribute('aria-selected')==='true'})()"))

    def text(self, sid):
        return self.js("[...document.querySelectorAll('[data-terminal=\"" + sid + "\"] .xterm-rows')].map(e=>e.textContent).join('\\n')")

    def input(self, sid, script):
        self.session(sid)
        self.invoke("send_input", {"request": {"session_id": sid, "input": script + "\r"}})

    def create(self, label):
        self.state()
        snapshot = self.invoke("create_session", {"request": {"label": label, "driver": "generic_terminal", "permission_profile": "normal", "room_id": self.room_id}})
        sid = snapshot["session_id"]
        self.ids.append(sid)
        self.select(sid)
        started = self.invoke("start_session", {"request": {"session_id": sid}})
        require(started["running"] and started["run_id"], "Terminal did not start")
        self.bindings[sid] = [started["run_id"], started["generation"]]
        self.record("started", started)
        return sid

    def observe(self, sid, phase):
        self.session(sid)
        self.js("window.__creditWalk?.unlisten?.()")
        self.js("""(async()=>{
          const w=window.__creditWalk={sid:SID,run:RUN,prefix:PREFIX,events:0,chars:0,max:0,end:false,carry:'',lastSequence:null,epochs:[],lifecycle:[],gaps:[]};
          w.unlisten=await window.__TAURI__.event.listen('runtime://event',({payload:p})=>{
            if(p.event==='ui_output_gap'){if(w.gaps.length<16)w.gaps.push(p);return}
            if(p.identity?.session_id!==w.sid)return;
            if(p.event!=='session_output'){
              if(['session_exit','session_state','session_output_draining'].includes(p.event)&&w.lifecycle.length<64)w.lifecycle.push(p);
              return;
            }
            if(p.identity.run_id!==w.run[0]||p.identity.generation!==w.run[1]){w.wrongRun=true;return}
            w.events++;w.chars+=p.chunk.length;w.lastSequence=p.identity.sequence;
            if(p.output_credit&&!w.epochs.includes(p.output_credit.epoch)&&w.epochs.length<8)w.epochs.push(p.output_credit.epoch);
            const text=w.carry+p.chunk;w.carry=text.slice(-256);
            for(const m of text.matchAll(new RegExp(w.prefix+'([0-9]{4}|END)','g'))){if(m[1]==='END')w.end=true;else w.max=Math.max(w.max,Number(m[1]))}
          });return true;
        })()""".replace("SID", json.dumps(sid)).replace("RUN", json.dumps(self.bindings[sid])).replace("PREFIX", json.dumps("OC_" + self.token + "_" + phase + "_")))

    def observation(self):
        return self.js("(()=>{const {unlisten,carry,...data}=window.__creditWalk;return data})()")

    def hook(self, sid):
        self.session(sid)
        return self.js("""(()=>{
          if(window.__creditHold)throw Error('fixture hook already installed');
          const owner=window.__TAURI_INTERNALS__,descriptor=Object.getOwnPropertyDescriptor(owner,'invoke');
          if(!descriptor||!('value' in descriptor)||descriptor.writable!==true||typeof descriptor.value!=='function')
            return {installed:false,reason:'Tauri invoke seam is read-only or unavailable; delayed-ACK checks skipped'};
          const original=owner.invoke;
          const h={owner,original,held:[],overflows:0,released:0,errors:[],holding:true};
          h.wrapper=function(cmd,args,...rest){
            const c=args?.credit;
            if(cmd==='acknowledge_output'&&h.holding&&c?.run.session_id===SID&&c.run.run_id===RUN[0]&&c.run.generation===RUN[1]){
              if(h.held.length>=256){h.overflows++;return Reflect.apply(original,this,[cmd,args,...rest])}
              return new Promise((resolve,reject)=>h.held.push({receiver:this,call:[cmd,args,...rest],resolve,reject}));
            }
            return Reflect.apply(original,this,[cmd,args,...rest]);
          };
          h.release=async()=>{
            h.holding=false;
            const pending=h.held.splice(0);
            await Promise.all(pending.map(async q=>{try{const value=await Reflect.apply(original,q.receiver,q.call);h.released++;q.resolve(value)}catch(e){h.errors.push(String(e));q.reject(e)}}));
            return {released:h.released,overflows:h.overflows,errors:h.errors};
          };
          h.restore=()=>{if(owner.invoke!==h.wrapper)throw Error('invoke seam changed externally');owner.invoke=original};
          window.__creditHold=h;
          owner.invoke=h.wrapper;
          if(owner.invoke!==h.wrapper){
            if(owner.invoke===original&&h.held.length===0){delete window.__creditHold;return {installed:false,reason:'Tauri invoke seam refused assignment; delayed-ACK checks skipped'}}
            throw Error('invoke seam changed unexpectedly');
          }
          return {installed:true};
        })()""".replace("SID", json.dumps(sid)).replace("RUN", json.dumps(self.bindings[sid])))

    def release(self):
        data = self.js("window.__creditHold.release()")
        self.record("ack_release", data)
        require(not data["overflows"] and not data["errors"], "ACK fixture overflow or settlement error")

    def unhook(self):
        self.release()
        self.js("window.__creditHold.restore();delete window.__creditHold")

    def burst(self, sid, phase, lines=1024, delay=0):
        # The complete marker never appears in the command echo.
        script = "$ocPrefix='OC_'+'" + self.token + "'+'_'+'" + phase + "'+'_';1.." + str(lines) + "|ForEach-Object{[Console]::WriteLine($ocPrefix+('{0:D4}' -f $_)+'|'+('x'*512));"
        if delay:
            script += "Start-Sleep -Milliseconds " + str(delay) + ";"
        script += "};[Console]::WriteLine($ocPrefix+'END')"
        self.input(sid, script)

    def plateau(self, name):
        wait("numbered output and held ACK", lambda: self.observation()["max"] > 0 and self.js("window.__creditHold.held.length>0"))
        deadline, unchanged, previous = time.monotonic() + 15, None, None
        while time.monotonic() < deadline:
            data = self.observation()
            require(not data["end"] and not data.get("wrongRun") and not data["gaps"], "invalid delayed-ACK plateau")
            key = [data["events"], data["max"], data["lastSequence"]]
            if key != previous:
                previous, unchanged = key, time.monotonic()
            if time.monotonic() - unchanged >= 2:
                data["held_calls"] = self.js("window.__creditHold.held.length")
                require(data["held_calls"] > 0, "plateau without held acknowledgements")
                data["screenshot"] = self.screenshot(name)
                self.record(name, data)
                return data
            time.sleep(.25)
        raise RuntimeError("no 2-second output plateau under delayed ACK")

    def run(self):
        manifest = json.loads(self.args.build_manifest.read_text(encoding="utf-8-sig"))
        require(re.fullmatch(r"[0-9a-fA-F]{40}", manifest["source_commit"]), "manifest needs full source SHA")
        require(manifest.get("app_identifier") == "io.prim1.v2", "manifest must identify the isolated v2 app")
        exe = manifest.get("frozen_exe", manifest.get("exe_path"))
        digest = manifest.get("sha256", manifest.get("exe_sha256"))
        require(exe and digest and hashlib.sha256(Path(exe).read_bytes()).hexdigest().lower() == digest.lower(), "frozen executable hash differs")
        self.result["manifest"] = manifest
        targets = [t for t in list_targets(9333) if t.get("type") == "page"]
        require(len(targets) == 1, "ambiguous preview page")
        url = urlparse(targets[0]["webSocketDebuggerUrl"])
        require(url.hostname in ("localhost", "127.0.0.1", "::1") and url.port == 9333, "wrong preview endpoint")
        self.client = CdpClient(targets[0]["webSocketDebuggerUrl"])
        self.record("initial_empty_scratch", self.state())
        self.saved = self.js("({theme:document.documentElement.dataset.theme,log:document.querySelector('.app-shell').dataset.log,unicolor:document.querySelector('#unicolor-toggle').checked,preferences:Object.fromEntries(['prim1-theme','prim1-theme-chosen','prim1-unicolor','prim1-work-context'].map(k=>[k,localStorage.getItem(k)]))})")
        self.result["room_label"] = "Output credit " + self.token
        self.room_id = self.invoke("create_room", {"request": {"label": self.result["room_label"], "member_ids": [], "brief_on_join": False}})["room_id"]
        a = self.create("Output and Stop")
        self.observe(a, "release")
        hook = self.hook(a)
        hooked = hook["installed"]
        self.result["delayed_ack"] = {"status": "RUNNING" if hooked else "SKIPPED", **hook}
        if not hooked:
            self.result["skipped_checks"] = ["output plateau/recovery under held ACK", "bounded Stop while ACK is held"]
        self.record("delayed-ack-availability", self.result["delayed_ack"])
        self.burst(a, "release")
        if hooked:
            before = self.plateau("ack-held")
            self.release()
        wait("finite output completes", lambda: self.observation()["end"], 30)
        resumed = self.observation()
        require(not resumed["gaps"] and not resumed.get("wrongRun"), "invalid finite output stream")
        if hooked:
            require(resumed["max"] > before["max"], "output did not resume")
        self.record("ack-resumed" if hooked else "normal-output-complete", resumed)

        self.observe(a, "stop")
        if hooked:
            self.js("window.__creditHold.holding=true")
            self.burst(a, "stop")
            self.plateau("stop-ack-held")
        else:
            self.burst(a, "stop", lines=100, delay=200)
            wait("active normal output before Stop", lambda: self.observation()["max"] >= 3)
            active = self.observation()
            require(not active["end"], "normal Stop missed the active-output window")
            self.record("normal-output-before-stop", active)
        self.session(a)
        start = time.monotonic()
        stopped = self.invoke("stop_session", {"request": {"session_id": a}})
        elapsed = time.monotonic() - start
        self.record("stop_return", {"delayed_ack": hooked, "seconds_observed": elapsed, "snapshot": stopped, "events": self.observation()})
        if hooked:
            require(elapsed < 8, "Stop exceeded fixture's 8-second bound")
        require(not stopped["running"] and stopped["run_id"] is None and stopped["last_launch"]["run_id"] == self.bindings[a][0], "exact fixture run was not retired")
        if hooked:
            self.unhook()
            self.result["delayed_ack"]["status"] = "ASSERTIONS_PASSED"

        self.theme("light")
        if self.js("document.querySelector('#unicolor-toggle').checked"):
            self.js("document.querySelector('#unicolor-toggle').click()")
        b = self.create("Light and reload")
        colour = "OC_" + self.token + "_contrast"
        self.input(b, "$ocLabel='OC_'+'" + self.token + "'+'_contrast';[Console]::WriteLine([char]27+'[37m'+$ocLabel+' SGR37'+[char]27+'[0m');[Console]::WriteLine([char]27+'[97m'+$ocLabel+' SGR97'+[char]27+'[0m');[Console]::WriteLine([char]27+'[38;2;255;255;255m'+$ocLabel+' RGB255'+[char]27+'[0m')")
        wait("contrast text reaches rendered DOM", lambda: colour + " RGB255" in self.text(b))
        for theme in ("light", "dark", "prim1", "prim1-deep", "light"):
            self.theme(theme)
            self.select(b)
            time.sleep(.3)
            self.record("contrast-" + theme, {"text": self.text(b), "screenshot": self.screenshot("contrast-" + theme + "-" + str(len(self.result["steps"])))})

        self.observe(b, "reload")
        self.burst(b, "reload", lines=100, delay=200)
        wait("active stream before reload", lambda: self.observation()["max"] >= 3)
        before_reload = self.observation()
        self.record("before-reload", before_reload)
        self.js("window.__creditOldDocument=true")
        response = self.client.send("Page.reload", {"ignoreCache": True})
        require("error" not in response, "reload refused")
        def ready():
            try:
                return self.js("Boolean(!window.__creditOldDocument&&window.__TAURI__?.core?.invoke&&document.querySelector('#context-rooms-chip'))")
            except Exception:
                return False
        wait("new renderer", ready)
        self.session(b)
        self.observe(b, "reload")
        wait("new same-run output after reload", lambda: self.observation()["max"] > before_reload["max"])
        require(set(self.observation()["epochs"]) - set(before_reload["epochs"]), "reload did not expose a new output epoch")
        if self.js("document.querySelector('.app-shell').dataset.log") != "open":
            self.js("document.querySelector('#corner-menu [data-corner=log]').click()")
        wait("explicit reset warning", lambda: self.js("document.querySelector('#system-terminal').textContent.includes('Previous terminal scrollback is lost')"))
        self.record("reload-warning", {"observation": self.observation(), "log": self.js("document.querySelector('#system-terminal').textContent"), "screenshot": self.screenshot("reload-warning")})
        if self.js("document.querySelector('.app-shell').dataset.log") == "open":
            self.js("document.querySelector('#corner-menu [data-corner=log]').click()")
        self.select(b)
        wait("finite reload stream completes", lambda: self.observation()["end"], 30)
        self.record("reload-complete", {"session": self.session(b), "observation": self.observation()})

        self.observe(b, "tail")
        tail = "OC_" + self.token + "_tail_END"
        self.input(b, "$ocTail='OC_'+'" + self.token + "'+'_tail_'+'END';1..20|ForEach-Object{[Console]::WriteLine('finite tail line '+$_)};[Console]::WriteLine($ocTail);Start-Sleep -Seconds 4;exit")
        wait("natural-exit tail reaches rendered DOM", lambda: tail in self.text(b))
        tail_snapshot = self.session(b)
        require(not tail_snapshot["output_draining"], "tail observation missed the pre-exit window")
        self.record("tail-before-exit", {"session": tail_snapshot, "text": self.text(b), "screenshot": self.screenshot("tail-before-exit")})
        ended = wait("natural exit retirement", lambda: (s if not s["running"] else None) if (s := self.session(b, active=False)) else None, 15)
        require(ended["run_id"] is None and ended["last_launch"]["run_id"] == self.bindings[b][0], "natural exit did not retain exact launch record")
        wait("natural exit event", lambda: any(e["event"] == "session_exit" and e["identity"]["run_id"] == self.bindings[b][0] for e in self.observation()["lifecycle"]))
        require(tail in self.text(b), "natural exit erased the visible tail")
        self.record("tail-after-exit", {"session": ended, "events": self.observation(), "text": self.text(b), "screenshot": self.screenshot("tail-after-exit")})
        self.result["assertions_passed"] = True

    def cleanup(self):
        if not self.client or not self.saved:
            return
        actions = [
            ("settle-and-restore-IPC", lambda: self.js("""(async()=>{
              const h=window.__creditHold;if(!h)return null;
              if(typeof h.release!=='function'||typeof h.restore!=='function'){
                if(h.held?.length===0&&h.owner?.invoke===h.original){delete window.__creditHold;return {empty_partial_removed:true}}
                throw Error('incomplete fixture state cannot be safely discarded');
              }
              try{return await h.release()}finally{h.restore();delete window.__creditHold}
            })()""")),
            ("unlisten", lambda: self.js("window.__creditWalk?.unlisten?.()")),
            ("theme", lambda: self.theme(self.saved["theme"])),
            ("unicolor", lambda: self.js("if(document.querySelector('#unicolor-toggle').checked!==" + json.dumps(self.saved["unicolor"]) + ")document.querySelector('#unicolor-toggle').click()")),
            ("log", lambda: self.js("if(document.querySelector('.app-shell').dataset.log!==" + json.dumps(self.saved["log"]) + ")document.querySelector('#corner-menu [data-corner=log]').click()")),
            ("preferences", lambda: self.js("for(const[k,v]of Object.entries(" + json.dumps(self.saved["preferences"]) + ")){if(v===null)localStorage.removeItem(k);else localStorage.setItem(k,v)}")),
        ]
        for name, action in actions:
            try:
                self.record("cleanup-" + name, action())
            except Exception as error:
                self.result.setdefault("cleanup_errors", []).append({"action": name, "error": repr(error)})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-runtime", required=True)
    parser.add_argument("--build-manifest", type=Path, required=True)
    parser.add_argument("--out-parent", type=Path, required=True)
    args = parser.parse_args()
    out = args.out_parent / ("output-credit-" + datetime.now(timezone.utc).strftime("%Y%m%d-%H%M%S%fZ"))
    out.mkdir()
    result = {"steps": [], "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "acceptance_claim": "none", "limits": "Delayed-ACK checks explicitly SKIPPED when the Tauri IPC seam is read-only; normal output/Stop then replace them without a backpressure/bound claim. No slow-parser, paint/performance or ledger-cap measurement. Root must inspect screenshots and attest the running frozen executable."}
    with (out / "steps.jsonl").open("x", encoding="utf-8") as journal:
        walk = Walk(args, out, result, journal)
        try:
            walk.run()
        except Exception:
            result["error"] = traceback.format_exc()
        finally:
            walk.cleanup()
            if walk.client:
                walk.client.close()
            result["finished_utc"] = datetime.now(timezone.utc).isoformat()
            with (out / "result.json").open("x", encoding="utf-8") as stream:
                json.dump(result, stream, indent=2)
            print(out)
    return 0 if result.get("assertions_passed") and not result.get("cleanup_errors") else 1


if __name__ == "__main__":
    raise SystemExit(main())
