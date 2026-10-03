"""Build manifests must reject source changes and describe exact binaries."""

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from build_benchmarks import build


class BuildProvenanceTests(unittest.TestCase):
    """Check provenance failure boundaries without compiling fixture code."""

    def test_stable_build_and_changed_inputs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root / "target" / "release" / "siderite_todo"
            binary.parent.mkdir(parents=True)
            binary.write_bytes(b"fixture binary")
            with patch("build_benchmarks.source_hashes", return_value={}), \
                    patch("build_benchmarks.subprocess.run") as run, \
                    patch("build_benchmarks.subprocess.check_output",
                          return_value="rustc fixture"):
                path = build(root, "siderite_todo", "mysql-native")
            record = json.loads(path.read_text())
            self.assertEqual(record["features"], "mysql-native")
            self.assertEqual(record["source_files_sha256"], {})
            self.assertEqual(len(record["binary_sha256"]), 64)
            self.assertIn("--locked", run.call_args.args[0])
            path.unlink()
            with patch("build_benchmarks.source_hashes", side_effect=[
                    {"Cargo.lock": "old"}, {"Cargo.lock": "new"}]), \
                    patch("build_benchmarks.subprocess.run"), \
                    self.assertRaises(RuntimeError):
                build(root, "siderite_todo", "")
            self.assertFalse(path.exists())
