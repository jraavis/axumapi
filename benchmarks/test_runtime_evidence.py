"""A configured worker count alone cannot qualify a comparison."""

import json
import tempfile
import unittest
from pathlib import Path

from benchmark_metrics import BenchmarkError
from server_process import (ServerSpec, read_runtime,
                            record_database_settings)


class RuntimeTests(unittest.TestCase):
    """Accept only startup evidence from the current owned process."""

    def test_current_segment_and_pid_are_required(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = ServerSpec([], root, "", "", root / "server.log",
                              expected_workers=1)
            old = 'BENCHMARK_RUNTIME {"pid": 12, "scheduler_workers": 1}\n'
            spec.log_path.write_text(old)
            with self.assertRaises(BenchmarkError):
                read_runtime(spec, 12, len(old))
            for report in ({"pid": 13, "scheduler_workers": 1},
                           {"pid": 12, "scheduler_workers": 2},
                           {"pid": 12, "scheduler_workers": True},
                           []):
                spec.log_path.write_text("BENCHMARK_RUNTIME " +
                                         json.dumps(report) + "\n")
                with self.assertRaises(BenchmarkError):
                    read_runtime(spec, 12, 0)
            self.assertFalse((root / "runtime.jsonl").exists())

    def test_record_contains_only_sanitized_runtime_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = ServerSpec([], root, "", "", root / "server.log",
                              expected_workers=1)
            spec.log_path.write_text('BENCHMARK_RUNTIME '
                                     '{"pid": 12, "scheduler_workers": 1, '
                                     '"secret": "must not retain"}\n')
            self.assertEqual(read_runtime(spec, 12, 0),
                             {"pid": 12, "scheduler_workers": 1})
            record = json.loads((root / "runtime.jsonl").read_text())
            self.assertEqual(set(record),
                             {"application", "pid", "scheduler_workers"})


class DatabaseSettingsTests(unittest.TestCase):
    """Reject identity drift and arbitrary fields before retaining settings."""

    def test_sanitized_settings_and_invalid_reports(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            spec = ServerSpec([], root, "", "", root / "server.log")
            valid = {"pid": 12, "settings": {"synchronous": 2}}
            segment = "BENCHMARK_DATABASE " + json.dumps(valid)
            record_database_settings(spec, 12, segment)
            path = root / "database-settings.jsonl"
            record = json.loads(path.read_text())
            self.assertEqual(record["settings"], {"synchronous": 2})
            before = path.read_text()
            invalid = [
                {"pid": 13, "settings": {"synchronous": 2}},
                {"pid": True, "settings": {"synchronous": 2}},
                {"pid": 12, "settings": {"password": "secret"}},
                {"pid": 12, "settings": {"autocommit": True}},
                {"pid": 12, "settings": {}},
                [],
            ]
            for value in invalid:
                with self.assertRaises(BenchmarkError):
                    record_database_settings(
                        spec, 12, "BENCHMARK_DATABASE " + json.dumps(value)
                    )
            with self.assertRaises(BenchmarkError):
                record_database_settings(spec, 12, segment + "\n" + segment)
            self.assertEqual(path.read_text(), before)
