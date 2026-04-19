#!/usr/bin/env python3
"""Chrome DevTools Protocol helper for the PRIM-1 Tauri wrapper.

Usage:
    python tools/cdp/cdp.py eval '<javascript-expression>'
    python tools/cdp/cdp.py click '<css-selector>'
    python tools/cdp/cdp.py info '<css-selector>'
    python tools/cdp/cdp.py html '<css-selector>'
    python tools/cdp/cdp.py listeners '<css-selector>'
    python tools/cdp/cdp.py console-dump
    python tools/cdp/cdp.py targets

Requires wrapper launched with PRIM1_CDP_PORT=9222 env var.
"""

from __future__ import annotations

import argparse
import json
import sys
import time
import urllib.request
from typing import Any

import websocket  # type: ignore


DEFAULT_PORT = 9222


def list_targets(port: int) -> list[dict[str, Any]]:
    url = f"http://localhost:{port}/json"
    with urllib.request.urlopen(url, timeout=5) as response:
        data = response.read().decode("utf-8")
    return json.loads(data)


def pick_target(targets: list[dict[str, Any]]) -> dict[str, Any]:
    pages = [t for t in targets if t.get("type") == "page"]
    if not pages:
        raise RuntimeError("No 'page' targets found on CDP port.")
    return pages[0]


class CdpClient:
    def __init__(self, ws_url: str) -> None:
        self.ws = websocket.create_connection(ws_url, timeout=10, suppress_origin=True)
        self._next_id = 1

    def close(self) -> None:
        try:
            self.ws.close()
        except Exception:
            pass

    def send(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        message_id = self._next_id
        self._next_id += 1
        payload = {"id": message_id, "method": method}
        if params is not None:
            payload["params"] = params
        self.ws.send(json.dumps(payload))
        deadline = time.time() + 10
        while time.time() < deadline:
            raw = self.ws.recv()
            message = json.loads(raw)
            if message.get("id") == message_id:
                return message
        raise RuntimeError(f"CDP timeout waiting for {method}")

    def evaluate(self, expression: str, await_promise: bool = True, return_by_value: bool = True) -> Any:
        result = self.send(
            "Runtime.evaluate",
            {
                "expression": expression,
                "awaitPromise": await_promise,
                "returnByValue": return_by_value,
                "allowUnsafeEvalBlockedByCSP": True,
            },
        )
        if "error" in result:
            raise RuntimeError(f"CDP error: {result['error']}")
        details = result.get("result", {})
        if "exceptionDetails" in details:
            ex = details["exceptionDetails"]
            raise RuntimeError(f"JS exception: {ex.get('text')} -- {ex.get('exception', {}).get('description')}")
        return details.get("result", {}).get("value")


def connect(port: int) -> CdpClient:
    targets = list_targets(port)
    target = pick_target(targets)
    ws_url = target["webSocketDebuggerUrl"]
    return CdpClient(ws_url)


def cmd_targets(port: int) -> int:
    for target in list_targets(port):
        print(f"{target.get('type'):>10}  {target.get('title')}  {target.get('url')}")
        print(f"{'':>10}  ws: {target.get('webSocketDebuggerUrl')}")
    return 0


def cmd_eval(port: int, expression: str) -> int:
    client = connect(port)
    try:
        value = client.evaluate(expression)
        print(json.dumps(value, indent=2, default=str))
    finally:
        client.close()
    return 0


def cmd_info(port: int, selector: str) -> int:
    script = f"""
(() => {{
    const el = document.querySelector({json.dumps(selector)});
    if (!el) return {{ found: false }};
    const rect = el.getBoundingClientRect();
    return {{
        found: true,
        tag: el.tagName,
        id: el.id,
        classes: Array.from(el.classList),
        text: (el.textContent || '').slice(0, 200),
        disabled: el.disabled ?? null,
        hidden: el.hidden ?? null,
        dataset: Object.assign({{}}, el.dataset),
        rect: {{ x: rect.x, y: rect.y, width: rect.width, height: rect.height }},
        visible: rect.width > 0 && rect.height > 0 && getComputedStyle(el).visibility !== 'hidden' && getComputedStyle(el).display !== 'none',
        pointerEvents: getComputedStyle(el).pointerEvents,
        zIndex: getComputedStyle(el).zIndex,
    }};
}})()
"""
    return cmd_eval(port, script.strip())


def cmd_html(port: int, selector: str) -> int:
    script = f"""
(() => {{
    const el = document.querySelector({json.dumps(selector)});
    return el ? el.outerHTML.slice(0, 2000) : null;
}})()
"""
    return cmd_eval(port, script.strip())


def cmd_click(port: int, selector: str) -> int:
    script = f"""
(() => {{
    const el = document.querySelector({json.dumps(selector)});
    if (!el) return {{ clicked: false, error: 'element not found' }};
    const rect = el.getBoundingClientRect();
    const ev = new MouseEvent('click', {{
        bubbles: true, cancelable: true, view: window,
        clientX: rect.x + rect.width / 2,
        clientY: rect.y + rect.height / 2,
    }});
    const dispatched = el.dispatchEvent(ev);
    return {{ clicked: true, dispatched, text: (el.textContent || '').slice(0, 80) }};
}})()
"""
    return cmd_eval(port, script.strip())


def cmd_listeners(port: int, selector: str) -> int:
    """Inspect event listeners on an element's ancestors up to document.

    Note: getEventListeners() is a DevTools-only helper. We approximate by
    dispatching a synthetic event and checking if it's caught anywhere.
    """
    script = f"""
(() => {{
    const el = document.querySelector({json.dumps(selector)});
    if (!el) return {{ found: false }};

    // Walk up the chain and attach a temporary capture listener to each to see
    // which ancestor the click bubbles through.
    const trace = [];
    const handler = function(ev) {{
        trace.push({{
            tag: this === document ? 'document' : (this.tagName + (this.id ? '#'+this.id : '') + (this.className ? '.'+this.className.split(' ').join('.') : '')),
            phase: ev.eventPhase,
            defaultPrevented: ev.defaultPrevented,
        }});
    }};
    const nodes = [el];
    let cur = el.parentNode;
    while (cur) {{ nodes.push(cur); cur = cur.parentNode; }}
    nodes.forEach(n => n.addEventListener('click', handler, true));
    const rect = el.getBoundingClientRect();
    const ev = new MouseEvent('click', {{ bubbles: true, cancelable: true, view: window, clientX: rect.x + rect.width/2, clientY: rect.y + rect.height/2 }});
    el.dispatchEvent(ev);
    nodes.forEach(n => n.removeEventListener('click', handler, true));
    return {{ found: true, trace, total: trace.length }};
}})()
"""
    return cmd_eval(port, script.strip())


def cmd_console_dump(port: int) -> int:
    """Enable Runtime + Log domains and dump buffered messages for ~2s."""
    targets = list_targets(port)
    target = pick_target(targets)
    client = CdpClient(target["webSocketDebuggerUrl"])
    try:
        client.send("Runtime.enable")
        client.send("Log.enable")
        client.send("Console.enable")
        deadline = time.time() + 2.5
        while time.time() < deadline:
            try:
                client.ws.settimeout(0.5)
                raw = client.ws.recv()
            except websocket.WebSocketTimeoutException:
                continue
            except Exception:
                break
            msg = json.loads(raw)
            method = msg.get("method")
            if method in ("Runtime.consoleAPICalled", "Log.entryAdded", "Console.messageAdded", "Runtime.exceptionThrown"):
                print(json.dumps({"method": method, "params": msg.get("params")}, indent=2, default=str))
    finally:
        client.close()
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description="PRIM-1 CDP helper.")
    parser.add_argument("--port", type=int, default=DEFAULT_PORT)
    subparsers = parser.add_subparsers(dest="command", required=True)

    subparsers.add_parser("targets")

    eval_p = subparsers.add_parser("eval")
    eval_p.add_argument("expression")

    info_p = subparsers.add_parser("info")
    info_p.add_argument("selector")

    html_p = subparsers.add_parser("html")
    html_p.add_argument("selector")

    click_p = subparsers.add_parser("click")
    click_p.add_argument("selector")

    listeners_p = subparsers.add_parser("listeners")
    listeners_p.add_argument("selector")

    subparsers.add_parser("console-dump")

    args = parser.parse_args()

    try:
        if args.command == "targets":
            return cmd_targets(args.port)
        if args.command == "eval":
            return cmd_eval(args.port, args.expression)
        if args.command == "info":
            return cmd_info(args.port, args.selector)
        if args.command == "html":
            return cmd_html(args.port, args.selector)
        if args.command == "click":
            return cmd_click(args.port, args.selector)
        if args.command == "listeners":
            return cmd_listeners(args.port, args.selector)
        if args.command == "console-dump":
            return cmd_console_dump(args.port)
    except Exception as exc:
        print(f"ERROR: {exc}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
