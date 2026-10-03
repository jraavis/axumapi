"""Read a sanitized sample of effective SQL session and durability settings."""

SQLITE = {
    "version": "SELECT sqlite_version()",
    "journal_mode": "PRAGMA journal_mode",
    "synchronous": "PRAGMA synchronous",
    "foreign_keys": "PRAGMA foreign_keys",
    "busy_timeout": "PRAGMA busy_timeout",
}
POSTGRES = {
    "version": "SELECT version()",
    "synchronous_commit": "SHOW synchronous_commit",
    "fsync": "SHOW fsync",
    "full_page_writes": "SHOW full_page_writes",
    "timezone": "SHOW TimeZone",
}
MYSQL = {
    "version": "SELECT VERSION()",
    "autocommit": "SELECT @@session.autocommit",
    "timezone": "SELECT @@session.time_zone",
    "sql_mode": "SELECT @@session.sql_mode",
    "innodb_flush_log_at_trx_commit":
        "SELECT @@global.innodb_flush_log_at_trx_commit",
    "sync_binlog": "SELECT @@global.sync_binlog",
}


async def capture(backend, sqlite, postgres, mysql, mongo):
    """Read fixed settings queries without recording connection information.

    Args:
        backend: SQL adapter name, or a non-SQL name to skip.
        sqlite: SQLite connection lifecycle manager.
        postgres: Asyncpg pool, when selected.
        mysql: Aiomysql pool, when selected.
        mongo: Motor database, when selected.

    Returns:
        Sampled settings, or None for unsupported adapters.
    """
    if backend == "sqlite":
        with sqlite.borrow() as connection:
            return {key: connection.execute(sql).fetchone()[0]
                    for key, sql in SQLITE.items()}
    if backend == "postgres":
        async with postgres.acquire() as connection:
            return {key: await connection.fetchval(sql)
                    for key, sql in POSTGRES.items()}
    if backend == "mysql":
        async with mysql.acquire() as connection:
            async with connection.cursor() as cursor:
                values = {}
                for key, sql in MYSQL.items():
                    await cursor.execute(sql)
                    values[key] = (await cursor.fetchone())[0]
                return values
    if backend == "mongodb":
        return await capture_mongo(mongo)
    return None


async def capture_mongo(database):
    """Capture only comparable concern, pool and retry policy fields.

    Args:
        database: The Motor database used by request handlers.

    Returns:
        Sanitized settings from the client and server defaults.

    Raises:
        ValueError: The benchmark uses an unsupported read preference.
    """
    import json

    if database.read_preference.mode != 0:
        raise ValueError("benchmark requires primary reads")
    admin = database.client.admin
    build = await admin.command("buildInfo")
    defaults = await admin.command("getDefaultRWConcern")
    replica = await admin.command("replSetGetConfig")
    journal = replica["config"]["writeConcernMajorityJournalDefault"]
    options = database.client.options
    pool = options.pool_options

    def canonical(value):
        return json.dumps(value, sort_keys=True, separators=(",", ":"))

    return {
        "version": build["version"],
        "read_preference": "primary",
        "client_read_concern": canonical(database.read_concern.document),
        "client_write_concern": canonical(database.write_concern.document),
        "server_default_read_concern": canonical(
            defaults.get("defaultReadConcern", {})
        ),
        "server_default_write_concern": canonical(
            defaults.get("defaultWriteConcern", {})
        ),
        "majority_journal_default": int(journal),
        "max_pool_size": pool.max_pool_size,
        "min_pool_size": pool.min_pool_size,
        "max_connecting": pool.max_connecting,
        "retry_reads": int(options.retry_reads),
        "retry_writes": int(options.retry_writes),
    }
