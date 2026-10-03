"""Isolated application lifecycle for each benchmark trial."""

import json
import subprocess
import time
import urllib.error
import urllib.request
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

from benchmark_metrics import BenchmarkError


@dataclass
class ServerSpec:
    """Process inputs and readiness endpoint for a benchmark application."""

    command: list[str]
    cwd: Path
    base_url: str
    health_path: str
    log_path: Path
    env: Optional[dict[str, str]] = None
    expected_workers: Optional[int] = None


@contextmanager
def running_server(spec: ServerSpec):
    """Start one application and always drain or terminate it after a trial.

    Args:
        spec: Application command, environment, readiness and log settings.

    Yields:
        Observed runtime metadata after readiness, or None without a policy.

    Raises:
        BenchmarkError: If the application exits or never becomes ready.
    """
    import signal

    spec.log_path.parent.mkdir(parents=True, exist_ok=True)
    # Preserve logs from earlier trials rather than truncating their evidence.
    with spec.log_path.open("a") as log:
        log.write(f"\n--- trial process started {time.time()} ---\n")
        log.flush()
        offset = log.tell()
        process = subprocess.Popen(
            spec.command,
            cwd=spec.cwd,
            env=spec.env,
            stdout=log,
            stderr=log,
        )
        try:
            deadline = time.monotonic() + 30
            ready = False
            while time.monotonic() < deadline and process.poll() is None:
                try:
                    with urllib.request.urlopen(
                        spec.base_url + spec.health_path, timeout=1
                    ) as response:
                        ready = 200 <= response.status < 300
                    if ready:
                        break
                except (urllib.error.URLError, TimeoutError):
                    pass
                time.sleep(0.1)
            if not ready:
                raise BenchmarkError(
                    f"Application failed readiness; see {spec.log_path}"
                )
            runtime = None
            if spec.expected_workers is not None:
                runtime = read_runtime(spec, process.pid, offset)
            yield runtime
            if process.poll() is not None:
                raise BenchmarkError(
                    f"Application exited during trial; see {spec.log_path}"
                )
        finally:
            if process.poll() is None:
                process.send_signal(signal.SIGINT)
                try:
                    process.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def read_runtime(spec, pid, offset):
    """Validate and retain the startup worker report for this process.

    Args:
        spec: Process specification with an explicit worker expectation.
        pid: PID of the directly launched single application process.
        offset: Start of this process's log segment.

    Returns:
        Sanitized actual scheduler count and PID, without environment data.

    Raises:
        BenchmarkError: Missing, malformed or mismatched runtime evidence.
    """
    with spec.log_path.open("rb") as log:
        log.seek(offset)
        segment = log.read(65536).decode("utf-8", errors="replace")
    for line in segment.splitlines():
        if not line.startswith("BENCHMARK_RUNTIME "):
            continue
        try:
            report = json.loads(line.removeprefix("BENCHMARK_RUNTIME "))
            workers = report["scheduler_workers"]
            if (type(workers) is not int or workers != spec.expected_workers
                    or report["pid"] != pid):
                raise ValueError("Mismatched runtime")
        except (ValueError, TypeError, KeyError) as error:
            raise BenchmarkError("Invalid process runtime evidence") from error
        settings = record_database_settings(spec, pid, segment)
        observation = {"scheduler_workers": workers, "pid": pid}
        with (spec.log_path.parent / "runtime.jsonl").open("a") as record:
            record.write(json.dumps({"application": spec.log_path.name,
                                     **observation}) + "\n")
        if settings is not None:
            observation["database_settings"] = settings
        return observation
    raise BenchmarkError(f"Missing runtime evidence; see {spec.log_path}")


DATABASE_SETTING_KEYS = frozenset({
    "version", "journal_mode", "synchronous", "foreign_keys", "busy_timeout",
    "synchronous_commit", "fsync", "full_page_writes", "timezone",
    "autocommit", "sql_mode", "innodb_flush_log_at_trx_commit", "sync_binlog",
    "read_preference", "client_read_concern", "client_write_concern",
    "server_default_read_concern", "server_default_write_concern",
    "majority_journal_default", "max_pool_size", "min_pool_size",
    "max_connecting", "retry_reads", "retry_writes",
})


def record_database_settings(spec, pid, segment):
    """Retain only fixed, non-secret effective database settings.

    Args:
        spec: Owned application's log and artifact directory.
        pid: Expected process identity.
        segment: Startup output from only this process.

    Returns:
        Recorded settings, or None when this is a non-SQL process.

    Raises:
        BenchmarkError: Malformed, duplicated or mismatched setting report.
    """
    reports = [line.removeprefix("BENCHMARK_DATABASE ")
               for line in segment.splitlines()
               if line.startswith("BENCHMARK_DATABASE ")]
    if not reports:
        return
    try:
        if len(reports) != 1:
            raise ValueError("Duplicate settings report")
        report = json.loads(reports[0])
        settings = report["settings"]
        if (type(report["pid"]) is not int or report["pid"] != pid
                or not isinstance(settings, dict) or not settings
                or not set(settings) <= DATABASE_SETTING_KEYS
                or any(type(value) not in (int, str) or
                       (isinstance(value, str) and len(value) > 4096)
                       for value in settings.values())):
            raise ValueError("Invalid database settings")
    except (ValueError, TypeError, KeyError) as error:
        raise BenchmarkError("Invalid database settings evidence") from error
    observation = {"application": spec.log_path.name, "pid": pid,
                   "settings": settings, "scope": "sampled database settings"}
    path = spec.log_path.parent / "database-settings.jsonl"
    with path.open("a") as record:
        record.write(json.dumps(observation) + "\n")
    return settings
