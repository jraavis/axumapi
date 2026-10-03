"""Validate and retain ApacheBench trials before publishing comparisons."""

from __future__ import annotations

import json
import math
import re
import statistics
import tempfile
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Optional


class BenchmarkError(RuntimeError):
    """A trial failed validation and must not enter a comparison."""


@dataclass
class BenchRunResult:
    """Measurements and validation evidence for one load-generator run."""

    req_per_sec: Optional[float]
    time_per_req_ms: Optional[float]
    p50_ms: Optional[float]
    p90_ms: Optional[float]
    p99_ms: Optional[float]
    failed_requests: Optional[int]
    raw_output: str
    exit_status: Optional[int]
    completed_requests: Optional[int]
    non_2xx_responses: int
    elapsed_seconds: Optional[float]
    requested_requests: int
    concurrency: int
    timed_out: bool
    invalid_reasons: list[str]
    row_delta: Optional[int] = None
    response_checks: Optional[dict] = None
    keep_alive_requested: bool = False
    keep_alive_requests: Optional[int] = None
    pair_index: Optional[int] = None
    runtime_workers: Optional[int] = None
    latency_resolution_ms: float = 1.0
    latency_csv: Optional[str] = None
    database_backend: Optional[str] = None
    database_settings: Optional[dict] = None

    @property
    def valid(self) -> bool:
        """Return whether the trial satisfies all recorded checks."""
        return not self.invalid_reasons


@dataclass
class MetricSummary:
    """Summary computed exclusively from validated trials."""

    median_rps: float
    min_rps: float
    max_rps: float
    median_time_per_req_ms: float
    median_p99_ms: float
    total_failures: int
    runs: list[BenchRunResult]
    paired_comparison: Optional[dict] = None


def parse_ab(
    output: str,
    exit_status: Optional[int],
    n: int,
    c: int,
    timed_out: bool = False,
    keep_alive: bool = False,
) -> BenchRunResult:
    """Parse metrics without inventing values for missing measurements.

    Args:
        output: Combined load-generator stdout and stderr.
        exit_status: Process exit code, or None after a timeout.
        n: Requested number of responses.
        c: Requested concurrency.
        timed_out: Whether the process exceeded its deadline.
        keep_alive: Whether every request must negotiate keep-alive.

    Returns:
        Parsed evidence with explicit reasons for any invalid trial.
    """
    reasons = []

    def metric(pattern: str, label: str, integer: bool = False):
        match = re.search(pattern, output, re.MULTILINE)
        if match is None:
            reasons.append(f"Missing metric: {label}")
            return None
        value = int(match[1]) if integer else float(match[1])
        if not math.isfinite(value) or value < 0:
            reasons.append(f"Invalid metric: {label}")
        return value

    number = r"(\d+(?:\.\d+)?)"
    rps = metric(r"^Requests per second:\s+" + number, "throughput")
    tpr = metric(
        r"^Time per request:\s+" + number + r"\s+\[ms\]\s+\(mean\)",
        "mean latency",
    )
    elapsed = metric(r"^Time taken for tests:\s+" + number, "elapsed time")
    complete = metric(r"^Complete requests:\s+(\d+)", "completed", True)
    failed = metric(r"^Failed requests:\s+(\d+)", "failed", True)
    percentiles = [
        metric(rf"^\s*{p}%\s+{number}", f"p{p}") for p in (50, 90, 99)
    ]
    # ab omits this line when every response is 2xx.
    non_2xx_match = re.search(r"^Non-2xx responses:\s+(\d+)", output, re.M)
    non_2xx = int(non_2xx_match[1]) if non_2xx_match else 0
    kept_match = re.search(r"^Keep-Alive requests:\s+(\d+)", output, re.M)
    kept = int(kept_match[1]) if kept_match else None
    if keep_alive and kept != n:
        reasons.append(f"Keep-alive negotiated for {kept} of {n} requests")
    if not keep_alive and kept:
        reasons.append("Unexpected keep-alive in a fresh-connection trial")
    if timed_out:
        reasons.append("Load generator timed out")
    if exit_status != 0:
        reasons.append(f"Load generator exit status: {exit_status}")
    if complete != n:
        reasons.append(f"Completed {complete} of {n} requests")
    if failed:
        reasons.append(f"Transport failures: {failed}")
    if non_2xx:
        reasons.append(f"Non-2xx responses: {non_2xx}")
    if rps is not None and rps <= 0:
        reasons.append("Throughput must be positive")
    if elapsed is not None and elapsed <= 0:
        reasons.append("Elapsed time must be positive")
    if all(p is not None for p in percentiles):
        if percentiles != sorted(percentiles):
            reasons.append("Latency percentiles are out of order")
    return BenchRunResult(
        rps,
        tpr,
        *percentiles,
        failed,
        output,
        exit_status,
        complete,
        non_2xx,
        elapsed,
        n,
        c,
        timed_out,
        reasons,
        keep_alive_requested=keep_alive,
        keep_alive_requests=kept,
    )


def retain_trial(result: BenchRunResult, directory: Path, label: str) -> Path:
    """Save evidence, including rejected runs, before further processing.

    Args:
        result: Parsed trial and its validity reasons.
        directory: Directory for immutable trial records.
        label: Human-readable trial identifier.

    Returns:
        Path to the retained JSON record.
    """
    directory.mkdir(parents=True, exist_ok=True)
    safe_label = re.sub(r"[^A-Za-z0-9_-]", "_", label)[:100]
    with tempfile.NamedTemporaryFile(
        mode="w",
        prefix=safe_label + "-",
        suffix=".json",
        dir=directory,
        delete=False,
    ) as record:
        json.dump({**asdict(result), "valid": result.valid}, record, indent=2)
        record.write("\n")
        return Path(record.name)


def require_valid(result: BenchRunResult) -> None:
    """Raise when a trial cannot support a performance comparison.

    Args:
        result: Trial to validate.

    Returns:
        None if the trial is valid.

    Raises:
        BenchmarkError: When any validation check failed.
    """
    if not result.valid:
        raise BenchmarkError("; ".join(result.invalid_reasons))


def summarize_runs(runs: list[BenchRunResult]) -> MetricSummary:
    """Summarize validated runs, refusing empty or partially failed sets.

    Args:
        runs: Trials contributing to the comparison.

    Returns:
        Throughput and latency statistics for the trials.

    Raises:
        BenchmarkError: If a trial is invalid or the set is empty.
    """
    if not runs:
        raise BenchmarkError("No trials to summarize")
    for run in runs:
        require_valid(run)
    rps = [r.req_per_sec for r in runs]
    return MetricSummary(
        statistics.median(rps),
        min(rps),
        max(rps),
        statistics.median(r.time_per_req_ms for r in runs),
        statistics.median(r.p99_ms for r in runs),
        sum(r.failed_requests for r in runs),
        runs,
    )
