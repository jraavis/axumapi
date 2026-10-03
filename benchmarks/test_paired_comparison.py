"""Confidence gates must preserve trial pairs and reject weak evidence."""

import math
import unittest
from dataclasses import replace

from benchmark_metrics import BenchmarkError, parse_ab
from paired_comparison import compare_pairs
from test_benchmark_runner import OUTPUT


def trial(rate, pair):
    """Build one valid synthetic trial for pairing-specific regressions.

    Args:
        rate: Positive request throughput.
        pair: Independent repetition identity, or None for old evidence.

    Returns:
        A complete trial with internally consistent timing/count metrics.
    """
    return replace(parse_ab(OUTPUT, 0, 100, 10), req_per_sec=rate,
                   elapsed_seconds=500000 / rate,
                   requested_requests=500000, completed_requests=500000,
                   pair_index=pair, p99_ms=100, runtime_workers=1,
                   latency_resolution_ms=0)


class PairedTests(unittest.TestCase):
    """A median advantage alone cannot satisfy the confidence gate."""

    def test_sql_settings_must_be_present_and_equal(self):
        left = [replace(trial(200, index), database_backend="sqlite",
                        database_settings={"version": "3.53.4"})
                for index in range(5)]
        right = [replace(trial(100, index), database_backend="sqlite",
                         database_settings={"version": "3.53.4"})
                 for index in range(5)]
        self.assertTrue(compare_pairs(left, right)["lead_gate_passed"])
        for settings in (None, {}, {"version": "3.46.0"}):
            right[0] = replace(right[0], database_settings=settings)
            result = compare_pairs(left, right)
            self.assertFalse(result["lead_gate_passed"])
            self.assertFalse(result["adoption_gate_passed"])

    def test_mongodb_concern_drift_rejects_an_apparent_lead(self):
        settings = {"version": "8.3.11", "client_write_concern": "{}"}
        left = [replace(trial(200, index), database_backend="mongodb",
                        database_settings=settings) for index in range(5)]
        right = [replace(trial(100, index), database_backend="mongodb",
                         database_settings=settings) for index in range(5)]
        self.assertTrue(compare_pairs(left, right)["lead_gate_passed"])
        right[0] = replace(right[0], database_settings={
            **settings, "client_write_concern": '{"w":1}',
        })
        self.assertFalse(compare_pairs(left, right)["lead_gate_passed"])

    def test_pair_ratios_preserve_shared_drift(self):
        baselines = [10, 1000, 10, 1000, 10]
        ratios = [1, 1, 100, 1, 100]
        left = [trial(rate * ratio, index)
                for index, (rate, ratio) in enumerate(zip(baselines, ratios))]
        right = [trial(rate, index) for index, rate in enumerate(baselines)]
        result = compare_pairs(left, right)
        self.assertAlmostEqual(result["paired_geometric_ratio"],
                               math.exp(sum(map(math.log, ratios)) / 5))
        self.assertNotAlmostEqual(result["paired_geometric_ratio"], 100)
        self.assertEqual(result, compare_pairs(left, right))

    def test_stable_lead_passes_and_variability_can_fail(self):
        right = [trial(100, index) for index in range(5)]
        stable = [trial(200, index) for index in range(5)]
        self.assertTrue(compare_pairs(stable, right)["lead_gate_passed"])
        noisy = [trial(rate, index)
                 for index, rate in enumerate([50, 200, 200, 200, 200])]
        result = compare_pairs(noisy, right)
        self.assertGreater(result["paired_geometric_ratio"], 1.1)
        self.assertFalse(result["lead_gate_passed"])

    def test_short_or_unidentified_evidence_cannot_pass(self):
        right = [trial(100, index) for index in range(5)]
        left = [trial(200, index) for index in range(5)]
        result = compare_pairs(left[:1], right[:1])
        self.assertIsNone(result["percentile_interval"])
        self.assertFalse(result["lead_gate_passed"])
        short = [replace(run, elapsed_seconds=1) for run in left]
        self.assertFalse(compare_pairs(short, right)["lead_gate_passed"])
        old_left = [replace(run, pair_index=None) for run in left]
        old_right = [replace(run, pair_index=None) for run in right]
        old = compare_pairs(old_left, old_right)
        self.assertFalse(old["lead_gate_passed"])

    def test_mismatched_or_repeated_pairs_fail_explicitly(self):
        left = [trial(200, index) for index in range(5)]
        right = [trial(100, index) for index in range(5)]
        invalid = [[], right[:-1], list(reversed(right)),
                   [replace(run, concurrency=2) for run in right],
                   [replace(run, req_per_sec=0) for run in right]]
        for other in invalid:
            with self.subTest(other=other), self.assertRaises(BenchmarkError):
                compare_pairs(left, other)
        with self.assertRaises(BenchmarkError):
            compare_pairs(left * 2, right * 2)

    def test_equal_boundary_does_not_establish_a_strict_lead(self):
        left = [trial(110, index) for index in range(5)]
        right = [trial(100, index) for index in range(5)]
        self.assertFalse(compare_pairs(left, right)["lead_gate_passed"])

    def test_adoption_and_strong_lead_are_distinct(self):
        right = [trial(100, index) for index in range(5)]
        left = [trial(rate, index)
                for index, rate in enumerate([108, 110, 112, 114, 116])]
        result = compare_pairs(left, right)
        self.assertTrue(result["adoption_gate_passed"])
        self.assertFalse(result["lead_gate_passed"])

    def test_tail_regressions_and_unknown_ratios_fail_both_gates(self):
        right = [trial(100, index) for index in range(5)]
        left = [trial(200, index) for index in range(5)]
        for tails in (106, 0, None, float("nan")):
            with self.subTest(tails=tails):
                if tails is None or math.isnan(tails):
                    with self.assertRaises(BenchmarkError):
                        compare_pairs([replace(run, p99_ms=tails)
                                       for run in left], right)
                    continue
                first, second = left, right
                if tails == 0:
                    second = [replace(run, p99_ms=0) for run in right]
                else:
                    first = [replace(run, p99_ms=tails) for run in left]
                result = compare_pairs(first, second)
                self.assertFalse(result["adoption_gate_passed"])
                self.assertFalse(result["lead_gate_passed"])
        boundary = [replace(run, p99_ms=105) for run in left]
        self.assertTrue(compare_pairs(boundary, right)["lead_gate_passed"])

    def test_unknown_or_different_workers_cannot_establish_a_lead(self):
        right = [trial(100, index) for index in range(5)]
        left = [trial(200, index) for index in range(5)]
        for count in (None, 2):
            runs = [replace(run, runtime_workers=count) for run in left]
            result = compare_pairs(runs, right)
            self.assertFalse(result["adoption_gate_passed"])
            self.assertFalse(result["lead_gate_passed"])

    def test_equal_rounded_tails_do_not_prove_a_five_percent_budget(self):
        right = [replace(trial(100, index), p99_ms=1,
                         latency_resolution_ms=1) for index in range(5)]
        left = [replace(trial(200, index), p99_ms=1,
                        latency_resolution_ms=1) for index in range(5)]
        result = compare_pairs(left, right)
        self.assertEqual(result["paired_p99_rounding_upper_bounds"], [3] * 5)
        self.assertFalse(result["lead_gate_passed"])
