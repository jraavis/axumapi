"""Build release benchmark binaries with a checked source manifest."""

import argparse
import hashlib
import json
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path

from benchmark_metadata import source_hashes


def build(root, package, features):
    """Build and bind the binary hash to unchanged local build inputs.

    Args:
        root: Workspace root.
        package: Supported benchmark binary package name.
        features: Explicit Cargo feature list.

    Returns:
        Saved manifest path after a successful build with stable inputs.

    Raises:
        RuntimeError: Sources changed while Cargo was building.
        CalledProcessError: Compilation or toolchain inspection failed.
    """
    inputs = source_hashes(root)
    command = ["cargo", "build", "--locked", "--release", "-p", package]
    if features:
        command.extend(["--features", features])
    subprocess.run(command, cwd=root, check=True)
    if inputs != source_hashes(root):
        raise RuntimeError("Sources changed during build; rebuild evidence")
    binary = root / "target" / "release" / package
    manifest = {
        "package": package,
        "features": features,
        "sqlite_build_configuration": {
            key: os.environ.get(key) for key in (
                "LIBSQLITE3_SYS_USE_PKG_CONFIG", "PKG_CONFIG_PATH",
                "SQLITE3_LIB_DIR", "SQLITE3_INCLUDE_DIR",
            )
        },
        "command": command,
        "built_at": datetime.now(timezone.utc).isoformat(),
        "source_files_sha256": inputs,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "rustc": subprocess.check_output(
            ["rustc", "-Vv"], cwd=root, text=True
        ).strip(),
        "limitations": [
            "Local provenance, not a hermetic or independently attested build",
            "External native libraries and build environment are not attested",
        ],
    }
    path = binary.with_suffix(".build.json")
    path.write_text(json.dumps(manifest, indent=2) + "\n")
    return path


def main():
    """Build a supported binary and save its local provenance manifest.

    Returns:
        None after printing the manifest path.
    """
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", choices=("siderite_todo", "hello_world"),
                        default="siderite_todo")
    parser.add_argument("--features", default="")
    args = parser.parse_args()
    root = Path(__file__).resolve().parent.parent
    print(build(root, args.package, args.features))


if __name__ == "__main__":
    main()
