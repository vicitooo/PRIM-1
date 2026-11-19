"""Manual S8 data fixture, run INSIDE an isolated preview Generic Terminal pane.

Root first attests the preview runtime over CDP, then runs phases individually:
  python tools/cdp/typed_card_fixture.py initial --expected-exe <preview.exe>
    --expected-runtime <preview-runtime> --expected-room <room-uuid>
    --expected-label <fixture-label> --out-dir <absolute-evidence-directory>
Repeat with update, churn, read. Do not move the fixture member during a phase.
No desktop launch, direct pipe access, input, wake, lifecycle action or AI harness.
Only the inherited official CLI's --prim1-pane-mcp stdio helper is started.
"""
import argparse
import base64
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import queue
import struct
import subprocess
import sys
import threading
import uuid
import zlib

FRAME_LIMIT = 8 * 1024 * 1024
PREFIX = "s8qa-v1:"


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def absolute_path(value):
    path = Path(value)
    require(path.is_absolute(), "Expected paths must be absolute")
    return path.resolve()


class PaneMcp:
    def __init__(self, executable):
        self.process = subprocess.Popen([str(executable), "--prim1-pane-mcp"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW)
        self.lines = queue.Queue(maxsize=1)
        self.sequence = 0
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        while True:
            line = self.process.stdout.readline(FRAME_LIMIT + 1)
            self.lines.put(line)
            if not line or len(line) > FRAME_LIMIT or not line.endswith(b"\n"):
                return

    def send(self, method, params=None, notification=False):
        request = {"jsonrpc": "2.0", "method": method, "params": params or {}}
        if not notification:
            self.sequence += 1
            request["id"] = self.sequence
        self.process.stdin.write((json.dumps(request, separators=(",", ":")) + "\n").encode())
        self.process.stdin.flush()
        if notification:
            return None
        try:
            line = self.lines.get(timeout=30)
        except queue.Empty:
            raise RuntimeError("MCP response timed out") from None
        require(line and len(line) <= FRAME_LIMIT and line.endswith(b"\n"), "Invalid MCP frame")
        response = json.loads(line)
        require(response.get("id") == self.sequence and "error" not in response, "MCP request refused")
        return response["result"]

    def tool(self, name, arguments=None):
        require(name in ("ping", "room_read", "post_card", "room_post"), "Fixture tool is not allowed")
        result = self.send("tools/call", {"name": name, "arguments": arguments or {}})
        content = result.get("content", [])
        require(len(content) == 1 and content[0].get("type") == "text", "Expected full JSON text result")
        try:
            value = json.loads(content[0]["text"])
        except json.JSONDecodeError:
            raise RuntimeError("Fixture tool transport refused: " + name) from None
        if value.get("ok") is not True or result.get("isError"):
            # Classify only; do not echo arbitrary server/transport text.
            message = str(value.get("message", "")).lower()
            categories = [word for word in ("quota", "recovery", "history", "conflict", "stale", "member", "space") if word in message]
            raise RuntimeError("Fixture tool refused: " + name + " (" + ",".join(categories or ["other"]) + ")")
        if "structuredContent" in result:
            require(result["structuredContent"] == value, "MCP text and structured result disagree")
        return value

    def close(self):
        # Close/terminate only this script's stdio helper, never the supervisor.
        try:
            self.process.stdin.close()
        except BrokenPipeError:
            pass
        try:
            self.process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait(timeout=3)


def png_fixture():
    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))
    # A valid 2x2 RGBA PNG, including CRCs and compressed scanlines.
    pixels = b"\x00" + bytes([36, 107, 253, 255]) * 2
    png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 2, 2, 8, 6, 0, 0, 0))
    png += chunk(b"IDAT", zlib.compress(pixels * 2)) + chunk(b"IEND", b"")
    return base64.b64encode(png).decode("ascii")


def initial_cards():
    literal = '<script>alert("literal only")</script> <svg onload="alert(1)"> & https://example.invalid/no-fetch'
    cards = []
    for kind in ("decision", "progress", "result", "metric", "alert", "report", "image", "question"):
        cards.append({"key": "qa-" + kind, "kind": kind, "title": "Fixture " + kind,
            "summary": "S8 manual preview: " + kind, "body": "Select this text and leave Details open.\n" + literal,
            "state": "done" if kind == "result" else "open", "data": {}})
    cards[1]["data"] = {"done": 1, "total": 3, "unit": "checks"}
    cards[3]["data"] = {"value": 12.5, "unit": "ms", "series": [8, 13, 9, 15, 12.5]}
    cards[6]["data"] = {"png_base64": png_fixture(), "alt": "Two by two blue fixture PNG"}
    return cards


def run(args, record):
    require(os.name == "nt", "This fixture requires the Windows preview pane")
    executable = absolute_path(args.expected_exe)
    runtime = absolute_path(args.expected_runtime)
    require(executable.is_file() and executable.suffix.lower() == ".exe", "Preview executable is missing")
    require(runtime.is_dir(), "Expected preview runtime directory is missing")
    inherited_cli = os.environ.get("PRIM1_CLI")
    require(inherited_cli and absolute_path(inherited_cli) == executable, "Inherited CLI does not match the expected preview")
    inherited_runtime = os.environ.get("PRIM1_RUNTIME_DIR")
    if inherited_runtime:
        require(absolute_path(inherited_runtime) == runtime, "Inherited runtime does not match the expected preview")
    identity = os.environ.get("PRIM1_PANE_IDENTITY", "")
    require(identity.startswith("session-"), "Missing inherited pane identity")
    member_id = str(uuid.UUID(identity.removeprefix("session-")))
    room_id = str(uuid.UUID(args.expected_room))
    require(args.expected_label.strip(), "An explicit Generic Terminal fixture label is required")
    record({"check": "environment", "expected_exe": str(executable), "expected_runtime": str(runtime),
        "runtime_environment_checked": bool(inherited_runtime),
        "runtime_limit": "Root independently attests actual runtime via CDP; ping exposes no runtime path."})
    client = PaneMcp(executable)
    try:
        initialized = client.send("initialize", {"protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "prim1-s8-manual-fixture", "version": "1"}})
        require(initialized.get("serverInfo", {}).get("name") == "prim1-pane", "Unexpected MCP server")
        client.send("notifications/initialized", notification=True)
        require(client.tool("ping").get("message") == "pong", "Pane ping failed")

        def read(cursor=None):
            payload = client.tool("room_read", {"cursor": cursor} if cursor else {})["payload"]
            require(payload.get("kind") == "room_feed", "Expected room-feed result")
            page = payload["page"]
            require(page["room_id"] == room_id, "Fixture belongs to a different room")
            members = page["members"]
            require(members and all(m["driver"] == "generic_terminal" for m in members), "Room is not an isolated Generic Terminal fixture")
            own = [m for m in members if m["session_id"] == member_id]
            require(len(own) == 1 and own[0]["label"] == args.expected_label, "Authenticated fixture member does not match")
            require(len(page.get("cards", [])) <= 64, "Card snapshot exceeds the room bound")
            return page

        before = read()
        record({"check": "caller", "room_id": room_id, "member_id": member_id,
            "label": args.expected_label, "driver": "generic_terminal", "ping": "pong"})
        own_keys = {e["item"]["card"]["key"] for e in before.get("cards", []) if e["item"]["author"] == member_id}
        if args.phase in ("update", "churn"):
            require({c["key"] for c in initial_cards()} <= own_keys, "Run initial before update/churn")

        def write(name, body, request_id, duplicate=False):
            require(request_id.startswith(PREFIX), "Unscoped fixture request ID")
            arguments = dict(body, request_id=request_id)
            receipt = client.tool(name, arguments)["payload"]["result"]
            require(receipt["room_id"] == room_id, "Room changed during fixture; stop immediately")
            record({"tool": name, "receipt": receipt})
            if duplicate:
                repeated = client.tool(name, arguments)["payload"]["result"]
                require(repeated["replayed"] and dict(repeated, replayed=False) == dict(receipt, replayed=False), "Stable request ID did not deduplicate exactly")
                record({"check": "dedup", "tool": name, "receipt": repeated})

        if args.phase == "initial":
            write("room_post", {"content": "S8 fixture member chatter: visible in Raw after typed cards arrive."}, PREFIX + "chatter", True)
            for card in initial_cards():
                write("post_card", {"card": card}, PREFIX + "initial:" + card["key"], True)
        elif args.phase == "update":
            for card in initial_cards():
                card["summary"] = "Updated version; existing topic position stays stable"
                card["body"] = "Updated detail body. Selection should retain its displayed version until released.\n" + card["body"]
                if card["kind"] == "progress":
                    card["data"]["done"] = 2
                if card["kind"] == "metric":
                    card["data"].update(value=10.5, series=[8, 13, 9, 15, 10.5])
                write("post_card", {"card": card}, PREFIX + "update:" + card["key"], True)
        elif args.phase == "churn":
            for index in range(1, 57):
                card = {"key": f"qa-page-{index:02}", "kind": "report", "title": f"Paging topic {index:02}",
                    "summary": "Bounded paging fixture", "body": "Ordinary topic; the open decision stays pinned.", "state": "done"}
                write("post_card", {"card": card}, PREFIX + f"page:{index:02}")
            for index in range(520):
                write("room_post", {"content": f"S8 bounded raw-feed churn {index:03}"}, PREFIX + f"churn:{index:03}")
        # Each phase records the bounded snapshot and drains using only returned feed cursors.
        cursor = None
        for page_number in range(16):
            page = read(cursor)
            record({"check": "read", "page_number": page_number, "page": page})
            if not page["has_more"]:
                break
            require(page["cursor"] != cursor, "Room cursor did not advance")
            cursor = page["cursor"]
        else:
            raise RuntimeError("Fixture read exceeded its 16-page bound")
        if args.phase == "churn":
            expected_keys = {c["key"] for c in initial_cards()} | {f"qa-page-{i:02}" for i in range(1, 57)}
            retained = {e["item"]["card"]["key"] for e in page["cards"] if e["item"]["author"] == member_id}
            require(retained == expected_keys, "Latest-card snapshot lost topics after raw-feed churn")
            record({"check": "retained_after_churn", "topics": len(retained), "small_messages": 520})
    finally:
        client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("phase", choices=("initial", "update", "churn", "read"))
    for option in ("expected-exe", "expected-runtime", "expected-room", "expected-label", "out-dir"):
        parser.add_argument("--" + option, required=True)
    args = parser.parse_args()
    out = absolute_path(args.out_dir)
    out.mkdir(parents=True, exist_ok=True)
    path = out / ("typed-cards-" + args.phase + "-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%fZ") + ".jsonl")
    with path.open("x", encoding="utf-8") as stream:
        def record(value):
            stream.write(json.dumps(value, ensure_ascii=True, separators=(",", ":")) + "\n")
            stream.flush()
        try:
            run(args, record)
            record({"outcome": "phase_complete", "phase": args.phase, "ui_acceptance": "Root must inspect the production UI"})
        except Exception as error:
            # Never dump stderr, environment, arbitrary tool errors or tracebacks.
            message = str(error) if type(error) is RuntimeError else type(error).__name__
            record({"outcome": "failed", "phase": args.phase, "reason": message, "retry": "No automatic retry; inspect recorded receipts"})
            print("Fixture stopped: " + message, file=sys.stderr)
            return 1
    print("Fixture phase recorded: " + str(path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
