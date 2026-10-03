"""Schema creation and schema-preserving data resets for benchmark fixtures."""

import os
import urllib.parse
from contextlib import closing
from pathlib import Path

ROOT_DIR = Path(__file__).resolve().parent.parent
SQLITE_DB = ROOT_DIR / "todo_bench.db"


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
                        user=urllib.parse.unquote(p.username or "root"),
                        password=urllib.parse.unquote(p.password or ""),
                        db=p.path.lstrip("/") or "siderite",
                    )
                    await c.ensure_closed()

                asyncio.run(_test())
                return url
            except Exception:
                continue
        return "mysql://root:siderite@127.0.0.1:53306/siderite"
    elif backend == "mongodb":
        return os.getenv(
            "MONGODB_URL",
            "mongodb://127.0.0.1:57017/siderite?directConnection=true",
        )
    else:
        return f"sqlite://{SQLITE_DB}?mode=rwc"


# ---------------------------------------------------------------------------
# Database Seeding
# ---------------------------------------------------------------------------


def reset_sqlite_file(wal: bool, sqlite_path=None):
    """Recreate the SQLite file so the journal mode (stored in it) is known."""
    import sqlite3

    database = sqlite_path or SQLITE_DB
    for suffix in ("", "-wal", "-shm", "-journal"):
        Path(f"{database}{suffix}").unlink(missing_ok=True)
    if wal:
        with closing(sqlite3.connect(database)) as conn:
            conn.execute("PRAGMA journal_mode = WAL")


def seed_database(backend: str, recreate: bool = False, url=None,
                  sqlite_path=None):
    """Seed target database with 100 rows."""
    print(f"  -> Seeding 100 rows into database ({backend})...")
    if backend == "sqlite":
        import sqlite3

        with closing(sqlite3.connect(sqlite_path or SQLITE_DB)) as conn:
            cur = conn.cursor()
            if recreate:
                cur.execute("DROP TABLE IF EXISTS todos")
            cur.execute(
                """
                CREATE TABLE IF NOT EXISTS todos (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    title TEXT NOT NULL,
                    done INTEGER NOT NULL DEFAULT 0
                )
            """
            )
            cur.execute("DELETE FROM todos")
            cur.execute("DELETE FROM sqlite_sequence WHERE name = 'todos'")
            for i in range(1, 101):
                cur.execute(
                    "INSERT INTO todos (title, done) VALUES (?, ?)",
                    (f"Todo task #{i}", 1 if i % 2 == 0 else 0),
                )
            conn.commit()

    elif backend == "postgres":
        import asyncio
        import asyncpg

        pg_url = url or resolve_db_url("postgres")

        async def _seed_pg():
            conn = await asyncpg.connect(pg_url)
            try:
                if recreate:
                    await conn.execute("DROP TABLE IF EXISTS todos")
                await conn.execute(
                    """
                    CREATE TABLE IF NOT EXISTS todos (
                        id BIGSERIAL PRIMARY KEY,
                        title VARCHAR(280) NOT NULL,
                        done BOOLEAN NOT NULL DEFAULT FALSE
                    )
                """
                )
                await conn.execute("DELETE FROM todos")
                for i in range(1, 101):
                    await conn.execute(
                        "INSERT INTO todos (id, title, done) "
                        "VALUES ($1, $2, $3)",
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

        mysql_url = url or resolve_db_url("mysql")
        url_parts = urllib.parse.urlparse(mysql_url)
        host = url_parts.hostname or "127.0.0.1"
        port = url_parts.port or 53306
        user = urllib.parse.unquote(url_parts.username or "root")
        password = urllib.parse.unquote(url_parts.password or "")
        db = url_parts.path.lstrip("/") or "siderite"

        async def _seed_mysql():
            conn = await aiomysql.connect(
                host=host,
                port=port,
                user=user,
                password=password,
                db=db,
                autocommit=True,
            )
            try:
                async with conn.cursor() as cur:
                    if recreate:
                        await cur.execute("DROP TABLE IF EXISTS todos")
                    await cur.execute(
                        """
                        CREATE TABLE IF NOT EXISTS todos (
                            id BIGINT PRIMARY KEY AUTO_INCREMENT,
                            title VARCHAR(280) NOT NULL,
                            done BOOLEAN NOT NULL DEFAULT FALSE
                        )
                    """
                    )
                    await cur.execute("DELETE FROM todos")
                    for i in range(1, 101):
                        await cur.execute(
                            "INSERT INTO todos (id, title, done) "
                            "VALUES (%s, %s, %s)",
                            (i, f"Todo task #{i}", (i % 2 == 0)),
                        )
            finally:
                conn.close()

        asyncio.run(_seed_mysql())

    elif backend == "mongodb":
        import asyncio
        from motor.motor_asyncio import AsyncIOMotorClient

        mongo_url = url or os.getenv(
            "MONGODB_URL",
            "mongodb://127.0.0.1:57017/siderite?directConnection=true",
        )

        async def _seed_mongo():
            client = AsyncIOMotorClient(mongo_url)
            db = client.get_default_database("siderite")
            try:
                if recreate:
                    await db["todos"].drop()
                else:
                    await db["todos"].delete_many({})
                await db["siderite_counters"].delete_many({})
                docs = [
                    {
                        "_id": i,
                        "title": f"Todo task #{i}",
                        "done": (i % 2 == 0),
                    }
                    for i in range(1, 101)
                ]
                await db["todos"].insert_many(docs)
                await db["siderite_counters"].insert_one(
                    {"_id": "todos", "seq": 100}
                )
            finally:
                client.close()

        asyncio.run(_seed_mongo())
