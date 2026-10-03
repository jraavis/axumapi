"""Reproducibility metadata without connection URLs or environment secrets."""

import hashlib
import importlib.metadata
import json
import platform
import subprocess
import sys
from pathlib import Path


def write_metadata(root: Path, directory: Path, options: dict) -> None:
    """Record source identity, binary identity and installed driver versions.

    Args:
        root: Repository root.
        directory: Trial artifact directory.
        options: Parsed runner options; excludes database connection URLs.

    Returns:
        None after writing the run manifest.
    """
    versions = {}
    for package in (
        "fastapi",
        "uvicorn",
        "pydantic",
        "asyncpg",
        "aiomysql",
        "motor",
    ):
        try:
            versions[package] = importlib.metadata.version(package)
        except importlib.metadata.PackageNotFoundError:
            versions[package] = None
    binaries = {}
    for binary in ("hello_world", "siderite_todo"):
        path = root / "target" / "release" / binary
        if path.is_file():
            with path.open("rb") as content:
                digest = hashlib.file_digest(content, "sha256").hexdigest()
            binaries[binary] = {
                "sha256": digest,
                "modified_at": path.stat().st_mtime,
            }

    builds = {}
    current_sources = source_hashes(root)
    for name, binary in binaries.items():
        path = root / "target" / "release" / f"{name}.build.json"
        if not path.is_file():
            continue
        try:
            manifest = json.loads(path.read_text())
            matched = (manifest["binary_sha256"] == binary["sha256"]
                       and manifest["source_files_sha256"] == current_sources)
            builds[name] = {"matches_current_inputs": matched,
                            "manifest": manifest}
        except (ValueError, KeyError, TypeError):
            builds[name] = {"matches_current_inputs": False,
                            "error": "Invalid build manifest"}

    def git(*arguments):
        result = subprocess.run(
            ["git", *arguments],
            cwd=root,
            capture_output=True,
            text=True,
            check=False,
        )
        return result.stdout.strip() if result.returncode == 0 else None

    metadata = {
        "options": options,
        "source_revision": git("rev-parse", "HEAD"),
        "source_dirty": bool(git("status", "--porcelain")),
        "source_files_sha256": current_sources,
        "binaries": binaries,
        "build_provenance": builds,
        "python": sys.version,
        "platform": platform.platform(),
        "machine": platform.machine(),
        "python_packages": versions,
        "publication_minimums_met": (
            not options["fast"]
            and options["runs"] >= 5
            and options["min_seconds"] >= 10
        ),
        "limitations": [
            "Build provenance is local, not a hermetic attestation",
            "SQL settings are sampled; full session coverage remains",
            "ApacheBench distinguishes 2xx, not exact per-response status",
            "SQLite FastAPI uses the explicitly named blocking strategy",
        ],
    }
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "metadata.json").write_text(
        json.dumps(metadata, indent=2) + "\n"
    )


def source_hashes(root: Path) -> dict[str, str]:
    """Hash benchmark and Rust workspace sources, including untracked files.

    Args:
        root: Repository root containing the workspace and benchmark apps.

    Returns:
        Relative source paths mapped to SHA-256 digests.
    """
    paths = [root / "Cargo.toml", root / "Cargo.lock"]
    for folder in ("crates", "examples", "benchmarks/siderite_todo"):
        for pattern in ("*.rs", "Cargo.toml"):
            paths.extend((root / folder).rglob(pattern))
    for folder in ("benchmarks", "benchmarks/fastapi"):
        paths.extend((root / folder).glob("*.py"))
    paths.append(root / "benchmarks/fastapi/requirements.txt")
    hashes = {}
    for path in sorted(set(paths)):
        if path.is_file():
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            hashes[str(path.relative_to(root))] = digest
    return hashes
