#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Deterministic pure-stdlib generator & verifier for Flatpak `cargo-sources.json`.

Reads `Cargo.lock` and emits 2 entries per external `crates.io` dependency
(`archive` + `.cargo-checksum.json` `inline`) plus 1 `.cargo/config.toml` entry.
"""

from __future__ import annotations

import argparse
import json
import sys
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DEFAULT_LOCK = ROOT / "Cargo.lock"
DEFAULT_OUT = ROOT / "packaging" / "linux" / "cargo-sources.json"
CRATES_IO_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"


def build_cargo_sources(lock_path: Path) -> list[dict]:
    data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
    packages = data.get("package", [])
    external: list[tuple[str, str, str]] = []
    for pkg in packages:
        source = pkg.get("source")
        if source is None:
            continue
        if source != CRATES_IO_SOURCE:
            raise ValueError(
                f"Unsupported non-crates.io external source for {pkg.get('name')}: {source}"
            )
        name = pkg["name"]
        version = pkg["version"]
        checksum = pkg.get("checksum")
        if not checksum:
            raise ValueError(f"Missing SHA-256 checksum in Cargo.lock for {name} {version}")
        external.append((name, version, checksum))

    external.sort(key=lambda item: (item[0], item[1]))
    entries: list[dict] = []
    for name, version, checksum in external:
        dest = f"cargo/vendor/{name}-{version}"
        entries.append(
            {
                "type": "archive",
                "archive-type": "tar-gzip",
                "url": f"https://static.crates.io/crates/{name}/{name}-{version}.crate",
                "sha256": checksum,
                "dest": dest,
            }
        )
        entries.append(
            {
                "type": "inline",
                "contents": json.dumps({"package": checksum, "files": {}}, sort_keys=True),
                "dest": dest,
                "dest-filename": ".cargo-checksum.json",
            }
        )

    entries.append(
        {
            "type": "inline",
            "contents": (
                "[source.crates-io]\n"
                'replace-with = "vendored-sources"\n\n'
                "[source.vendored-sources]\n"
                'directory = "cargo/vendor"\n'
            ),
            "dest": ".cargo",
            "dest-filename": "config.toml",
        }
    )
    return entries


def main() -> int:
    parser = argparse.ArgumentParser(description="Generate or verify packaging/linux/cargo-sources.json")
    parser.add_argument("--lock", type=Path, default=DEFAULT_LOCK)
    parser.add_argument("--out", type=Path, default=DEFAULT_OUT)
    parser.add_argument("--check", action="store_true", help="Verify existing file matches Cargo.lock")
    args = parser.parse_args()

    expected = build_cargo_sources(args.lock)
    rendered = json.dumps(expected, indent=2) + "\n"

    if args.check:
        if not args.out.is_file():
            print(f"ERROR: missing {args.out}", file=sys.stderr)
            return 1
        actual = json.loads(args.out.read_text(encoding="utf-8"))
        if actual != expected:
            print(
                f"ERROR: {args.out} is out of date with {args.lock} "
                f"(actual={len(actual)}, expected={len(expected)}). "
                "Run `python3 scripts/generate_cargo_sources.py`.",
                file=sys.stderr,
            )
            return 1
        archives = sum(1 for item in actual if item.get("type") == "archive")
        print(
            f"cargo-sources.json: OK ({len(actual)} entries = {archives} crates + checksums + .cargo/config.toml)"
        )
        return 0

    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(rendered, encoding="utf-8")
    archives = sum(1 for item in expected if item.get("type") == "archive")
    print(f"Wrote {args.out} ({len(expected)} entries = {archives} crates + checksums + .cargo/config.toml)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
