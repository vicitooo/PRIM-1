"""Fixture-only tests. Retains its unique temporary tree; touches no real runtime."""
import copy
from concurrent.futures import ThreadPoolExecutor
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
from uuid import uuid4

SCRIPT = Path(__file__).with_name("prepare-experimental-runtime.py")
spec = importlib.util.spec_from_file_location("preparer", SCRIPT)
prep = importlib.util.module_from_spec(spec)
spec.loader.exec_module(prep)


class MigrationTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.tree = Path(tempfile.mkdtemp(prefix="prim1-migration-fixtures-"))
        print(f"Retained fixture tree: {cls.tree}")

    def setUp(self):
        self.root = self.tree / str(uuid4())
        self.root.mkdir()
        self.source = self.root / "source"
        self.source.mkdir()
        self.target = self.root / "prepared"
        self.sid, self.rid = str(uuid4()), str(uuid4())
        directory = {"namespace": "windows", "canonical_path": "D:\\All Repos\\victor", "identity": "fixture-only"}
        self.sessions = {"schema_version": 1, "workspace_preference": directory,
            "sessions": [{"session_id": self.sid, "label": "Codex – Тест", "driver": "codex",
                "working_directory": directory.copy(), "permission_profile": "normal",
                "harness_session_id": None, "running_at_shutdown": True,
                "launch_choices": {"model": "fixture-model", "effort": "high", "brief": {"mode": "room_default"}},
                "recovery_history": [], "last_pty_cols": 120, "last_pty_rows": 40}]}
        self.rooms = {"schema_version": 1, "rooms": [{"room_id": self.rid, "label": "Room — Български",
            "member_ids": [self.sid], "membership_revision": 2, "resume_on_restart": True,
            "brief_template": "Fixture brief\nunchanged", "join_floors": {self.sid: 9}}]}
        self.save()

    def save(self):
        for name, value in zip(prep.CATALOGS, (self.sessions, self.rooms)):
            (self.source / name).write_bytes(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())

    def run_prepare(self):
        return prep.prepare(self.source, self.target)

    def test_cli_preserves_everything_except_documented_changes(self):
        before = prep.read_catalog_bytes(self.source)
        result = subprocess.run([sys.executable, "-B", str(SCRIPT), "--source-runtime", str(self.source),
                                 "--target-root", str(self.target)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        receipt = json.loads((self.target / "receipt.json").read_text(encoding="utf-8"))
        expected_sessions, expected_rooms = copy.deepcopy(self.sessions), copy.deepcopy(self.rooms)
        expected_sessions["sessions"][0].update(running_at_shutdown=False, recovery=prep.UNKNOWN_CODEX)
        expected_rooms["rooms"][0].update(resume_on_restart=False, history_restarted=True)
        for name, expected in zip(prep.CATALOGS, (expected_sessions, expected_rooms)):
            self.assertEqual((self.source / name).read_bytes(), before[name])
            self.assertEqual((self.target / "source-snapshot" / name).read_bytes(), before[name])
            data = (self.target / "runtime" / name).read_bytes()
            self.assertEqual(json.loads(data), expected)
            self.assertEqual(receipt["runtime_sha256"][name], prep.sha(data))
        self.assertEqual(receipt["codex_holds_added"], [self.sid])
        self.assertEqual(receipt["counts"], {"rooms": 1, "sessions": 1})
        self.assertFalse((self.target / "runtime" / "rooms").exists())

    def test_idless_codex_keeps_prior_hold_or_gets_unknown_hold_even_when_stopped(self):
        held = {"original_id": None, "provenance": "fixture", "reason": "Keep exactly"}
        self.sessions["sessions"][0]["recovery"] = held
        self.save()
        self.assertEqual(self.run_prepare()["codex_holds_added"], [])
        data = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())
        self.assertEqual(data["sessions"][0]["recovery"], held)
        original = copy.deepcopy(self.sessions)
        original["sessions"][0].pop("recovery")
        original["sessions"][0]["running_at_shutdown"] = False
        self.assertEqual(prep.transform(original, self.rooms)[0]["sessions"][0]["recovery"], prep.UNKNOWN_CODEX)

    def test_recovery_only_ids_receive_import_hold_without_inventing_harness_id(self):
        previous = {"original_id": str(uuid4()), "provenance": "legacy_catalog",
                    "reason": "Original recovery reason must survive"}
        history = {"original_id": None, "provenance": "fixture", "reason": "Older evidence"}
        for driver in ("claude", "codex", "grok"):
            with self.subTest(driver=driver):
                row = self.sessions["sessions"][0]
                row.update(driver=driver, recovery=copy.deepcopy(previous),
                           recovery_history=[copy.deepcopy(history)])
                self.save()
                self.target = self.root / ("prepared-" + driver)
                before = prep.read_catalog_bytes(self.source)
                receipt = self.run_prepare()
                result = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())["sessions"][0]
                self.assertIsNone(result["harness_session_id"])
                self.assertEqual(result["recovery"], {"original_id": previous["original_id"],
                    "provenance": "room_import_in_use", "reason": prep.COPIED_CONVERSATION_REASON})
                self.assertEqual(result["recovery_history"], [history, previous])
                self.assertEqual(receipt["codex_holds_added"], [])
                self.assertEqual(receipt["conversation_holds_added"][0]["original_id"], previous["original_id"])
                self.assertEqual(prep.read_catalog_bytes(self.source), before)

    def test_conflicting_live_and_recovery_ids_remain_visible(self):
        row = self.sessions["sessions"][0]
        live_id, original_id = str(uuid4()), str(uuid4())
        previous = {"original_id": original_id, "provenance": "legacy_catalog", "reason": "Prior identity"}
        row.update(harness_session_id=live_id, recovery=copy.deepcopy(previous))
        self.save()
        receipt = self.run_prepare()
        result = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())["sessions"][0]
        self.assertEqual(result["harness_session_id"], live_id)
        self.assertEqual(result["recovery"]["original_id"], live_id)
        self.assertEqual(result["recovery_history"], [previous])
        conflict = next(item for item in receipt["unresolved"] if item["issue"] == "conflicting_conversation_ids")
        self.assertEqual((conflict["harness_session_id"], conflict["recovery_original_id"]), (live_id, original_id))
        row["recovery"]["original_id"] = live_id.upper()
        self.assertFalse(any(item["issue"] == "conflicting_conversation_ids"
                             for item in prep.transform(self.sessions, self.rooms)[2]))

    def test_reported_only_codex_thread_is_held_and_report_archived_intact(self):
        report = {"thread_id": str(uuid4()), "run_id": str(uuid4()), "generation": 7,
                  "rollout_path": "C:/fixture/rollout.jsonl", "reported_at": "2026-10-19T10:00:00Z"}
        previous = {"original_id": None, "provenance": "job_attested_codex_shell_report",
                    "reason": "Preserve provisional evidence", "reported_codex": report}
        history = {"original_id": None, "provenance": "fixture", "reason": "Earlier recovery"}
        self.sessions["sessions"][0].update(recovery=copy.deepcopy(previous),
                                          recovery_history=[copy.deepcopy(history)])
        self.save()
        before = prep.read_catalog_bytes(self.source)
        receipt = self.run_prepare()
        result = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())["sessions"][0]
        self.assertIsNone(result["harness_session_id"])
        self.assertEqual(result["recovery"], {"original_id": report["thread_id"],
            "provenance": "room_import_in_use", "reason": prep.COPIED_CONVERSATION_REASON})
        self.assertEqual(result["recovery_history"], [history, previous])
        self.assertEqual(receipt["conversation_holds_added"][0]["original_id"], report["thread_id"])
        self.assertEqual(receipt["codex_holds_added"], [])
        self.assertEqual(prep.read_catalog_bytes(self.source), before)

    def test_reported_codex_reference_participates_in_conflict_and_duplicate_checks(self):
        live_id, original_id, reported_id = str(uuid4()), str(uuid4()), str(uuid4())
        row = self.sessions["sessions"][0]
        row.update(harness_session_id=live_id, recovery={"original_id": original_id,
            "provenance": "job_attested_codex_shell_report", "reason": "Keep every reference",
            "reported_codex": {"thread_id": reported_id, "run_id": str(uuid4()), "generation": 1,
                "rollout_path": "C:/fixture/rollout.jsonl", "reported_at": "2026-10-19T10:00:00Z"}})
        second = copy.deepcopy(row)
        second.update(session_id=str(uuid4()), harness_session_id=reported_id.upper(), recovery=None)
        self.sessions["sessions"].append(second)
        self.save()
        receipt = self.run_prepare()
        result = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())["sessions"][0]
        self.assertEqual(result["recovery"]["original_id"], live_id)
        self.assertEqual(result["recovery_history"], [row["recovery"]])
        conflict = next(item for item in receipt["unresolved"] if item["issue"] == "conflicting_conversation_ids")
        self.assertEqual((conflict["harness_session_id"], conflict["recovery_original_id"],
                          conflict["reported_codex_thread_id"]), (live_id, original_id, reported_id))
        duplicate = next(item for item in receipt["unresolved"] if item["issue"] == "duplicate_conversation_id")
        self.assertEqual(duplicate["conversation_id"], reported_id)
        self.assertEqual(set(duplicate["session_ids"]), {row["session_id"], second["session_id"]})
        row["recovery"]["original_id"] = live_id.upper()
        row["recovery"]["reported_codex"]["thread_id"] = live_id.upper()
        self.assertFalse(any(item["issue"] == "conflicting_conversation_ids"
                             for item in prep.transform(self.sessions, self.rooms)[2]))

    def test_all_provider_ids_held_and_existing_recovery_archived(self):
        previous = {"original_id": str(uuid4()), "provenance": "fixture_prior", "reason": "Keep exact evidence"}
        history = {"original_id": str(uuid4()), "provenance": "fixture_history", "reason": "Older evidence"}
        rows = []
        for driver in ("claude", "codex", "grok"):
            row = copy.deepcopy(self.sessions["sessions"][0])
            row.update(session_id=str(uuid4()), driver=driver, harness_session_id=str(uuid4()),
                       running_at_shutdown=False, recovery_history=[copy.deepcopy(history)])
            if driver == "codex":
                row["recovery"] = copy.deepcopy(previous)
            rows.append(row)
        self.sessions["sessions"] = rows
        self.rooms["rooms"][0]["member_ids"] = [row["session_id"] for row in rows]
        self.save()
        before = prep.read_catalog_bytes(self.source)
        receipt = self.run_prepare()
        result = json.loads((self.target / "runtime" / prep.CATALOGS[0]).read_bytes())
        self.assertEqual(len(receipt["conversation_holds_added"]), 3)
        for old, row, recorded in zip(rows, result["sessions"], receipt["conversation_holds_added"]):
            self.assertEqual(row["harness_session_id"], old["harness_session_id"])
            self.assertEqual(row["recovery"], {"original_id": old["harness_session_id"],
                "provenance": "room_import_in_use", "reason": prep.COPIED_CONVERSATION_REASON})
            self.assertEqual(recorded["original_id"], old["harness_session_id"])
            self.assertEqual(row["recovery_history"], [history, previous] if row["driver"] == "codex" else [history])
            self.assertFalse(row["running_at_shutdown"])
        self.assertEqual(prep.read_catalog_bytes(self.source), before)
        # Re-preparing an already guarded fixture must not duplicate that guard in history.
        again = prep.transform(result, self.rooms)[0]
        self.assertEqual(again["sessions"], result["sessions"])

    def test_blank_id_has_one_missing_issue_and_no_invalid_id_issue(self):
        row = self.sessions["sessions"][0]
        for blank in (None, "", "   "):
            with self.subTest(blank=blank):
                row["harness_session_id"] = blank
                transformed, _, unresolved, holds, ownership = prep.transform(self.sessions, self.rooms)
                self.assertEqual(transformed["sessions"][0]["recovery"], prep.UNKNOWN_CODEX)
                self.assertEqual(sum(item["issue"] == "missing_harness_id" for item in unresolved), 1)
                self.assertFalse(any(item["issue"] == "invalid_conversation_id" for item in unresolved))
                self.assertEqual(holds, [self.sid])
                self.assertEqual(ownership, [])

    def test_duplicate_conversations_reported_not_reassigned(self):
        row = self.sessions["sessions"][0]
        row["harness_session_id"] = str(uuid4())
        second = copy.deepcopy(row)
        second["session_id"] = str(uuid4())
        second["harness_session_id"] = row["harness_session_id"].upper()
        self.sessions["sessions"].append(second)  # Lobby member is retained.
        self.save()
        receipt = self.run_prepare()
        self.assertEqual(receipt["counts"]["sessions"], 2)
        duplicate = next(item for item in receipt["unresolved"] if item["issue"] == "duplicate_conversation_id")
        self.assertEqual(len(duplicate["session_ids"]), 2)

    def test_catalog_change_refused_before_target_creation(self):
        first = prep.read_catalog_bytes(self.source)
        second = dict(first)
        second[prep.CATALOGS[0]] += b" "
        with patch.object(prep, "read_catalog_bytes", side_effect=[first, second]):
            with self.assertRaisesRegex(ValueError, "changed between reads"):
                self.run_prepare()
        self.assertFalse(self.target.exists())

    def test_invalid_schema_ids_membership_and_duplicate_properties(self):
        mutations = [lambda s, r: s.update(schema_version=2),
            lambda s, r: s["sessions"].append(copy.deepcopy(s["sessions"][0])),
            lambda s, r: r["rooms"][0].update(member_ids=[str(uuid4())]),
            lambda s, r: r["rooms"].append(dict(r["rooms"][0], room_id=str(uuid4()))),
            lambda s, r: r["rooms"][0].update(room_id="invalid"),
            lambda s, r: s["sessions"][0].update(running_at_shutdown="true"),
            lambda s, r: s.update(unknown=True)]
        for change in mutations:
            with self.subTest(change=change):
                sessions, rooms = copy.deepcopy(self.sessions), copy.deepcopy(self.rooms)
                change(sessions, rooms)
                with self.assertRaises(ValueError):
                    prep.validate(sessions, rooms)
        with self.assertRaisesRegex(ValueError, "Duplicate JSON"):
            json.loads('{"schema_version":1,"schema_version":2}', object_pairs_hook=prep.unique_object)

    def test_existing_destination_overlap_and_durable_logs_refused(self):
        self.target.mkdir()
        marker = self.target / "existing.txt"
        marker.write_bytes(b"keep")
        with self.assertRaisesRegex(ValueError, "empty directory"):
            self.run_prepare()
        self.assertEqual(marker.read_bytes(), b"keep")
        for target in (self.source, self.source / "child", self.root):
            with self.assertRaisesRegex(ValueError, "overlap"):
                prep.prepare(self.source, target)
        (self.source / "rooms").mkdir()
        (self.source / "rooms" / (self.rid + ".jsonl")).write_bytes(b"never read this fixture log")
        with self.assertRaisesRegex(ValueError, "durable room logs"):
            prep.prepare(self.source, self.root / "other")

    def test_windows_junction_escape_refused(self):
        if os.name != "nt":
            self.skipTest("Windows junction fixture")
        outside = self.root / "outside"
        outside.mkdir()
        link = self.root / "junction"
        result = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(outside)], capture_output=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        with self.assertRaisesRegex(ValueError, "reparse"):
            prep.prepare(self.source, link / "target")
        with self.assertRaisesRegex(ValueError, "reparse"):
            prep.prepare(link, self.target)
        self.assertEqual(list(outside.iterdir()), [])

    def test_protected_targets_refused_before_writes(self):
        local = self.root / "local-app-data"
        with patch.dict(os.environ, {"LOCALAPPDATA": str(local)}):
            for suffix in ("io.prim1.runtime", "io.prim1.v2", "Programs/PRIM-1"):
                for target in (local / suffix, local / suffix / "new-copy"):
                    with self.subTest(target=target), self.assertRaisesRegex(ValueError, "Protected"):
                        prep.prepare(self.source, target)
            self.assertFalse(local.exists())
        for target in ("D:/prim1-v2-victor-walk-20261014", "d:/PRIM1-V2-VICTOR-WALK-20261014/new-copy"):
            with self.assertRaisesRegex(ValueError, "Protected"):
                prep.refuse_protected_target(target)
        # Similar names must not be mistaken for descendants.
        prep.refuse_protected_target("D:/prim1-v2-victor-walk-20261014-separate")

    @unittest.skipUnless(os.name == "nt", "Windows ReplaceFileW sharing contract")
    def test_live_writer_can_replace_catalog_while_reader_is_open(self):
        import ctypes
        from ctypes import wintypes
        path = self.source / prep.CATALOGS[0]
        before = path.read_bytes()
        replacement = self.source / "replacement.fixture"
        after = before + b"\n"
        replacement.write_bytes(after)
        kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        replace = kernel.ReplaceFileW
        replace.argtypes = (wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.LPCWSTR,
                            wintypes.DWORD, ctypes.c_void_p, ctypes.c_void_p)
        replace.restype = wintypes.BOOL

        def writer():
            if not replace(str(path), str(replacement), None, 0, None, None):
                raise ctypes.WinError(ctypes.get_last_error())

        with prep.open_catalog_read(path) as reader, ThreadPoolExecutor(max_workers=1) as workers:
            workers.submit(writer).result(timeout=10)
            self.assertEqual(reader.read(), before)
            self.assertEqual(path.read_bytes(), after)
        # The descriptor/Win32 handle is released on context exit too.
        path.rename(self.source / "closed-reader.fixture")

    def test_failed_write_leaves_no_completion_receipt(self):
        for failure_at in (1, 5):  # First catalog and the final receipt fence.
            self.target = self.root / f"failed-at-{failure_at}"
            with patch.object(prep.os, "fsync", side_effect=[None] * (failure_at - 1) + [OSError("fixture disk failure")]):
                with self.assertRaises(OSError):
                    self.run_prepare()
            self.assertFalse((self.target / "receipt.json").exists())
            with self.assertRaisesRegex(ValueError, "empty directory"):
                self.run_prepare()


if __name__ == "__main__":
    unittest.main(verbosity=2)
