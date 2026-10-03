"""Explicit SQLite lifetime and baseline strategy for the benchmark app."""

import sqlite3
from contextlib import contextmanager


class SQLiteConnections:
    """Use native synchronous SQLite with a named connection strategy.

    The pooled mode is confined to a single async application event loop.
    Its handlers contain no await while borrowing the connection. It is a
    throughput baseline; blocking SQLite still delays unrelated loop work.
    """

    def __init__(self, path, profile, mode):
        """Validate the baseline without opening a file.

        Args:
            path: Owned benchmark database file.
            profile: default, wal-full or wal-normal durability.
            mode: pooled-sync or per-request connection strategy.

        Returns:
            None; open pooled resources during application lifespan.
        """
        if profile not in ("default", "wal-full", "wal-normal"):
            raise ValueError("Invalid SQLite durability profile")
        if mode not in ("pooled-sync", "per-request"):
            raise ValueError("Invalid SQLite connection strategy")
        self.path, self.profile, self.mode = path, profile, mode
        self._shared = None

    def _connect(self):
        # One statement commits on acknowledgement, matching SQLx autocommit.
        connection = sqlite3.connect(self.path, isolation_level=None)
        try:
            connection.execute("PRAGMA foreign_keys = ON")
            sync = "NORMAL" if self.profile == "wal-normal" else "FULL"
            connection.execute(f"PRAGMA synchronous = {sync}")
        except BaseException:
            connection.close()
            raise
        return connection

    def open(self):
        """Open the shared writer on its owning application thread.

        Returns:
            None after opening; duplicate opens are rejected.
        """
        if self._shared is not None:
            raise RuntimeError("SQLite benchmark connection already open")
        if self.mode == "pooled-sync":
            self._shared = self._connect()

    @contextmanager
    def borrow(self):
        """Borrow on the application thread, closing private connections.

        Yields:
            A connection in autocommit mode. Pooled borrows do not await.
        """
        private = self.mode == "per-request"
        connection = self._connect() if private else self._shared
        if connection is None:
            raise RuntimeError("SQLite benchmark lifespan has not started")
        try:
            yield connection
        finally:
            if private:
                connection.close()

    def close(self):
        """Close the shared writer on the application thread.

        Returns:
            None after closing; repeated cleanup is harmless.
        """
        if self._shared is not None:
            self._shared.close()
            self._shared = None
