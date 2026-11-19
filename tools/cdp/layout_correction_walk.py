"""Inspect DP layout on an attested Generic-only preview at loopback port 9333.

Root must seed a real Send result with Details before this walk. This driver
never posts, sends, starts sessions, submits Create, or injects product state.
"""
import argparse
import base64
from datetime import datetime, timezone
import hashlib
import json
from pathlib import Path
import sys
import time
import traceback
from urllib.parse import urlparse

sys.path.insert(0, str(Path(__file__).resolve().parent))
from cdp import CdpClient, list_targets

THEMES = ("dark", "light", "prim1", "prim1-deep")
DISCLOSURES = ("#room-brief-settings", ".room-member-settings", "#room-status-details")
CONTROLS = ("#room-post", "#room-message", "#room-recipient", "#room-add-member",
            "#room-add-member-select", "#room-brief-text", "#room-brief-save",
            "#room-brief-default", "#room-brief-settings > summary",
            ".room-member-settings > summary", "#room-status-details > summary",
            "#room-send")  # Send last: the next screenshot shows its hit area.


def layout_expectations(theme, mode):
    redesigned = theme in ("dark", "light") and mode == "readable"
    disclosures = tuple(s for s in DISCLOSURES if redesigned or s != ".room-member-settings")
    controls = tuple(s for s in CONTROLS if redesigned or s != ".room-member-settings > summary")
    return disclosures, controls, 96 if redesigned else 0


def fixture_room(state, expected_runtime, label):
    normalized = lambda path: path.replace("\\", "/").lower().removeprefix("//?/").rstrip("/")
    assert normalized(state["runtime_dir"]) == normalized(expected_runtime), "runtime mismatch"
    assert state["sessions"] and all(s["driver"] == "generic_terminal" for s in state["sessions"]), "Generic-only fixture required"
    matches = [room for room in state["rooms"] if room["label"] == label]
    assert matches, "exact fixture room label not found"
    return matches[0]


# Measures real DOM only. Disabled controls are reported, not treated as an
# actionable target. Scrolling is allowed; it must make each enabled control
# reachable without the floating corner menu covering it.
GEOMETRY = """
const target=document.querySelector(SELECTOR), corner=document.querySelector('#corner-button');
const rect=e=>{const r=e.getBoundingClientRect();return {left:r.left,top:r.top,right:r.right,bottom:r.bottom,width:r.width,height:r.height}};
if(!target || target.closest('[hidden]')) return {available:false};
target.scrollIntoView({block:'center',inline:'nearest'});
const a=rect(target),b=rect(corner),cx=(a.left+a.right)/2,cy=(a.top+a.bottom)/2;
const overlap=a.left<b.right&&a.right>b.left&&a.top<b.bottom&&a.bottom>b.top;
const points=[[a.left+2,cy],[a.right-2,cy],[cx,a.top+2],[cx,a.bottom-2],[cx,cy]];
return {available:a.width>0&&a.height>0,disabled:target.disabled===true,
  target:a,corner:b,overlap,hits:points.map(([x,y])=>target.contains(document.elementFromPoint(x,y))),
  overflow:document.documentElement.scrollWidth>innerWidth+1};
"""


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--expected-runtime", required=True)
    parser.add_argument("--fixture-label", required=True, help="exact label; first matching room in catalog order")
    parser.add_argument("--out-parent", type=Path, required=True)
    parser.add_argument("--normal-width", type=int, default=1560)
    parser.add_argument("--normal-height", type=int, default=980)
    args = parser.parse_args()
    assert args.normal_width > 0 and args.normal_height > 0
    assert (args.normal_width, args.normal_height) != (1280, 780), "normal viewport must differ from the compact case"
    out = args.out_parent / ("layout-correction-" + datetime.now(timezone.utc).strftime("%H%M%S%fZ"))
    out.mkdir()
    result = {"started_utc": datetime.now(timezone.utc).isoformat(), "checks": [],
              "failures": [], "coverage_gaps": [], "acceptance_claim": "none",
              "driver_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
              "limits": "CDP viewport and DOM hit tests, not physical pointer input; screenshots require root review."}
    client = None
    saved = None
    metrics_changed = False
    walk_started = False
    try:
        pages = [target for target in list_targets(9333) if target.get("type") == "page"]
        assert len(pages) == 1, "ambiguous preview target"
        url = urlparse(pages[0]["webSocketDebuggerUrl"])
        assert url.hostname in ("localhost", "127.0.0.1", "::1") and url.port == 9333
        client = CdpClient(pages[0]["webSocketDebuggerUrl"])

        def js(body):
            return client.evaluate("(() => {" + body + "})()")

        def checked(method, parameters=None):
            response = client.send(method, parameters)
            if "error" in response:
                raise RuntimeError(str(response["error"]))
            return response

        def shot(name):
            data = checked("Page.captureScreenshot", {"format": "png"})["result"]["data"]
            with (out / (name + ".png")).open("xb") as stream:
                stream.write(base64.b64decode(data))

        def viewport(width, height):
            nonlocal metrics_changed
            metrics_changed = True  # finally clears even if command/verification fails.
            checked("Emulation.setDeviceMetricsOverride", {"width": width, "height": height, "deviceScaleFactor": 1, "mobile": False})
            assert js("return [innerWidth,innerHeight]") == [width, height], "viewport override not applied"

        def menu(view):
            if js("return document.querySelector('.app-shell').dataset.view") == view:
                return
            js("if(document.querySelector('#corner-menu').hidden)document.querySelector('#corner-button').click();"
               "document.querySelector('#corner-menu [data-corner=" + view + "]').click();")
            time.sleep(.15)

        def theme(value):
            menu("themes")
            js("document.querySelector('[data-theme-pick=" + value + "]').click();")
            assert js("return document.documentElement.dataset.theme") == value

        def context(index, label):
            js("document.querySelector('#context-rooms-chip').click();")
            js("const cards=[...document.querySelectorAll('.rooms-overview-card:not(.rooms-overview-new)')];"
               "const card=cards[" + str(index) + "];if(!card||card.querySelector('h3')?.textContent!==" + json.dumps(label) +
               ")throw Error('context target changed');card.click();")
            time.sleep(.15)

        def room():
            js("document.querySelector('#context-name-chip').click();")
            time.sleep(.15)
            assert js("return document.querySelector('.app-shell').dataset.view") == "rooms"
            assert js("return document.querySelector('#room-panel-title').textContent.trim()") == selected["label"]

        def open_disclosures(selectors):
            return js("return " + json.dumps(selectors) + """.map(selector=>{
              const e=document.querySelector(selector);
              const available=!!e&&!e.hidden&&e.getClientRects().length>0;
              if(available&&!e.open)e.querySelector('summary').click();
              return {selector,available,open:available&&e.open};
            });""")

        def wait_brief():
            # Let the native details toggle event start its asynchronous load.
            js("return new Promise(resolve=>setTimeout(resolve,0));")
            deadline = time.monotonic() + 3
            while js("return document.querySelector('#room-brief-state')?.textContent==='Loading…'"):
                if time.monotonic() >= deadline:
                    raise RuntimeError("room brief is still loading; draft restoration cannot be verified")
                time.sleep(.05)

        def measure(selector):
            return js(GEOMETRY.replace("SELECTOR", json.dumps(selector)))

        def record_control(case, selector, geometry):
            result["checks"].append({**case, "control": selector, "geometry": geometry})
            if not geometry.get("available"):
                result["coverage_gaps"].append({**case, "control": selector, "reason": "control not available"})
            elif geometry.get("disabled"):
                if selector in ("#room-send", "#cancel-room-create"):
                    result["coverage_gaps"].append({**case, "control": selector, "reason": "required hit target disabled"})
            elif geometry["overlap"] or not all(geometry["hits"]) or geometry["overflow"]:
                result["failures"].append({**case, "control": selector, "reason": "unreachable, corner-covered, or horizontal overflow", "geometry": geometry})

        state = client.evaluate("window.__TAURI__.core.invoke('bootstrap')")
        selected = fixture_room(state, args.expected_runtime, args.fixture_label)
        assert client.evaluate("window.__TAURI__.core.invoke('trial_status')") is None, "normal preview required"
        result["runtime"] = state["runtime_dir"]
        result["room"] = {"id": selected["room_id"], "label": selected["label"], "total_rooms": len(state["rooms"])}
        result["run_bindings"] = {s["session_id"]: [s["run_id"], s["generation"]] for s in state["sessions"]}
        wait_brief()  # Capture settled editor contents before any navigation.
        saved = js("""const cards=[...document.querySelectorAll('.rooms-overview-card:not(.rooms-overview-new)')];
          const index=cards.findIndex(e=>e.dataset.current==='true');
          return {theme:document.documentElement.dataset.theme,
            preferences:Object.fromEntries(['prim1-theme','prim1-theme-chosen','prim1-room-feed-view','prim1-work-context'].map(k=>[k,localStorage.getItem(k)])),
            feed:document.querySelector('input[name=room-feed-view]:checked')?.value,
            view:document.querySelector('.app-shell').dataset.view, roomsMode:document.querySelector('.app-shell').dataset.roomsMode,
            cornerOpen:!document.querySelector('#corner-menu').hidden,
            contextIndex:index, contextLabel:cards[index]?.querySelector('h3')?.textContent,
            activeTab:document.querySelector('[data-session-tab][aria-selected=true]')?.dataset.sessionTab,
            createOpen:!document.querySelector('#room-create-form').hidden,
            drafts:Object.fromEntries(['room-message','room-recipient','room-label','room-brief-template','room-brief-text'].map(id=>[id,document.getElementById(id).value])),
            checks:Object.fromEntries(['room-brief-auto','room-brief-on-launch'].map(id=>[id,document.getElementById(id).checked])),
            members:[...document.querySelectorAll('#room-member-choices input:checked')].map(e=>e.value),
            details:Object.fromEntries(['#room-brief-settings','.room-member-settings','#room-status-details'].map(s=>[s,document.querySelector(s)?.open??false])),
            scrolls:[...document.querySelectorAll('[id],.room-member-settings')].map(e=>({id:e.id,top:e.scrollTop,left:e.scrollLeft}))};
        """)
        assert not saved["createOpen"], "existing create draft is open; close it before this walk"
        assert saved["contextIndex"] >= 0, "cannot safely restore current context"
        walk_started = True
        context(1 + state["rooms"].index(selected), selected["label"])
        assert js("return JSON.parse(localStorage.getItem('prim1-work-context')).roomId") == selected["room_id"], "fixture identity changed"
        room()
        result["status_seed"] = js("return {summary:document.querySelector('#room-status-summary').textContent, details:document.querySelector('#room-status-detail-text').textContent};")

        for value in THEMES:
            theme(value)
            room()
            for width, height in ((1280, 780), (args.normal_width, args.normal_height)):
                viewport(width, height)
                for mode in ("readable", "raw"):
                    case = {"theme": value, "width": width, "height": height, "mode": mode}
                    required_disclosures, required_controls, minimum_height = layout_expectations(value, mode)
                    js("document.querySelector('input[name=room-feed-view][value=" + mode + "]').click();")
                    disclosures = open_disclosures(required_disclosures)
                    wait_brief()
                    time.sleep(.2)
                    js("for(const e of document.querySelectorAll('#room-content,#room-members,#room-brief-settings,.room-member-settings,#room-status-detail-text')){e.scrollTop=0;e.scrollLeft=0;}")
                    floor = js("""const feed=document.querySelector('#room-feed'),r=feed.getBoundingClientRect();
                      return {height:r.height,client:feed.clientHeight,scroll:feed.scrollHeight,
                        top:r.top,bottom:r.bottom,viewportHeight:innerHeight,
                        overflow:document.documentElement.scrollWidth>innerWidth+1};""")
                    floor["minimum_border_box_height"] = minimum_height
                    result["checks"].append({**case, "disclosures": disclosures, "feed_floor": floor})
                    for item in disclosures:
                        if not item["available"] or not item["open"]:
                            result["coverage_gaps"].append({**case, "control": item["selector"], "reason": "real disclosure unavailable; no state injected"})
                    if floor["height"] <= 0 or floor["height"] < minimum_height or floor["client"] <= 0 or floor["overflow"]:
                        result["failures"].append({**case, "reason": "feed below required floor or horizontal overflow", "feed_floor": floor})
                    name = f"{value}-{width}x{height}-{mode}"
                    shot(name + "-expanded")
                    member_controls = js("return [...document.querySelectorAll('#room-members .room-member-chip')].flatMap((chip,c)=>[...chip.querySelectorAll('button')].map((button,b)=>'#room-members .room-member-chip:nth-child('+(c+1)+') button:nth-of-type('+(b+1)+')'));")
                    for selector in (*member_controls, *required_controls):
                        if selector == "#room-status-details > summary" and not disclosures[-1]["available"]:
                            continue
                        record_control(case, selector, measure(selector))
                    shot(name + "-send-hit")
                # Open the actual create form and cancel it, never submit it.
                case = {"theme": value, "width": width, "height": height, "view": "create-cancel"}
                js("document.querySelector('#new-room').click();")
                time.sleep(.2)
                record_control(case, "#cancel-room-create", measure("#cancel-room-create"))
                shot(f"{value}-{width}x{height}-create-cancel")
                js("document.querySelector('#cancel-room-create').click();")
                assert js("return document.querySelector('#room-create-form').hidden"), "Cancel did not close form"
        result["matrix_completed"] = True
    except Exception:
        result["error"] = traceback.format_exc()
        if client:
            try:
                shot("failure")
            except Exception as error:
                result["failure_screenshot_error"] = repr(error)
    finally:
        if client:
            def restore_ui():
                if not walk_started:
                    return
                js("if(!document.querySelector('#room-create-form').hidden)document.querySelector('#cancel-room-create').click();")
                theme(saved["theme"])
                context(saved["contextIndex"], saved["contextLabel"])
                if saved["contextIndex"] > 0:
                    # Rebind the brief editor before restoring unsaved values.
                    js("document.querySelector('#context-name-chip').click();")
                if saved["feed"]:
                    js("document.querySelector('input[name=room-feed-view][value=" + saved["feed"] + "]').click();")
                js("for(const [s,open] of Object.entries(" + json.dumps(saved["details"]) +
                   ")){const e=document.querySelector(s);if(e&&e.open!==open){if(e.hidden)e.open=open;else e.querySelector('summary').click();}}")
                # Selecting a tab switches to Sessions, so do it before the
                # final view restore rather than silently undoing that restore.
                if saved["activeTab"]:
                    js("document.querySelector('[data-session-tab='+CSS.escape(" + json.dumps(saved["activeTab"]) + ")+']')?.click();")
                else:
                    context(saved["contextIndex"], saved["contextLabel"])
                if saved["view"] == "rooms":
                    if saved["roomsMode"] == "panel":
                        js("document.querySelector('#context-name-chip').click();")
                    else:
                        js("document.querySelector('#context-rooms-chip').click();")
                elif saved["view"] != "sessions":
                    menu(saved["view"])
                # Finish navigation/load callbacks before restoring unsaved edits.
                wait_brief()
                js("for(const [id,value] of Object.entries(" + json.dumps(saved["drafts"]) + "))document.getElementById(id).value=value;"
                   "for(const [id,value] of Object.entries(" + json.dumps(saved["checks"]) + "))document.getElementById(id).checked=value;"
                   "for(const e of document.querySelectorAll('#room-member-choices input'))e.checked=" + json.dumps(saved["members"]) + ".includes(e.value);")
                js("if((!document.querySelector('#corner-menu').hidden)!==" + json.dumps(saved["cornerOpen"]) + ")document.querySelector('#corner-button').click();")
                js("for(const p of " + json.dumps(saved["scrolls"]) + "){const e=p.id?document.getElementById(p.id):document.querySelector('.room-member-settings');if(e){e.scrollTop=p.top;e.scrollLeft=p.left;}}")
                restored = js("""return {theme:document.documentElement.dataset.theme,
                  view:document.querySelector('.app-shell').dataset.view,
                  feed:document.querySelector('input[name=room-feed-view]:checked')?.value,
                  drafts:Object.fromEntries(['room-message','room-recipient','room-label','room-brief-template','room-brief-text'].map(id=>[id,document.getElementById(id).value])),
                  checks:Object.fromEntries(['room-brief-auto','room-brief-on-launch'].map(id=>[id,document.getElementById(id).checked])),
                  details:Object.fromEntries(['#room-brief-settings','.room-member-settings','#room-status-details'].map(s=>[s,document.querySelector(s)?.open??false]))};""")
                result["restored_ui"] = restored
                assert restored == {key: saved[key] for key in ("theme", "view", "feed", "drafts", "checks", "details")}, "UI/draft restoration did not match"
            for name, action in [
                # Restore real geometry before scroll offsets can be clamped.
                ("viewport", lambda: checked("Emulation.clearDeviceMetricsOverride") if metrics_changed else None),
                ("ui_and_drafts", restore_ui),
                ("preferences", lambda: js("for(const [k,v] of Object.entries(" + json.dumps(saved["preferences"]) + ")){if(v===null)localStorage.removeItem(k);else localStorage.setItem(k,v);}") if saved and walk_started else None),
            ]:
                try:
                    action()
                except Exception as error:
                    result.setdefault("cleanup_errors", []).append({"action": name, "error": repr(error)})
            client.close()
        result["finished_utc"] = datetime.now(timezone.utc).isoformat()
        with (out / "result.json").open("x", encoding="utf-8") as stream:
            json.dump(result, stream, indent=2)
        print(out)
    return 0 if result.get("matrix_completed") and not any(result.get(k) for k in ("failures", "coverage_gaps", "cleanup_errors")) else 1


if __name__ == "__main__":
    raise SystemExit(main())
