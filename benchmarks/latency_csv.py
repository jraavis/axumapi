"""Retain ApacheBench's microsecond-resolution percentile measurements."""

import csv
import io
import math
import re

from benchmark_metrics import BenchmarkError


def apply_percentiles(result, content):
    """Validate the full CSV distribution before replacing rounded tails.

    Args:
        result: Parsed trial whose integer console percentiles are retained.
        content: ApacheBench -e output with all percentiles from 0 to 100.

    Returns:
        None after attaching precise p50/p90/p99 and rounding resolution.

    Raises:
        BenchmarkError: Missing, malformed or nonmonotonic percentiles.
    """
    rows = list(csv.reader(io.StringIO(content)))
    if (len(rows) != 102
            or rows[0] != ["Percentage served", "Time in ms"]):
        raise BenchmarkError("Missing complete latency CSV distribution")
    values = []
    for index, row in enumerate(rows[1:]):
        if len(row) != 2 or row[0] != str(index):
            raise BenchmarkError("Invalid latency CSV percentile sequence")
        if not re.fullmatch(r"\d+\.\d{3}", row[1]):
            raise BenchmarkError("Unexpected latency CSV timing resolution")
        try:
            value = float(row[1])
        except ValueError as error:
            raise BenchmarkError("Invalid latency CSV timing") from error
        if (not math.isfinite(value) or value < 0
                or (values and value < values[-1])):
            raise BenchmarkError("Invalid latency CSV timing order")
        values.append(value)
    result.p50_ms, result.p90_ms, result.p99_ms = (
        values[50], values[90], values[99]
    )
    result.latency_resolution_ms = 0.001
