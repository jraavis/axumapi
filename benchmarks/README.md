# Siderite vs. FastAPI Benchmarks

This directory contains the end-to-end HTTP benchmark suite comparing Siderite against FastAPI, matching the methodology described in [`docs/BENCHMARKS.md`](../docs/BENCHMARKS.md).

## Quick Start

### 1. Prerequisites

- **ApacheBench (`ab`)**: Available by default on macOS (`/usr/sbin/ab`) or via `apache2-utils` on Linux.
- **Rust Toolchain**: `cargo` with edition 2024.
- **Python 3.12+**: With dependencies installed:
  ```bash
  pip install -r benchmarks/fastapi/requirements.txt
  ```

### 2. Build Siderite in Release Mode

```bash
cargo build --release -p hello_world -p siderite_todo
```

### 3. Run the Benchmarks

To run the **Plain HTTP** suite (matching `examples/hello_world`):
```bash
python3 benchmarks/run_benchmarks.py --suite plain
```

To run a fast sanity check (lower request counts):
```bash
python3 benchmarks/run_benchmarks.py --suite plain --fast
```

To run the **Database-backed Todo API** suite (e.g. SQLite):
```bash
python3 benchmarks/run_benchmarks.py --suite db --db sqlite
```

To run against PostgreSQL (ensure Docker container is running):
```bash
DATABASE_URL=postgres://siderite:siderite@127.0.0.1:55432/siderite python3 benchmarks/run_benchmarks.py --suite db --db postgres
```

## Workloads

### Plain HTTP Workload
- `GET /`: Root endpoint returning plain text.
- `GET /hello/{name}`: Path parameter extraction and query parameter handling (`shout`).
- `POST /echo`: Request body streaming, JSON deserialization, schema validation, and response serialization.
- Parameters: `-n 20000 -c 100` for GETs, `-n 10000 -c 50` for POST.

### Database-Backed Todo Workload
- `GET /todos`: List latest 20 items.
- `GET /todos/1`: Fetch item by primary key.
- `POST /todos`: Insert new todo item (returns 201).
- Connection pool: 10 connections for both Siderite and FastAPI.
- Data state: Tables truncated and reseeded with 100 items before each phase.
- Parameters: `-n 5000 -c 50` for reads, `-n 2000 -c 20` for inserts.

## Output

The runner executes 3 runs for each test, verifies that there are 0 failed requests, computes the median throughput (req/s), and formats a side-by-side comparison table with the speedup ratio.
