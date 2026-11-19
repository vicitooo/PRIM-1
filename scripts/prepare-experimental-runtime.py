#!/usr/bin/env python3
"""Prepare a stopped Experimental copy of legacy PRIM-1 catalogs; never launch it."""
import argparse
import copy
import hashlib
import json
import ntpath
import os
from pathlib import Path
import stat
import sys
from datetime import datetime, timezone
from uuid import UUID

CATALOGS = ("session-catalog-v1.json", "room-catalog-v1.json")
MAX_BYTES = 16 * 1024 * 1024
SOURCE_REVISION = "fce0247607b30bf711eb6fb4258efe22f12d242f"
UNKNOWN_CODEX = {
    "original_id": None,
    "provenance": "legacy_catalog",
    "reason": "The imported Codex session has no verified conversation ID. No replacement was guessed.",
}
COPIED_CONVERSATION_REASON = (
    "Copied from the live PRIM-1, which may still be running this conversation. "
    "Choose Start fresh session here, or Resume here only after closing it in the live PRIM-1."
)


def require(ok, message):
    if not ok:
        raise ValueError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def checked_path(path):
    """Reject symlink/reparse ancestors, including Windows junctions, before I/O."""
    path = Path(path)
    require(path.is_absolute(), "Use explicit absolute source and target paths")
    require(".." not in path.parts, "Parent traversal is not accepted")
    for part in reversed((path, *path.parents)):
        try:
            info = part.lstat()
        except FileNotFoundError:
            continue
        require(not stat.S_ISLNK(info.st_mode) and not (
            getattr(info, "st_file_attributes", 0) & stat.FILE_ATTRIBUTE_REPARSE_POINT
        ), f"Symlink/reparse path refused: {part}")
    return path.resolve()


def read_catalog_bytes(source):
    result = {}
    for name in CATALOGS:
        path = checked_path(source / name)
        require(path.is_file(), f"Missing catalog: {path}")
        with open_catalog_read(path) as stream:
            data = stream.read(MAX_BYTES + 1)
        require(len(data) <= MAX_BYTES, f"Catalog exceeds 16 MiB: {name}")
        result[name] = data
    return result


def open_catalog_read(path):
    """Do not block the live writer's ReplaceFileW while inspecting its catalog."""
    if os.name != "nt":
        return Path(path).open("rb")
    import ctypes
    from ctypes import wintypes
    import msvcrt
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    create = kernel.CreateFileW
    create.argtypes = (wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
                       wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE)
    create.restype = wintypes.HANDLE
    close = kernel.CloseHandle
    close.argtypes = (wintypes.HANDLE,)
    close.restype = wintypes.BOOL
    handle = create(str(path), 0x80000000, 0x1 | 0x2 | 0x4, None, 3, 0x80, None)
    if handle == ctypes.c_void_p(-1).value:
        raise ctypes.WinError(ctypes.get_last_error())
    try:
        fd = msvcrt.open_osfhandle(handle, os.O_RDONLY | os.O_BINARY)
    except BaseException:
        close(handle)
        raise
    # Ownership transferred to the CRT descriptor; closing it closes the handle.
    try:
        return os.fdopen(fd, "rb")
    except BaseException:
        os.close(fd)
        raise


def refuse_protected_target(target):
    local = os.environ.get("LOCALAPPDATA")
    if not local and os.environ.get("USERPROFILE"):
        local = str(Path(os.environ["USERPROFILE"]) / "AppData" / "Local")
    protected = ["D:/prim1-v2-victor-walk-20261014"]
    if local:
        protected.extend(str(Path(local) / suffix) for suffix in
                         ("io.prim1.runtime", "io.prim1.v2", "Programs/PRIM-1"))

    def normalized(value):
        value = str(value).replace("/", "\\")
        if value.startswith("\\\\?\\UNC\\"):
            value = "\\\\" + value[8:]
        elif value.startswith("\\\\?\\"):
            value = value[4:]
        return ntpath.normcase(ntpath.normpath(value))

    candidate = normalized(target)
    for root in protected:
        root = normalized(root)
        require(candidate != root and not candidate.startswith(root.rstrip("\\") + "\\"),
                f"Protected live/Experimental target refused: {target}")


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        require(key not in result, f"Duplicate JSON property: {key}")
        result[key] = value
    return result


def fields(value, required, optional=()):
    require(isinstance(value, dict), "Expected a JSON object")
    require(set(required) <= value.keys(), f"Missing fields: {set(required) - value.keys()}")
    require(value.keys() <= set(required) | set(optional),
            f"Unsupported fields: {value.keys() - set(required) - set(optional)}")


def uid(value):
    require(isinstance(value, str), "IDs must be strings")
    parsed = UUID(value)
    require(parsed.int != 0, "Nil IDs are not accepted")
    return str(parsed)


def text(value, label):
    require(isinstance(value, str) and bool(value.strip()) and "\0" not in value,
            f"Invalid {label}")


def directory(value):
    fields(value, ("canonical_path", "identity"), ("namespace",))
    namespace = value.get("namespace", "windows")
    require(namespace in ("windows", "wsl_ubuntu"), "Unsupported directory namespace")
    for name in ("canonical_path", "identity"):
        text(value[name], name)
    path = value["canonical_path"]
    require(path == path.strip() and (
        ntpath.isabs(path) if namespace == "windows" else path.startswith("/")
    ), "Workspace must have an absolute canonical path")


def recovery(value):
    fields(value, ("original_id", "provenance", "reason"), ("reported_codex",))
    require(value["original_id"] is None or isinstance(value["original_id"], str),
            "Invalid recovery original_id")
    for name in ("provenance", "reason"):
        text(value[name], name)
    if value.get("reported_codex") is not None:
        report = value["reported_codex"]
        fields(report, ("thread_id", "run_id", "generation", "rollout_path", "reported_at"))
        uid(report["run_id"])
        require(type(report["generation"]) is int and report["generation"] >= 0,
                "Invalid reported generation")
        for name in ("thread_id", "rollout_path", "reported_at"):
            text(report[name], name)


def validate(sessions, rooms):
    fields(sessions, ("schema_version", "workspace_preference", "sessions"))
    fields(rooms, ("schema_version", "rooms"))
    for catalog in (sessions, rooms):
        require(type(catalog["schema_version"]) is int and catalog["schema_version"] == 1,
                "Only catalog schema_version 1 is supported")
    directory(sessions["workspace_preference"])
    require(sessions["workspace_preference"].get("namespace", "windows") == "windows",
            "Workspace preference must use Windows")
    require(isinstance(sessions["sessions"], list) and isinstance(rooms["rooms"], list),
            "Catalog rows must be arrays")
    require(len(rooms["rooms"]) <= 64, "More than 64 rooms")
    known, seen_rooms, membership = set(), set(), set()
    for row in sessions["sessions"]:
        fields(row, ("session_id", "label", "driver", "working_directory", "permission_profile"),
               ("launch_choices", "last_launch", "harness_session_id", "recovery", "recovery_history",
                "running_at_shutdown", "last_pty_cols", "last_pty_rows"))
        ident = uid(row["session_id"])
        require(ident not in known, "Duplicate PRIM session UUID")
        known.add(ident)
        text(row["label"], "session label")
        require(row["driver"] in ("claude", "codex", "grok", "prime", "generic_terminal"), "Unknown driver")
        require(row["permission_profile"] in ("normal", "unsafe"), "Unknown permission profile")
        require(row["driver"] not in ("prime", "generic_terminal") or row["permission_profile"] == "normal",
                "Terminal/Prime cannot be unsafe")
        directory(row["working_directory"])
        require(type(row.get("running_at_shutdown", False)) is bool, "Invalid running flag")
        require(row.get("harness_session_id") is None or isinstance(row["harness_session_id"], str),
                "Invalid harness ID type")
        if row.get("recovery") is not None:
            recovery(row["recovery"])
        require(isinstance(row.get("recovery_history", []), list), "Invalid recovery history")
        for previous in row.get("recovery_history", []):
            recovery(previous)
        for name in ("last_pty_cols", "last_pty_rows"):
            require(row.get(name) is None or (type(row[name]) is int and 0 <= row[name] <= 65535), "Invalid PTY size")
        if row.get("launch_choices") is not None:
            choices = row["launch_choices"]
            fields(choices, (), ("model", "effort", "brief"))
            for name in ("model", "effort"):
                require(choices.get(name) is None or isinstance(choices[name], str), "Invalid launch choice")
            if "brief" in choices:
                brief = choices["brief"]
                fields(brief, ("mode",), ("text",))
                require(brief["mode"] in ("room_default", "custom", "none"), "Invalid brief mode")
                if brief["mode"] == "custom":
                    text(brief.get("text"), "custom brief")
        require(row.get("last_launch") is None or isinstance(row["last_launch"], dict), "Invalid last launch")
    for row in rooms["rooms"]:
        fields(row, ("room_id", "label", "member_ids", "membership_revision"),
               ("join_floors", "join_floor_epoch", "history_restarted", "resume_on_restart",
                "brief_on_join", "brief_template", "briefed_member_ids"))
        ident = uid(row["room_id"])
        require(ident not in seen_rooms, "Duplicate room UUID")
        seen_rooms.add(ident)
        text(row["label"], "room label")
        require(type(row["membership_revision"]) is int and 0 < row["membership_revision"] < 2**64,
                "Invalid membership revision")
        require(isinstance(row["member_ids"], list) and len(row["member_ids"]) <= 64, "Invalid members")
        members = [uid(item) for item in row["member_ids"]]
        require(len(set(members)) == len(members), "Duplicate member in a room")
        require(set(members) <= known, "Room references missing sessions")
        require(not membership.intersection(members), "Session belongs to more than one room")
        membership.update(members)
        for name in ("history_restarted", "resume_on_restart", "brief_on_join"):
            require(name not in row or type(row[name]) is bool, f"Invalid {name}")
        if row.get("join_floor_epoch") is not None:
            uid(row["join_floor_epoch"])
        if row.get("join_floors") is not None:
            require(isinstance(row["join_floors"], dict), "Invalid join floors")
            for key, value in row["join_floors"].items():
                uid(key)
                require(type(value) is int and 0 <= value < 2**64, "Invalid join floor")
        require(row.get("brief_template") is None or isinstance(row["brief_template"], str), "Invalid room brief")
        if row.get("briefed_member_ids") is not None:
            require(isinstance(row["briefed_member_ids"], list), "Invalid briefed members")
            for member in row["briefed_member_ids"]:
                uid(member)


def transform(sessions, rooms):
    sessions, rooms = copy.deepcopy(sessions), copy.deepcopy(rooms)
    unresolved, holds_added, ownership_holds, owners = [], [], [], {}
    for row in sessions["sessions"]:
        if row["driver"] in ("claude", "codex", "grok"):
            # Blank IDs are missing references, not a second invalid-ID issue.
            live_id = row.get("harness_session_id")
            live_id = live_id if live_id and live_id.strip() else None
            original_id = (row.get("recovery") or {}).get("original_id")
            original_id = original_id if original_id and original_id.strip() else None
            reported_id = ((row.get("recovery") or {}).get("reported_codex") or {}).get("thread_id")
            reported_id = reported_id if reported_id and reported_id.strip() else None
            if not live_id:
                unresolved.append({"session_id": row["session_id"], "issue": "missing_harness_id",
                                   "previously_running": row.get("running_at_shutdown", False)})
            reference_keys = set()
            for reference in {live_id, original_id, reported_id} - {None}:
                try:
                    normalized = uid(reference)
                except (ValueError, AttributeError):
                    reference_keys.add(reference)
                    unresolved.append({"session_id": row["session_id"], "issue": "invalid_conversation_id"})
                    continue
                reference_keys.add(normalized)
                owners.setdefault((row["driver"], normalized), set()).add(row["session_id"])
            if len(reference_keys) > 1:
                unresolved.append({"session_id": row["session_id"], "issue": "conflicting_conversation_ids",
                                   "harness_session_id": live_id, "recovery_original_id": original_id,
                                   "reported_codex_thread_id": reported_id})
            # Recovery-only and reported references can be resumed by the importer. Guard
            # that existing ID without promoting it into harness_session_id.
            guarded_id = live_id or original_id or reported_id
            if guarded_id is not None:
                hold = {"original_id": guarded_id, "provenance": "room_import_in_use",
                        "reason": COPIED_CONVERSATION_REASON}
                previous = row.get("recovery")
                if previous is not None and previous != hold:
                    row.setdefault("recovery_history", []).append(copy.deepcopy(previous))
                row["recovery"] = hold
                ownership_holds.append({"session_id": row["session_id"], "driver": row["driver"],
                                        "original_id": guarded_id, "provenance": hold["provenance"],
                                        "reason": hold["reason"]})
            elif row["driver"] == "codex" and row.get("recovery") is None:
                row["recovery"] = copy.deepcopy(UNKNOWN_CODEX)
                holds_added.append(row["session_id"])
            if row.get("recovery") is not None:
                unresolved.append({"session_id": row["session_id"], "issue": "recovery_hold",
                                   "provenance": row["recovery"]["provenance"]})
        row["running_at_shutdown"] = False
    for (driver, reference), member_ids in sorted(owners.items()):
        if len(member_ids) > 1:
            unresolved.append({"issue": "duplicate_conversation_id", "driver": driver,
                               "conversation_id": reference, "session_ids": sorted(member_ids)})
    for row in rooms["rooms"]:
        row["resume_on_restart"] = False
        row["history_restarted"] = True
    unresolved = list({json.dumps(item, sort_keys=True): item for item in unresolved}.values())
    return sessions, rooms, unresolved, holds_added, ownership_holds


def encode(value):
    return (json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n").encode("utf-8")


def prepare(source, target):
    source, target = checked_path(source), checked_path(target)
    refuse_protected_target(target)
    require(source.is_dir(), "Source runtime is not a directory")
    require(source != target and source not in target.parents and target not in source.parents,
            "Source and target cannot overlap")
    require(target.parent.is_dir(), "Target parent must already exist")
    require(not target.exists() or (target.is_dir() and not any(target.iterdir())),
            "Target must be absent or an empty directory; nothing is overwritten")
    first, second = read_catalog_bytes(source), read_catalog_bytes(source)
    require({k: sha(v) for k, v in first.items()} == {k: sha(v) for k, v in second.items()},
            "Source catalogs changed between reads; retry with a new stable snapshot")
    catalogs = [json.loads(first[name].decode("utf-8"), object_pairs_hook=unique_object,
                           parse_constant=lambda value: require(False, f"Invalid JSON number: {value}"))
                for name in CATALOGS]
    validate(*catalogs)
    logs = checked_path(source / "rooms")
    require(not logs.exists() or (logs.is_dir() and not any(logs.glob("*.jsonl"))),
            "Source has durable room logs; use room export instead of this legacy catalog-only preparer")
    sessions, rooms, unresolved, holds, ownership_holds = transform(*catalogs)
    validate(sessions, rooms)
    transformed = dict(zip(CATALOGS, (encode(sessions), encode(rooms))))
    checked_path(source)
    checked_path(target)
    refuse_protected_target(target)
    target.mkdir(exist_ok=True)
    require(not any(target.iterdir()), "Target changed before writing")

    def write(relative, data):
        path = checked_path(target / relative)
        require(target in path.parents, "Destination escaped target root")
        with path.open("xb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())

    for subdir, files in (("source-snapshot", first), ("runtime", transformed)):
        (target / subdir).mkdir()
        for name, data in files.items():
            write(Path(subdir) / name, data)
    receipt = {
        "format": "prim1-experimental-catalog-preparation-v1", "status": "prepared_not_launched",
        "at_utc": datetime.now(timezone.utc).isoformat(), "source_runtime": str(source),
        "target_runtime": str(target / "runtime"), "source_schema_revision": SOURCE_REVISION,
        "preparer_sha256": sha(Path(__file__).read_bytes()),
        "source_sha256": {k: sha(v) for k, v in first.items()},
        "runtime_sha256": {k: sha(v) for k, v in transformed.items()},
        "counts": {"rooms": len(rooms["rooms"]), "sessions": len(sessions["sessions"])},
        "changes": ["running_at_shutdown=false", "resume_on_restart=false", "history_restarted=true",
                    "all copied Claude/Codex/Grok conversation IDs held as room_import_in_use; prior recovery archived",
                    "Codex without ID and without prior recovery held as legacy_catalog"],
        "codex_holds_added": holds, "conversation_holds_added": ownership_holds, "unresolved": unresolved,
        "limits": ["No feed or harness history copied; new room history begins on first launch.",
                   "Stable double-read is not an atomic transaction across two live files.",
                   "No harness started, identity guessed, prior recovery discarded, or live source changed.",
                   "Stopped copies do not fence the old installation; do not activate a conversation twice.",
                   "Metadata validated; launch compatibility and actual resume remain candidate checks."],
    }
    # Publish the completion marker only after its bytes pass fsync. A failed
    # receipt write must not leave an apparently successful receipt.json.
    write("receipt.pending.json", encode(receipt))
    checked_path(target / "receipt.json")
    require(not (target / "receipt.json").exists(), "Receipt already exists")
    (target / "receipt.pending.json").rename(target / "receipt.json")
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source-runtime", required=True, type=Path)
    parser.add_argument("--target-root", required=True, type=Path)
    args = parser.parse_args()
    try:
        receipt = prepare(args.source_runtime, args.target_root)
    except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
        print(f"Preparation refused: {error}", file=sys.stderr)
        return 1
    print(json.dumps({"status": receipt["status"], "counts": receipt["counts"],
                      "unresolved": len(receipt["unresolved"]),
                      "receipt": str(args.target_root / "receipt.json")}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
