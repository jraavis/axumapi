"""Tail budgets need retained fine-resolution timing, not rounded equality."""

import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import run_benchmarks as runner
from benchmark_metrics import BenchmarkError, parse_ab
from latency_csv import apply_percentiles
from test_benchmark_runner import OUTPUT


def distribution():
    return "Percentage served,Time in ms\n" + "".join(
        f"{index},{index / 100:.3f}\n" for index in range(101)
    )


class LatencyTests(unittest.TestCase):
    """Incomplete or corrupt CSV data cannot enter a comparison."""

    def test_all_percentiles_and_finite_ordered_times_are_required(self):
        result = parse_ab(OUTPUT, 0, 100, 10)
        apply_percentiles(result, distribution())
        self.assertEqual((result.p50_ms, result.p90_ms, result.p99_ms),
                         (0.5, 0.9, 0.99))
        self.assertEqual(result.latency_resolution_ms, 0.001)
        for invalid in ("", distribution().replace("99,0.990\n", ""),
                        distribution().replace("99,0.990", "98,0.990"),
                        distribution().replace("99,0.990", "99,nan"),
                        distribution().replace("99,0.990", "99,-1.000"),
                        distribution().replace("99,0.990", "99,0.500")):
            with self.subTest(invalid=invalid), \
                    self.assertRaises(BenchmarkError):
                apply_percentiles(result, invalid)

    def test_small_samples_are_rejected_before_generator_execution(self):
        with patch.object(runner.subprocess, "run") as run:
            for count in (1, 20, 50, 99):
                with self.assertRaises(ValueError):
                    runner.run_ab("http://localhost/", count, 1)
            run.assert_not_called()

    def test_generator_retains_csv_and_rejects_a_missing_distribution(self):
        def successful(command, **kwargs):
            Path(command[command.index("-e") + 1]).write_text(distribution())
            return subprocess.CompletedProcess(command, 0, OUTPUT, "")

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with patch.object(runner, "TRIALS_DIR", root), \
                    patch.object(runner.subprocess, "run", successful):
                result = runner.run_ab("http://localhost/", 100, 10)
            self.assertTrue(result.valid)
            self.assertEqual(result.p99_ms, 0.99)
            self.assertEqual((root / result.latency_csv).read_text(),
                             distribution())
            with patch.object(runner, "TRIALS_DIR", root), \
                    patch.object(runner.subprocess, "run", return_value=
                                 subprocess.CompletedProcess([], 0, OUTPUT,
                                                             "")):
                invalid = runner.run_ab("http://localhost/", 100, 10)
            self.assertFalse(invalid.valid)
            self.assertTrue(any("Latency CSV" in reason
                                for reason in invalid.invalid_reasons))
