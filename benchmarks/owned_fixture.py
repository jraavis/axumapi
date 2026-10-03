"""CLI benchmark fixtures own their namespace and always remove it."""

import asyncio
import tempfile
import urllib.parse
import uuid
from contextlib import contextmanager
from pathlib import Path


@contextmanager
def owned_fixture(backend, url):
    """Create a private benchmark database instead of resetting caller data.

    Args:
        backend: SQLite, PostgreSQL, MySQL or MongoDB adapter name.
        url: Configured administrative connection, never printed or retained.

    Yields:
        Private connection URL and SQLite file path (None for server engines).

    Raises:
        RuntimeError: Missing database creation rights or cleanup failure.
        Driver errors are wrapped without exposing URL credentials.
    """
    if backend == "sqlite":
        with tempfile.TemporaryDirectory(prefix="siderite-benchmark-") as path:
            database = Path(path) / "owned.db"
            yield f"sqlite://{database}?mode=rwc", database
        return
    name = f"siderite_bench_{uuid.uuid4().hex}"
    parts = urllib.parse.urlsplit(url)
    private_url = urllib.parse.urlunsplit(parts._replace(path=f"/{name}"))
    if backend == "mongodb" and parts.username:
        query = urllib.parse.parse_qsl(parts.query, keep_blank_values=True)
        if not any(key == "authSource" for key, _ in query):
            query.append(("authSource", parts.path.lstrip("/") or "admin"))
            private_url = urllib.parse.urlunsplit(parts._replace(
                path=f"/{name}", query=urllib.parse.urlencode(query),
            ))
    try:
        asyncio.run(change_database(backend, url, name, create=True))
    except Exception as error:
        cleanup = ""
        try:
            asyncio.run(change_database(backend, url, name, create=False))
        except Exception as failure:
            cleanup = (f"; cleanup failed ({type(failure).__name__}), "
                       f"retained namespace {name}")
        raise RuntimeError(
            f"Cannot create owned {backend} fixture: "
            f"{type(error).__name__}{cleanup}"
        ) from None
    primary = None
    try:
        yield private_url, None
    except BaseException as error:
        primary = error
        raise
    finally:
        try:
            asyncio.run(change_database(backend, url, name, create=False))
        except Exception as error:
            message = (
                f"Cannot remove owned {backend} fixture: "
                f"{type(error).__name__}; retained namespace {name}"
            )
            if primary is not None:
                primary.add_note(message)
            else:
                raise RuntimeError(message) from None


async def change_database(backend, url, name, *, create):
    """Create/drop only a generated, validated benchmark namespace.

    Args:
        backend: Network database engine.
        url: Administrative connection URL.
        name: Generated private database name, never an application name.
        create: True to create; False to remove the owned database.

    Returns:
        None after the administrative operation and connection closure.
    """
    suffix = name.removeprefix("siderite_bench_")
    valid_suffix = all(character in "0123456789abcdef" for character in suffix)
    if (not name.startswith("siderite_bench_") or len(suffix) != 32
            or not valid_suffix):
        raise ValueError("Invalid owned database name")
    verb = "CREATE" if create else "DROP"
    condition = "" if create else "IF EXISTS "
    if backend == "postgres":
        import asyncpg

        connection = await asyncpg.connect(url, timeout=10, command_timeout=10)
        try:
            await connection.execute(f'{verb} DATABASE {condition}"{name}"')
        finally:
            await connection.close()
    elif backend == "mysql":
        import aiomysql

        parts = urllib.parse.urlsplit(url)
        connection = await aiomysql.connect(
            host=parts.hostname or "127.0.0.1", port=parts.port or 3306,
            user=urllib.parse.unquote(parts.username or "root"),
            password=urllib.parse.unquote(parts.password or ""),
            db=parts.path.lstrip("/") or "mysql",
            autocommit=True, connect_timeout=10,
        )
        try:
            async with connection.cursor() as cursor:
                await cursor.execute(f"{verb} DATABASE {condition}`{name}`")
        finally:
            await connection.ensure_closed()
    elif backend == "mongodb":
        from motor.motor_asyncio import AsyncIOMotorClient

        client = AsyncIOMotorClient(
            url, serverSelectionTimeoutMS=10000, connectTimeoutMS=10000,
            socketTimeoutMS=10000,
        )
        try:
            if create:
                await client[name].create_collection("siderite_fixture_owner")
            else:
                await client.drop_database(name)
        finally:
            client.close()
    else:
        raise ValueError("Unsupported fixture backend")
