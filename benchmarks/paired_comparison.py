"""Paired throughput intervals with explicit publication and lead gates."""

import math
import random
import statistics

from benchmark_metrics import BenchmarkError, require_valid


def compare_pairs(left, right, *, samples=10000, seed=0, lead=1.10):
    """Resample complete trial pairs, preserving shared run-level variation.

    Args:
        left: Valid Siderite trials in pair order.
        right: Valid FastAPI trials in the same pair order.
        samples: Bootstrap draws; at least 1000 for percentile resolution.
        seed: Deterministic independent random seed for reproducibility.
        lead: Required strictly exceeded 95% lower throughput-ratio bound.

    Returns:
        Geometric paired ratio, percentile interval, inputs and gate reasons.
        A local interval describes these trials, not all deployments.

    Raises:
        BenchmarkError: Invalid, unmatched, repeated or incomparable pairs.
    """
    if not left or len(left) != len(right):
        raise BenchmarkError("Require equal nonempty paired trial sets")
    if samples < 1000 or not math.isfinite(lead) or lead <= 1:
        raise ValueError("Require >=1000 draws and a finite lead >1")
    logarithms = []
    pair_ids = []
    durations = []
    worker_counts = []
    tail_ratios = []
    tail_upper_bounds = []
    settings_match = []
    for first, second in zip(left, right):
        require_valid(first)
        require_valid(second)
        comparable = (
            first.pair_index == second.pair_index
            and first.concurrency == second.concurrency
            and first.requested_requests == second.requested_requests
            and first.keep_alive_requested == second.keep_alive_requested
        )
        if not comparable:
            raise BenchmarkError("Paired trials use different conditions")
        rates = (first.req_per_sec, second.req_per_sec)
        times = (first.elapsed_seconds, second.elapsed_seconds)
        if any(value is None or not math.isfinite(value) or value <= 0
               for value in (*rates, *times)):
            raise BenchmarkError("Invalid paired rate or elapsed time")
        logarithms.append(math.log(rates[0]) - math.log(rates[1]))
        durations.extend(times)
        pair_ids.append(first.pair_index)
        worker_counts.append([first.runtime_workers, second.runtime_workers])
        observed_backends = {"sqlite", "postgres", "mysql", "mongodb"}
        if (first.database_backend in observed_backends
                or second.database_backend in observed_backends):
            settings_match.append(
                first.database_backend == second.database_backend
                and isinstance(first.database_settings, dict)
                and bool(first.database_settings)
                and first.database_settings == second.database_settings
            )
        tails = (first.p99_ms, second.p99_ms)
        if any(value is None or not math.isfinite(value) or value < 0
               for value in tails):
            raise BenchmarkError("Invalid paired p99 latency")
        # Millisecond-rounded zero baselines cannot establish a ratio.
        tail_ratios.append(tails[0] / tails[1] if tails[1] > 0 else None)
        resolutions = (first.latency_resolution_ms,
                       second.latency_resolution_ms)
        if any(not math.isfinite(value) or value < 0
               for value in resolutions):
            raise BenchmarkError("Invalid latency timing resolution")
        denominator = tails[1] - resolutions[1] / 2
        tail_upper_bounds.append(
            (tails[0] + resolutions[0] / 2) / denominator
            if denominator > 0 else None
        )
    identified = all(index is not None for index in pair_ids)
    if identified and len(set(pair_ids)) != len(pair_ids):
        raise BenchmarkError("A trial pair appears more than once")
    rng = random.Random(seed)
    draws = sorted(
        statistics.fmean(rng.choices(logarithms, k=len(logarithms)))
        for _ in range(samples)
    )

    def percentile(fraction):
        position = fraction * (len(draws) - 1)
        lower = math.floor(position)
        upper = math.ceil(position)
        interpolated = draws[lower] + (
            draws[upper] - draws[lower]
        ) * (position - lower)
        return math.exp(interpolated)

    point = math.exp(statistics.fmean(logarithms))
    # A one-pair percentile bootstrap would give a misleading zero width.
    interval = (
        [percentile(0.025), percentile(0.975)] if len(left) >= 5 else None
    )
    reasons = []
    if not identified:
        reasons.append("Pair identities were not recorded")
    if len(left) < 5:
        reasons.append("Fewer than five independent trial pairs")
    if min(durations) < 10:
        reasons.append("A timed trial was shorter than ten seconds")
    if any(type(first) is not int or type(second) is not int
           or first < 1 or second < 1 or first != second
           for first, second in worker_counts):
        reasons.append("Actual scheduler worker counts are unknown or differ")
    if not all(settings_match):
        reasons.append(
            "Sampled database engine/session settings are missing or differ"
        )
    evidence_reasons = list(reasons)
    if any(value is None for value in tail_upper_bounds):
        reasons.append("A rounded zero p99 baseline prevents comparison")
    elif any(value > 1.05 and not math.isclose(value, 1.05,
                                             rel_tol=1e-12)
             for value in tail_upper_bounds):
        reasons.append("A paired p99 regression exceeds five percent")
    adoption_reasons = list(reasons)
    if point < lead and not math.isclose(point, lead, rel_tol=1e-12):
        adoption_reasons.append("Point throughput gain is below ten percent")
    if interval is not None and (
        interval[0] <= 1 or math.isclose(interval[0], 1, rel_tol=1e-12)
    ):
        adoption_reasons.append(
            "Lower confidence bound does not exceed parity"
        )
    if interval is not None and (
        interval[0] <= lead or math.isclose(interval[0], lead, rel_tol=1e-12)
    ):
        reasons.append("Lower confidence bound does not exceed the lead gate")
    return {
        "paired_geometric_ratio": point,
        "paired_ratios": [math.exp(value) for value in logarithms],
        "pair_ids": pair_ids,
        "pair_scheduler_workers": worker_counts,
        "database_pair_settings_match": settings_match,
        "pairs": len(left),
        "confidence_level": 0.95,
        "percentile_interval": interval,
        "bootstrap_draws": samples,
        "bootstrap_seed": seed,
        "minimum_trial_seconds": min(durations),
        "lead_threshold": lead,
        "adoption_gate_passed": not adoption_reasons,
        "adoption_gate_reasons": adoption_reasons,
        "evidence_gate_passed": not evidence_reasons,
        "paired_p99_ratios": tail_ratios,
        "paired_p99_rounding_upper_bounds": tail_upper_bounds,
        "p99_ratio_budget": 1.05,
        "lead_gate_passed": not reasons,
        "gate_reasons": reasons,
        "limitations": [
            "Interval covers this host, workload and durability profile",
            "Pair bootstrap does not remove systematic benchmark bias",
            "Repeated requests within a trial are not independent pairs",
        ],
    }
