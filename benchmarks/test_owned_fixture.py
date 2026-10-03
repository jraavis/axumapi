"""Fixture ownership must preserve caller data through success and failure."""

import asyncio
import urllib.parse
import unittest
from pathlib import Path
from unittest.mock import AsyncMock, patch

from owned_fixture import change_database, owned_fixture


class OwnedFixtureTests(unittest.TestCase):
    """Database creation, names, credentials and cleanup are explicit."""

    def test_sqlite_file_is_private_and_removed_on_error(self):
        with self.assertRaisesRegex(RuntimeError, "trial failed"):
            with owned_fixture("sqlite", "sqlite://application.db") as values:
                url, path = values
                self.assertEqual(url, f"sqlite://{path}?mode=rwc")
                path.write_text("owned")
                self.assertNotEqual(path, Path("application.db"))
                raise RuntimeError("trial failed")
        self.assertFalse(path.exists())

    def test_network_cleanup_preserves_the_original_trial_error(self):
        operation = AsyncMock()
        root = "postgres://user:secret@localhost/application?sslmode=require"
        with patch("owned_fixture.change_database", operation):
            with self.assertRaisesRegex(RuntimeError, "trial failed"):
                with owned_fixture("postgres", root) as (url, path):
                    self.assertIsNone(path)
                    parts = urllib.parse.urlsplit(url)
                    self.assertTrue(parts.path.startswith("/siderite_bench_"))
                    self.assertEqual(parts.query, "sslmode=require")
                    raise RuntimeError("trial failed")
        calls = operation.await_args_list
        self.assertEqual(len(calls), 2)
        self.assertTrue(calls[0].kwargs["create"])
        self.assertFalse(calls[1].kwargs["create"])
        self.assertEqual(calls[0].args, calls[1].args)

    def test_mongodb_keeps_original_authentication_source(self):
        root = "mongodb://user:secret@localhost/application"
        with patch("owned_fixture.change_database", AsyncMock()):
            with owned_fixture("mongodb", root) as (url, _):
                query = urllib.parse.parse_qs(urllib.parse.urlsplit(url).query)
                self.assertEqual(query["authSource"], ["application"])

    def test_creation_failure_is_sanitized_and_attempts_owned_cleanup(self):
        operation = AsyncMock(side_effect=[ValueError("secret URL"), None])
        with patch("owned_fixture.change_database", operation):
            with self.assertRaisesRegex(RuntimeError, "ValueError") as failure:
                with owned_fixture("mysql", "mysql://user:secret@localhost/db"):
                    self.fail("creation failure entered fixture")
        self.assertNotIn("secret", str(failure.exception))
        self.assertEqual(operation.await_count, 2)

    def test_destructive_commands_reject_nonowned_names_before_io(self):
        for name in ["application", "siderite_bench_", "siderite_bench_;DROP"]:
            with self.subTest(name=name), self.assertRaises(ValueError):
                asyncio.run(change_database("mysql", "unused", name,
                                            create=False))
