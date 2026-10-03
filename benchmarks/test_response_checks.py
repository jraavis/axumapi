"""Verify HTTP correctness evidence and its separation from timed metrics."""

import contextlib
import io
import json
import sqlite3
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import run_benchmarks as runner
from benchmark_metrics import BenchmarkError, parse_ab
from database_checks import inspect_database
from response_checks import TITLE, check_insert_responses, request_insert
from test_benchmark_runner import OUTPUT


def response(status=201, key=101, title=TITLE, done=False):
    """Build a readable response for the HTTP client's context manager."""
    result = io.BytesIO(json.dumps({
        "id": key, "title": title, "done": done,
    }).encode())
    result.status = status
    return result


class ResponseTests(unittest.TestCase):
    """Check correctness failures independently of transport throughput."""

    def test_exact_status_payload_and_id_types(self):
        for options in (
            {"status": 200}, {"key": True}, {"key": 0},
            {"title": "wrong"}, {"done": 0},
        ):
            with self.subTest(options=options):
                with patch("urllib.request.urlopen", return_value=response(
                    **options
                )):
                    self.assertTrue(request_insert("http://s")[
                        "invalid_reasons"
                    ])

    def test_malformed_or_oversized_body_is_rejected(self):
        for body in (b"not-json", b"[]", b"x" * 65537):
            result = io.BytesIO(body)
            result.status = 201
            with patch("urllib.request.urlopen", return_value=result):
                self.assertTrue(request_insert("http://s")[
                    "invalid_reasons"
                ])

    def test_unique_ids_and_committed_rows_checked_in_one_batch(self):
        samples = [
            {"status": 201, "id": key, "invalid_reasons": []}
            for key in (101, 102, 103)
        ]
        calls = []

        def read(keys):
            calls.append(keys)
            return {key: (TITLE, False) for key in keys}

        with patch("response_checks.request_insert", side_effect=samples):
            result = check_insert_responses("http://s", read, 3)
        self.assertTrue(result["valid"])
        self.assertEqual(result["verified_rows"], 3)
        self.assertEqual(len(calls), 1)
        self.assertEqual(set(calls[0]), {101, 102, 103})
        self.assertFalse(result["measured_responses_checked"])

    def test_duplicate_or_uncommitted_ids_fail(self):
        for keys, rows in (
            ((101, 101), {101: (TITLE, False)}),
            ((101, 102), {101: (TITLE, False)}),
            ((101, 102), {101: (TITLE, False), 102: ("wrong", False)}),
        ):
            samples = [
                {"status": 201, "id": key, "invalid_reasons": []}
                for key in keys
            ]
            with patch("response_checks.request_insert", side_effect=samples):
                result = check_insert_responses("http://s", lambda _: rows, 2)
            self.assertFalse(result["valid"])

    def test_database_failure_is_recorded_without_secret_exception_text(self):
        sample = {"status": 201, "id": 101, "invalid_reasons": []}
        with patch("response_checks.request_insert", return_value=sample):
            result = check_insert_responses(
                "http://s", lambda _: 1 / 0, 1
            )
        self.assertFalse(result["valid"])
        self.assertIn("ZeroDivisionError", str(result["invalid_reasons"]))

    def test_sample_concurrency_is_bounded(self):
        sample = {"status": 201, "id": 101, "invalid_reasons": []}
        with patch("response_checks.ThreadPoolExecutor") as executor:
            worker = executor.return_value.__enter__.return_value
            worker.map.return_value = [sample] * 100
            result = check_insert_responses(
                "http://s", lambda _: {101: (TITLE, False)}, 1000
            )
        executor.assert_called_once_with(max_workers=100)
        self.assertEqual(result["requested_samples"], 100)
        self.assertFalse(result["valid"])
        with self.assertRaises(BenchmarkError):
            check_insert_responses("http://s", lambda _: {}, 0)

    def test_sqlite_batch_missing_duplicate_and_empty_keys(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "todos.db"
            with contextlib.closing(sqlite3.connect(path)) as conn:
                conn.execute("CREATE TABLE todos (id, title, done)")
                conn.execute("INSERT INTO todos VALUES (101, ?, 0)", (TITLE,))
                conn.commit()
            self.assertEqual(
                inspect_database("sqlite", "", path, (101, 101, 999)),
                {101: (TITLE, False)},
            )
            self.assertEqual(inspect_database("sqlite", "", path, ()), {})

    def test_failed_post_trial_check_retains_measured_row_delta(self):
        evidence = {"invalid_reasons": ["Wrong status"], "valid": False}
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(runner, "TRIALS_DIR", Path(directory)):
                with patch.object(
                    runner, "run_ab",
                    side_effect=lambda *a, **k: parse_ab(OUTPUT, 0, 100, 10),
                ):
                    with contextlib.redirect_stdout(io.StringIO()):
                        with self.assertRaises(BenchmarkError):
                            runner.run_benchmark_set(
                                "test", "http://s", "http://f", "/", 100, 10,
                                row_count_callback=iter([100, 200]).__next__,
                                response_check_callback=lambda *args: evidence,
                            )
            data = json.loads(next(Path(directory).glob("*.json")).read_text())
            self.assertEqual(data["row_delta"], 100)
            self.assertEqual(data["response_checks"], evidence)
            self.assertFalse(data["valid"])


if __name__ == "__main__":
    unittest.main()
