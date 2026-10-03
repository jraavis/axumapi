"""Independent database and HTTP correctness checks outside timed trials."""

import asyncio
import sqlite3
import urllib.parse
from contextlib import closing
from pathlib import Path
from typing import Callable, Optional

from benchmark_metrics import BenchmarkError
from response_checks import TITLE, request_insert


def inspect_database(
    backend: str,
    url: str,
    sqlite_path: Path,
    key: Optional[int | tuple[int, ...]] = None,
):
    """Read committed row count or a sampled row via an independent client.

    Args:
        backend: Database engine name.
        url: Connection URL, never written into trial records.
        sqlite_path: SQLite fixture path.
        key: Primary key, a nonempty tuple of keys, or None to count rows.

    Returns:
        Row count, title/done tuple, or ID-to-values mapping for a batch.
    """
    batch = isinstance(key, tuple)
    if batch and not key:
        return {}
    args = key if batch else (() if key is None else (key,))

    def query(marker):
        if key is None:
            return "SELECT COUNT(*) FROM todos"
        if batch:
            placeholders = ", ".join(marker(i) for i in range(len(args)))
            return (
                "SELECT id, title, done FROM todos "
                f"WHERE id IN ({placeholders})"
            )
        return f"SELECT title, done FROM todos WHERE id = {marker(0)}"

    def shape(row):
        if batch:
            return {item[0]: (item[1], bool(item[2])) for item in row}
        if key is None:
            return row[0]
        return (row[0], bool(row[1])) if row else None

    if backend == "sqlite":
        with closing(sqlite3.connect(sqlite_path)) as conn:
            cursor = conn.execute(query(lambda _: "?"), args)
            return shape(cursor.fetchall() if batch else cursor.fetchone())

    async def read():
        if backend == "postgres":
            import asyncpg

            conn = await asyncpg.connect(
                url, timeout=10, command_timeout=10
            )
            try:
                sql = query(lambda i: f"${i + 1}")
                read = conn.fetch if batch else conn.fetchrow
                return shape(await read(sql, *args))
            finally:
                await conn.close()
        if backend == "mysql":
            import aiomysql

            parts = urllib.parse.urlparse(url)
            conn = await aiomysql.connect(
                host=parts.hostname or "127.0.0.1",
                port=parts.port or 3306,
                user=urllib.parse.unquote(parts.username or "root"),
                password=urllib.parse.unquote(parts.password or ""),
                db=parts.path.lstrip("/"),
                autocommit=True,
                connect_timeout=10,
            )
            try:
                async with conn.cursor() as cursor:
                    await cursor.execute(query(lambda _: "%s"), args)
                    read = cursor.fetchall if batch else cursor.fetchone
                    return shape(await read())
            finally:
                await conn.ensure_closed()
        if backend == "mongodb":
            from motor.motor_asyncio import AsyncIOMotorClient

            client = AsyncIOMotorClient(
                url, serverSelectionTimeoutMS=10000,
                connectTimeoutMS=10000, socketTimeoutMS=10000,
            )
            try:
                collection = client.get_default_database("siderite")["todos"]
                if key is None:
                    return await collection.count_documents({})
                if batch:
                    rows = await collection.find(
                        {"_id": {"$in": list(key)}},
                        {"title": 1, "done": 1},
                    ).to_list(length=len(key))
                    return {
                        row["_id"]: (row["title"], bool(row["done"]))
                        for row in rows
                    }
                row = await collection.find_one({"_id": key})
                return (row["title"], bool(row["done"])) if row else None
            finally:
                client.close()
        raise ValueError(f"Unsupported backend: {backend}")

    return asyncio.run(read())


def preflight_insert(base: str, read_row: Callable) -> None:
    """Require an exact 201 response and verify its committed row.

    Args:
        base: Application HTTP origin.
        read_row: Independent database reader accepting an integer key.

    Returns:
        None after response and database correctness checks pass.

    Raises:
        BenchmarkError: If the HTTP response or persisted values differ.
    """
    response = request_insert(base)
    if response["invalid_reasons"]:
        raise BenchmarkError("; ".join(response["invalid_reasons"]))
    if read_row(response["id"]) != (TITLE, False):
        raise BenchmarkError(
            "Insert preflight row was not committed correctly"
        )
