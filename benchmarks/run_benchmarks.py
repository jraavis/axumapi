#!/usr/bin/env python3
"""Automated benchmark runner for Siderite vs FastAPI.

Matches the benchmark methodology documented in docs/BENCHMARKS.md.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import signal
import subprocess
import sys
import time
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import asdict, dataclass
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

ROOT_DIR = Path(__file__).resolve().parent.parent
BENCHMARKS_DIR = ROOT_DIR / "benchmarks"
FASTAPI_DIR = BENCHMARKS_DIR / "fastapi"
LOGS_DIR = BENCHMARKS_DIR / "logs"
LOGS_DIR.mkdir(parents=True, exist_ok=True)
# One SQLite file for the runner and both servers, whatever their cwd.
SQLITE_DB = ROOT_DIR / "todo_bench.db"


@dataclass
class BenchRunResult:
    req_per_sec: float
    time_per_req_ms: float
    p50_ms: float
    p90_ms: float
    p99_ms: float
    failed_requests: int
    raw_output: str


@dataclass
class MetricSummary:
    median_rps: float
    min_rps: float
    max_rps: float
    median_time_per_req_ms: float
    median_p99_ms: float
    total_failures: int
    runs: List[BenchRunResult]


def check_prerequisites():
    """Verify ab and python dependencies are available."""
    if not os.path.exists("/usr/sbin/ab") and not subprocess.run(["which", "ab"], capture_output=True).returncode == 0:
        print("ERROR: 'ab' (ApacheBench) is not found in PATH or /usr/sbin/ab.", file=sys.stderr)
        sys.exit(1)

    try:
        import fastapi
        import pydantic
        import uvicorn
    except ImportError as e:
        print(f"ERROR: Missing Python dependency: {e}", file=sys.stderr)
        print("Run: pip install -r benchmarks/fastapi/requirements.txt", file=sys.stderr)
        sys.exit(1)


def is_port_in_use(port: int) -> bool:
    import socket

    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        return s.connect_ex(("127.0.0.1", port)) == 0


def wait_until_ready(url: str, timeout: float = 15.0) -> bool:
    start = time.time()
    while time.time() - start < timeout:
        try:
            req = urllib.request.Request(url, headers={"User-Agent": "bench-healthcheck"})
            with urllib.request.urlopen(req, timeout=1.0) as resp:
                if 200 <= resp.status < 300:
                    return True
        except Exception:
            time.sleep(0.1)
    return False


def stop_process(proc: subprocess.Popen, log_file=None):
    if proc.poll() is None:
        try:
            proc.send_signal(signal.SIGINT)
            proc.wait(timeout=3.0)
        except (subprocess.TimeoutExpired, ProcessLookupError):
            try:
                proc.kill()
                proc.wait(timeout=2.0)
            except Exception:
                pass
    if log_file:
        try:
            log_file.close()
        except Exception:
            pass


def run_ab(
    url: str,
    n: int,
    c: int,
    method: str = "GET",
    post_data: Optional[str] = None,
    content_type: str = "application/json",
    keep_alive: bool = False,
) -> BenchRunResult:
    cmd = ["ab", "-l"]
    if keep_alive:
        cmd.append("-k")
    cmd.extend(["-n", str(n), "-c", str(c)])

    temp_post_file = None
    if method == "POST" and post_data is not None:
        import tempfile

        temp_post_file = tempfile.NamedTemporaryFile(mode="w", delete=False)
        temp_post_file.write(post_data)
        temp_post_file.flush()
        temp_post_file.close()
        cmd.extend(["-p", temp_post_file.name, "-T", content_type])

    cmd.append(url)

    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, check=False)
        output = proc.stdout + proc.stderr

        # Parse metrics
        rps_match = re.search(r"Requests per second:\s+([\d.]+)", output)
        rps = float(rps_match.group(1)) if rps_match else 0.0

        tpr_match = re.search(r"Time per request:\s+([\d.]+)\s+\[ms\]\s+\(mean\)", output)
        tpr = float(tpr_match.group(1)) if tpr_match else 0.0

        failed_match = re.search(r"Failed requests:\s+(\d+)", output)
        failed = int(failed_match.group(1)) if failed_match else 0

        p50_match = re.search(r"50%\s+(\d+)", output)
        p50 = float(p50_match.group(1)) if p50_match else 0.0

        p90_match = re.search(r"90%\s+(\d+)", output)
        p90 = float(p90_match.group(1)) if p90_match else 0.0

        p99_match = re.search(r"99%\s+(\d+)", output)
        p99 = float(p99_match.group(1)) if p99_match else 0.0

        return BenchRunResult(
            req_per_sec=rps,
            time_per_req_ms=tpr,
            p50_ms=p50,
            p90_ms=p90,
            p99_ms=p99,
            failed_requests=failed,
            raw_output=output,
        )
    finally:
        if temp_post_file and os.path.exists(temp_post_file.name):
            os.remove(temp_post_file.name)


def summarize_runs(runs: List[BenchRunResult]) -> MetricSummary:
    rps_list = [r.req_per_sec for r in runs]
    tpr_list = [r.time_per_req_ms for r in runs]
    p99_list = [r.p99_ms for r in runs]
    total_failures = sum(r.failed_requests for r in runs)

    import statistics

    median_rps = statistics.median(rps_list)
    min_rps = min(rps_list)
    max_rps = max(rps_list)
    median_tpr = statistics.median(tpr_list)
    median_p99 = statistics.median(p99_list)

    return MetricSummary(
        median_rps=median_rps,
        min_rps=min_rps,
        max_rps=max_rps,
        median_time_per_req_ms=median_tpr,
        median_p99_ms=median_p99,
        total_failures=total_failures,
        runs=runs,
    )


def resolve_db_url(backend: str) -> str:
    if backend == "postgres":
        if os.getenv("DATABASE_URL"):
            return os.environ["DATABASE_URL"]
        for url in [
            "postgres://siderite:siderite@127.0.0.1:55432/siderite",
            "postgres://axumapi:axumapi@127.0.0.1:55432/axumapi",
        ]:
            try:
                import asyncio
                import asyncpg

                async def _test():
                    c = await asyncpg.connect(url)
                    await c.close()

                asyncio.run(_test())
                return url
            except Exception:
                continue
        return "postgres://siderite:siderite@127.0.0.1:55432/siderite"
    elif backend == "mysql":
        if os.getenv("MYSQL_URL"):
            return os.environ["MYSQL_URL"]
        for url in [
            "mysql://root:siderite@127.0.0.1:53306/siderite",
            "mysql://axumapi:axumapi@127.0.0.1:53306/axumapi",
        ]:
            try:
                import asyncio
                import aiomysql

                p = urllib.parse.urlparse(url)

                async def _test():
                    c = await aiomysql.connect(
                        host=p.hostname or "127.0.0.1",
                        port=p.port or 3306,
                        user=p.username or "root",
                        password=p.password or "",
                        db=p.path.lstrip("/") or "siderite",
                    )
                    await c.ensure_closed()

                asyncio.run(_test())
                return url
            except Exception:
                continue
        return "mysql://root:siderite@127.0.0.1:53306/siderite"
    elif backend == "mongodb":
        return os.getenv("MONGODB_URL", "mongodb://127.0.0.1:57017/siderite?directConnection=true")
    else:
        return f"sqlite://{SQLITE_DB}?mode=rwc"


# ---------------------------------------------------------------------------
# Database Seeding
# ---------------------------------------------------------------------------


def reset_sqlite_file(wal: bool):
    """Recreate the SQLite file so the journal mode (stored in it) is known."""
    import sqlite3

    for suffix in ("", "-wal", "-shm", "-journal"):
        Path(f"{SQLITE_DB}{suffix}").unlink(missing_ok=True)
    if wal:
        with sqlite3.connect(SQLITE_DB) as conn:
            conn.execute("PRAGMA journal_mode = WAL")


def seed_database(backend: str):
    """Seed target database with 100 rows."""
    print(f"  -> Seeding 100 rows into database ({backend})...")
    if backend == "sqlite":
        import sqlite3

        with sqlite3.connect(SQLITE_DB) as conn:
            cur = conn.cursor()
            cur.execute("DROP TABLE IF EXISTS todos")
            cur.execute(
                """
                CREATE TABLE todos (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    done INTEGER NOT NULL DEFAULT 0
                )
            """
            )
            for i in range(1, 101):
                cur.execute(
                    "INSERT INTO todos (title, done) VALUES (?, ?)",
                    (f"Todo task #{i}", 1 if i % 2 == 0 else 0),
                )
            conn.commit()

    elif backend == "postgres":
        import asyncio
        import asyncpg

        pg_url = resolve_db_url("postgres")

        async def _seed_pg():
            conn = await asyncpg.connect(pg_url)
            try:
                await conn.execute("DROP TABLE IF EXISTS todos")
                await conn.execute(
                    """
                    CREATE TABLE todos (
                        id BIGSERIAL PRIMARY KEY,
                        title VARCHAR(280) NOT NULL,
                        done BOOLEAN NOT NULL DEFAULT FALSE
                    )
                """
                )
                for i in range(1, 101):
                    await conn.execute(
                        "INSERT INTO todos (id, title, done) VALUES ($1, $2, $3)",
                        i,
                        f"Todo task #{i}",
                        (i % 2 == 0),
                    )
                # Reset sequence
                await conn.execute("SELECT setval('todos_id_seq', 100)")
            finally:
                await conn.close()

        asyncio.run(_seed_pg())

    elif backend == "mysql":
        import asyncio
        import aiomysql

        mysql_url = resolve_db_url("mysql")
        url_parts = urllib.parse.urlparse(mysql_url)
        host = url_parts.hostname or "127.0.0.1"
        port = url_parts.port or 53306
        user = url_parts.username or "root"
        password = url_parts.password or ""
        db = url_parts.path.lstrip("/") or "siderite"

        async def _seed_mysql():
            conn = await aiomysql.connect(
                host=host, port=port, user=user, password=password, db=db, autocommit=True
            )
            try:
                async with conn.cursor() as cur:
                    await cur.execute("DROP TABLE IF EXISTS todos")
                    await cur.execute(
                        """
                        CREATE TABLE todos (
                            id BIGINT PRIMARY KEY AUTO_INCREMENT,
                            title VARCHAR(280) NOT NULL,
                            done BOOLEAN NOT NULL DEFAULT FALSE
                        )
                    """
                    )
                    for i in range(1, 101):
                        await cur.execute(
                            "INSERT INTO todos (id, title, done) VALUES (%s, %s, %s)",
                            (i, f"Todo task #{i}", (i % 2 == 0)),
                        )
            finally:
                conn.close()

        asyncio.run(_seed_mysql())

    elif backend == "mongodb":
        import asyncio
        from motor.motor_asyncio import AsyncIOMotorClient

        mongo_url = os.getenv(
            "MONGODB_URL",
            "mongodb://127.0.0.1:57017/siderite?directConnection=true",
        )

        async def _seed_mongo():
            client = AsyncIOMotorClient(mongo_url)
            db = client["siderite"]
            try:
                await db["todos"].drop()
                await db["siderite_counters"].delete_many({})
                docs = [
                    {"_id": i, "title": f"Todo task #{i}", "done": (i % 2 == 0)}
                    for i in range(1, 101)
                ]
                await db["todos"].insert_many(docs)
                await db["siderite_counters"].insert_one({"_id": "todos", "seq": 100})
            finally:
                client.close()

        asyncio.run(_seed_mongo())


# ---------------------------------------------------------------------------
# Benchmark Runners
# ---------------------------------------------------------------------------


def run_benchmark_set(
    name: str,
    siderite_url_base: str,
    fastapi_url_base: str,
    path: str,
    n: int,
    c: int,
    method: str = "GET",
    post_data: Optional[str] = None,
    num_runs: int = 3,
    keep_alive: bool = False,
    reseed_callback=None,
) -> Tuple[MetricSummary, MetricSummary]:
    print(f"\n=======================================================")
    print(f"Benchmark: {name} ({method} {path})")
    print(f"Parameters: -n {n} -c {c}, repetitions={num_runs}")
    print(f"=======================================================")

    # Warmup
    print("  -> Warming up Siderite...")
    if reseed_callback:
        reseed_callback()
    run_ab(f"{siderite_url_base}{path}", min(n // 10, 500), min(c, 20), method, post_data, keep_alive=keep_alive)

    siderite_runs = []
    for r in range(1, num_runs + 1):
        if reseed_callback:
            reseed_callback()
        print(f"  -> Siderite Run {r}/{num_runs}...", end="", flush=True)
        res = run_ab(f"{siderite_url_base}{path}", n, c, method, post_data, keep_alive=keep_alive)
        print(f" {res.req_per_sec:,.0f} req/s (failures: {res.failed_requests})")
        siderite_runs.append(res)

    print("  -> Warming up FastAPI...")
    if reseed_callback:
        reseed_callback()
    run_ab(f"{fastapi_url_base}{path}", min(n // 10, 500), min(c, 20), method, post_data, keep_alive=keep_alive)

    fastapi_runs = []
    for r in range(1, num_runs + 1):
        if reseed_callback:
            reseed_callback()
        print(f"  -> FastAPI Run {r}/{num_runs}...", end="", flush=True)
        res = run_ab(f"{fastapi_url_base}{path}", n, c, method, post_data, keep_alive=keep_alive)
        print(f" {res.req_per_sec:,.0f} req/s (failures: {res.failed_requests})")
        fastapi_runs.append(res)

    siderite_summary = summarize_runs(siderite_runs)
    fastapi_summary = summarize_runs(fastapi_runs)

    ratio = siderite_summary.median_rps / (fastapi_summary.median_rps or 1.0)
    print(f"==> Result: Siderite = {siderite_summary.median_rps:,.0f} req/s | FastAPI = {fastapi_summary.median_rps:,.0f} req/s | Speedup: {ratio:.2f}x\n")

    return siderite_summary, fastapi_summary


def run_plain_suite(
    siderite_port: int,
    fastapi_port: int,
    num_runs: int = 3,
    fast: bool = False,
    keep_alive: bool = False,
) -> Dict[str, Dict[str, Any]]:
    print("\n>>> STARTING PLAIN HTTP BENCHMARK SUITE <<<")

    # Request counts
    n_get = 2000 if fast else 20000
    c_get = 50 if fast else 100
    n_post = 1000 if fast else 10000
    c_post = 25 if fast else 50

    # Start Siderite server (hello_world)
    siderite_bin = ROOT_DIR / "target" / "release" / "hello_world"
    if not siderite_bin.exists():
        print(f"ERROR: Siderite binary {siderite_bin} not found. Run cargo build --release -p hello_world first.")
        sys.exit(1)

    s_log = open(LOGS_DIR / "bench_siderite_plain.log", "w")
    s_proc = subprocess.Popen(
        [str(siderite_bin), "run", "--addr", f"127.0.0.1:{siderite_port}"],
        cwd=str(ROOT_DIR),
        stdout=s_log,
        stderr=s_log,
    )
    s_url = f"http://127.0.0.1:{siderite_port}"
    if not wait_until_ready(f"{s_url}/"):
        print(f"ERROR: Siderite plain server failed to start. See {LOGS_DIR / 'bench_siderite_plain.log'}", file=sys.stderr)
        stop_process(s_proc, s_log)
        sys.exit(1)

    # Start FastAPI server (plain_app)
    f_log = open(LOGS_DIR / "bench_fastapi_plain.log", "w")
    f_proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "uvicorn",
            "plain_app:app",
            "--host",
            "127.0.0.1",
            "--port",
            str(fastapi_port),
            "--workers",
            "1",
            "--log-level",
            "warning",
            "--no-access-log",
        ],
        cwd=str(FASTAPI_DIR),
        stdout=f_log,
        stderr=f_log,
    )
    f_url = f"http://127.0.0.1:{fastapi_port}"
    if not wait_until_ready(f"{f_url}/"):
        print(f"ERROR: FastAPI plain server failed to start. See {LOGS_DIR / 'bench_fastapi_plain.log'}", file=sys.stderr)
        stop_process(s_proc, s_log)
        stop_process(f_proc, f_log)
        sys.exit(1)

    results = {}
    try:
        # Test 1: GET /
        s_res, f_res = run_benchmark_set(
            "Plain GET /",
            s_url,
            f_url,
            "/",
            n=n_get,
            c=c_get,
            num_runs=num_runs,
            keep_alive=keep_alive,
        )
        results["GET /"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

        # Test 2: GET /hello/world
        s_res, f_res = run_benchmark_set(
            "Path Parameter GET /hello/world",
            s_url,
            f_url,
            "/hello/world",
            n=n_get,
            c=c_get,
            num_runs=num_runs,
            keep_alive=keep_alive,
        )
        results["GET /hello/{name}"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

        # Test 3: POST /echo
        s_res, f_res = run_benchmark_set(
            "JSON Validation POST /echo",
            s_url,
            f_url,
            "/echo",
            n=n_post,
            c=c_post,
            method="POST",
            post_data='{"message":"benchmark test payload"}',
            num_runs=num_runs,
            keep_alive=keep_alive,
        )
        results["POST /echo"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

    finally:
        stop_process(s_proc, s_log)
        stop_process(f_proc, f_log)

    return results


def run_todo_suite(
    backend: str,
    siderite_port: int,
    fastapi_port: int,
    num_runs: int = 3,
    fast: bool = False,
    keep_alive: bool = False,
    sqlite_wal: bool = False,
    pool_size: int = 10,
) -> Dict[str, Dict[str, Any]]:
    print(f"\n>>> STARTING TODO API BENCHMARK SUITE (Backend: {backend}) <<<")

    n_read = 1000 if fast else 5000
    c_read = 25 if fast else 50
    # Long enough (seconds, not a fraction of one) for a stable median.
    n_write = 500 if fast else 10000
    c_write = 10 if fast else 20

    if backend == "sqlite":
        reset_sqlite_file(sqlite_wal)
    seed_database(backend)

    # Determine connection string
    env = os.environ.copy()
    db_url = resolve_db_url(backend)
    env["DATABASE_URL"] = db_url
    env["DATABASE_POOL_SIZE"] = str(pool_size)
    if backend == "mysql":
        env["MYSQL_URL"] = db_url
    elif backend == "mongodb":
        env["MONGODB_URL"] = db_url
    elif backend == "sqlite":
        env["SQLITE_PATH"] = str(SQLITE_DB)
        env["SQLITE_WAL"] = "1" if sqlite_wal else "0"

    # Start Siderite server (siderite_todo)
    siderite_bin = ROOT_DIR / "target" / "release" / "siderite_todo"
    if not siderite_bin.exists():
        print(f"ERROR: Siderite binary {siderite_bin} not found. Run cargo build --release -p siderite_todo first.")
        sys.exit(1)

    s_log = open(LOGS_DIR / f"bench_siderite_todo_{backend}.log", "w")
    s_proc = subprocess.Popen(
        [str(siderite_bin), "run", "--addr", f"127.0.0.1:{siderite_port}"],
        cwd=str(ROOT_DIR),
        env=env,
        stdout=s_log,
        stderr=s_log,
    )
    s_url = f"http://127.0.0.1:{siderite_port}"
    if not wait_until_ready(f"{s_url}/health"):
        print(f"ERROR: Siderite todo server failed to start. See {LOGS_DIR / f'bench_siderite_todo_{backend}.log'}", file=sys.stderr)
        stop_process(s_proc, s_log)
        sys.exit(1)

    # Start FastAPI server (todo_app)
    f_env = env.copy()
    f_env["DB_BACKEND"] = backend
    f_log = open(LOGS_DIR / f"bench_fastapi_todo_{backend}.log", "w")
    f_proc = subprocess.Popen(
        [
            sys.executable,
            "-m",
            "uvicorn",
            "todo_app:app",
            "--host",
            "127.0.0.1",
            "--port",
            str(fastapi_port),
            "--workers",
            "1",
            "--log-level",
            "warning",
            "--no-access-log",
        ],
        cwd=str(FASTAPI_DIR),
        env=f_env,
        stdout=f_log,
        stderr=f_log,
    )
    f_url = f"http://127.0.0.1:{fastapi_port}"
    if not wait_until_ready(f"{f_url}/health"):
        print(f"ERROR: FastAPI todo server failed to start. See {LOGS_DIR / f'bench_fastapi_todo_{backend}.log'}", file=sys.stderr)
        stop_process(s_proc, s_log)
        stop_process(f_proc, f_log)
        sys.exit(1)

    results = {}
    try:
        # Test 1: GET /todos (list 20)
        s_res, f_res = run_benchmark_set(
            f"{backend.capitalize()} GET /todos (list 20)",
            s_url,
            f_url,
            "/todos",
            n=n_read,
            c=c_read,
            num_runs=num_runs,
            keep_alive=keep_alive,
        )
        results[f"{backend}: list 20"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

        # Test 2: GET /todos/1 (get one)
        s_res, f_res = run_benchmark_set(
            f"{backend.capitalize()} GET /todos/1 (get one)",
            s_url,
            f_url,
            "/todos/1",
            n=n_read,
            c=c_read,
            num_runs=num_runs,
            keep_alive=keep_alive,
        )
        results[f"{backend}: get one"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

        # Test 3: POST /todos (insert)
        s_res, f_res = run_benchmark_set(
            f"{backend.capitalize()} POST /todos (insert)",
            s_url,
            f_url,
            "/todos",
            n=n_write,
            c=c_write,
            method="POST",
            post_data='{"title":"Benchmark new task"}',
            num_runs=num_runs,
            keep_alive=keep_alive,
            reseed_callback=lambda: seed_database(backend),
        )
        results[f"{backend}: insert"] = {"siderite": asdict(s_res), "fastapi": asdict(f_res)}

    finally:
        stop_process(s_proc, s_log)
        stop_process(f_proc, f_log)

    return results


def print_markdown_summary(plain_results: Dict[str, Any], todo_results: Dict[str, Any]):
    print("\n" + "=" * 60)
    print("                BENCHMARK RESULTS SUMMARY")
    print("=" * 60 + "\n")

    if plain_results:
        print("### Plain HTTP (`examples/hello_world` vs FastAPI)\n")
        print("| Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Latency p99 (S / F) |")
        print("|---|---|---|---|---|")
        for test, data in plain_results.items():
            s_rps = data["siderite"]["median_rps"]
            f_rps = data["fastapi"]["median_rps"]
            s_p99 = data["siderite"]["median_p99_ms"]
            f_p99 = data["fastapi"]["median_p99_ms"]
            ratio = s_rps / (f_rps or 1.0)
            print(f"| `{test}` | {s_rps:,.0f} | {f_rps:,.0f} | **{ratio:.2f}x** | {s_p99:.0f} ms / {f_p99:.0f} ms |")
        print()

    if todo_results:
        print("### Database-backed Todo API (Siderite ORM vs FastAPI)\n")
        print("| Backend | Test | Siderite (req/s) | FastAPI (req/s) | Speedup | Min-max (S / F) |")
        print("|---|---|---|---|---|---|")
        for test_key, data in todo_results.items():
            backend, test_name = test_key.split(":", 1)
            s_rps = data["siderite"]["median_rps"]
            f_rps = data["fastapi"]["median_rps"]
            ratio = s_rps / (f_rps or 1.0)
            spread = " / ".join(
                f"{data[side]['min_rps']:,.0f}-{data[side]['max_rps']:,.0f}" for side in ("siderite", "fastapi")
            )
            print(f"| {backend.strip().capitalize()} | {test_name.strip()} | {s_rps:,.0f} | {f_rps:,.0f} | **{ratio:.2f}x** | {spread} |")
        print()


def main():
    parser = argparse.ArgumentParser(description="Benchmark Siderite against FastAPI")
    parser.add_argument(
        "--suite",
        choices=["plain", "db", "all"],
        default="plain",
        help="Which benchmark suite to run (plain, db, or all)",
    )
    parser.add_argument(
        "--db",
        choices=["sqlite", "postgres", "mysql", "mongodb", "all"],
        default="sqlite",
        help="Database backend for todo suite",
    )
    parser.add_argument("--runs", type=int, default=3, help="Repetitions per benchmark (default: 3)")
    parser.add_argument("--siderite-port", type=int, default=8081, help="Port for Siderite server")
    parser.add_argument("--fastapi-port", type=int, default=8082, help="Port for FastAPI server")
    parser.add_argument("--fast", action="store_true", help="Quick run with reduced request counts")
    parser.add_argument("--keep-alive", action="store_true", help="Enable HTTP keep-alive (-k) in ab")
    parser.add_argument(
        "--sqlite-wal",
        action="store_true",
        help="SQLite: journal_mode=WAL and synchronous=NORMAL in both apps (default: SQLite defaults)",
    )
    parser.add_argument(
        "--pool-size",
        type=int,
        default=10,
        help="PostgreSQL / MySQL connection pool size in both apps (default: 10)",
    )
    parser.add_argument("--json", action="store_true", help="Print JSON result at end")
    parser.add_argument("--output", type=str, default="", help="File to write markdown report to")
    args = parser.parse_args()

    check_prerequisites()

    if is_port_in_use(args.siderite_port):
        print(f"ERROR: Port {args.siderite_port} is already in use.", file=sys.stderr)
        sys.exit(1)
    if is_port_in_use(args.fastapi_port):
        print(f"ERROR: Port {args.fastapi_port} is already in use.", file=sys.stderr)
        sys.exit(1)

    all_plain_results = {}
    all_todo_results = {}

    if args.suite in ("plain", "all"):
        all_plain_results = run_plain_suite(
            siderite_port=args.siderite_port,
            fastapi_port=args.fastapi_port,
            num_runs=args.runs,
            fast=args.fast,
            keep_alive=args.keep_alive,
        )

    if args.suite in ("db", "all"):
        backends = ["sqlite", "postgres", "mysql", "mongodb"] if args.db == "all" else [args.db]
        for b in backends:
            res = run_todo_suite(
                backend=b,
                siderite_port=args.siderite_port,
                fastapi_port=args.fastapi_port,
                num_runs=args.runs,
                fast=args.fast,
                keep_alive=args.keep_alive,
                sqlite_wal=args.sqlite_wal,
                pool_size=args.pool_size,
            )
            all_todo_results.update(res)

    print_markdown_summary(all_plain_results, all_todo_results)

    if args.output:
        import io

        buf = io.StringIO()
        old_stdout = sys.stdout
        sys.stdout = buf
        print_markdown_summary(all_plain_results, all_todo_results)
        sys.stdout = old_stdout
        with open(args.output, "w") as f:
            f.write(buf.getvalue())
        print(f"Report written to {args.output}")

    if args.json:
        combined = {"plain": all_plain_results, "todo": all_todo_results}
        print(json.dumps(combined, indent=2))


if __name__ == "__main__":
    main()
