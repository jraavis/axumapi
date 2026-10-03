"""Untimed concurrent HTTP correctness evidence for insert benchmarks."""

import json
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor
from typing import Callable

from benchmark_metrics import BenchmarkError

TITLE = "Benchmark new task"


def request_insert(base: str) -> dict:
    """Check an insert response without retaining arbitrary server content.

    Args:
        base: Application HTTP origin.

    Returns:
        Status, returned ID and correctness reasons for one request.
    """
    evidence = {"status": None, "id": None, "invalid_reasons": []}
    request = urllib.request.Request(
        f"{base}/todos",
        data=json.dumps({"title": TITLE}).encode(),
        headers={"Content-Type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=10) as response:
            evidence["status"] = response.status
            body = response.read(65537)
        if len(body) > 65536:
            raise ValueError("Response exceeded the correctness-check limit")
        payload = json.loads(body)
        if not isinstance(payload, dict):
            raise ValueError("Response must be an object")
        key = payload.get("id")
        if type(key) is not int or key <= 0:
            evidence["invalid_reasons"].append("Invalid returned ID")
        else:
            evidence["id"] = key
        if payload.get("title") != TITLE or payload.get("done") is not False:
            evidence["invalid_reasons"].append("Incorrect returned values")
    except urllib.error.HTTPError as error:
        evidence["status"] = error.code
        error.close()
        evidence["invalid_reasons"].append("HTTP request rejected")
    except Exception as error:
        # Error messages may contain connection URLs or arbitrary server text.
        evidence["invalid_reasons"].append(type(error).__name__)
    if evidence["status"] != 201:
        evidence["invalid_reasons"].append("Expected exactly HTTP 201")
    return evidence


def check_insert_responses(
    base: str, read_rows: Callable, concurrency: int
) -> dict:
    """Check a bounded concurrent batch outside the measured workload.

    Args:
        base: Application HTTP origin.
        read_rows: Independent reader returning an ID-to-values mapping.
        concurrency: Load concurrency; at most 100 sample workers are used.

    Returns:
        Separate untimed response and committed-row evidence, including errors.
    """
    count = min(concurrency, 100)
    if count < 1:
        raise BenchmarkError("Correctness concurrency must be positive")
    with ThreadPoolExecutor(max_workers=count) as workers:
        responses = list(workers.map(request_insert, [base] * count))
    reasons = []
    if any(response["invalid_reasons"] for response in responses):
        reasons.append("Insert response correctness check failed")
    keys = [response["id"] for response in responses]
    valid_keys = tuple(key for key in keys if key is not None)
    if len(set(valid_keys)) != len(valid_keys):
        reasons.append("Duplicate returned IDs in correctness batch")
    verified = 0
    if valid_keys:
        try:
            rows = read_rows(valid_keys)
            verified = sum(rows.get(key) == (TITLE, False) for key in keys)
            if verified != count:
                reasons.append("Correctness batch rows differ or are missing")
        except Exception as error:
            reasons.append(f"Independent row check: {type(error).__name__}")
    return {
        "phase": "after_timed_trial",
        "measured_responses_checked": False,
        "requested_samples": count,
        "concurrency": count,
        "verified_rows": verified,
        "responses": responses,
        "invalid_reasons": reasons,
        "valid": not reasons,
    }
