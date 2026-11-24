#!/usr/bin/env python3
"""Brand and documented MIME allowlist CI gate for coree.

Standard library only. Executes replica-brand/sweep.py (or an embedded
identical pure-stdlib scanner in offline CI environments without ~/.gemini)
against `crates` and `packaging`, then validates every hit against the
explicit `mime_allowlist` in `replica/brand.json` by reading the full source
line from disk, stripping allowlisted MIME literals longest-first, and
re-scanning the remainder. Exits 0 iff zero non-MIME violations exist.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
from pathlib import Path

DEFAULT_EXTERNAL_SWEEP = Path(
    "/home/ahmed/.gemini/config/plugins/replica-skill/skills/replica-brand/sweep.py"
)

SKIP_DIRS = {
    ".git",
    "node_modules",
    ".next",
    ".nuxt",
    ".svelte-kit",
    "dist",
    "build",
    "out",
    ".vercel",
    ".turbo",
    ".cache",
    "coverage",
    "__pycache__",
    ".venv",
    "venv",
    "Pods",
    ".expo",
    "DerivedData",
    ".gradle",
}
SKIP_FILES = {
    "package-lock.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "bun.lockb",
    "Podfile.lock",
    "Cargo.lock",
    "poetry.lock",
}
BINARY_EXT = {
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".webp",
    ".ico",
    ".icns",
    ".pdf",
    ".zip",
    ".gz",
    ".woff",
    ".woff2",
    ".ttf",
    ".otf",
    ".eot",
    ".mp4",
    ".mov",
    ".mp3",
    ".wav",
    ".avif",
    ".heic",
    ".psd",
    ".sketch",
    ".fig",
    ".jar",
    ".so",
    ".dylib",
}
HEX = re.compile(r"#(?:[0-9a-fA-F]{6}|[0-9a-fA-F]{3})(?![0-9a-zA-Z])")


def norm_hex(h: str) -> str:
    h = h.lower().lstrip("#")
    if len(h) == 3:
        h = "".join(c * 2 for c in h)
    return "#" + h


def build_patterns(avoid: list[str], domains: list[str]) -> list[tuple[str, str, re.Pattern[str]]]:
    pats: list[tuple[str, str, re.Pattern[str]]] = []
    for name in avoid:
        name = name.strip()
        if not name:
            continue
        parts = [re.escape(p) for p in re.split(r"[\s_-]+", name) if p]
        pats.append(("name", name, re.compile(r"[\s_-]?".join(parts), re.I)))
    for d in domains:
        d = d.strip().lower()
        if d:
            pats.append(("domain", d, re.compile(re.escape(d), re.I)))
    return pats


def is_binary(path: str) -> bool:
    if os.path.splitext(path)[1].lower() in BINARY_EXT:
        return True
    try:
        with open(path, "rb") as fh:
            return b"\0" in fh.read(4096)
    except OSError:
        return True


def embedded_sweep(
    root: Path,
    avoid: list[str],
    domains: list[str],
    colors: list[str],
    max_size: int = 2_000_000,
) -> list[dict]:
    pats = build_patterns(avoid, domains)
    cols = {norm_hex(c) for c in colors if HEX.fullmatch(c.strip())}
    hits: list[dict] = []
    abs_root = os.path.abspath(root)
    for dirpath, dirnames, filenames in os.walk(abs_root):
        rel_dir = os.path.relpath(dirpath, abs_root)
        keep = []
        for d in dirnames:
            if d in SKIP_DIRS:
                continue
            if d == "replica" and rel_dir == ".":
                continue
            keep.append(d)
        dirnames[:] = sorted(keep)
        for d in dirnames:
            for kind, label, rx in pats:
                if kind == "name" and rx.search(d):
                    hits.append(
                        {
                            "file": os.path.normpath(os.path.join(rel_dir, d)) + "/",
                            "line": 0,
                            "kind": "path",
                            "match": label,
                            "text": "folder name",
                        }
                    )
        for fn in sorted(filenames):
            if fn in SKIP_FILES:
                continue
            path = os.path.join(dirpath, fn)
            rel = os.path.normpath(os.path.join(rel_dir, fn))
            for kind, label, rx in pats:
                if kind == "name" and rx.search(fn):
                    hits.append(
                        {
                            "file": rel,
                            "line": 0,
                            "kind": "path",
                            "match": label,
                            "text": "file name",
                        }
                    )
            try:
                if os.path.getsize(path) > max_size or is_binary(path):
                    continue
                with open(path, encoding="utf-8", errors="replace") as fh:
                    lines = fh.readlines()
            except OSError:
                continue
            for i, line in enumerate(lines, 1):
                for kind, label, rx in pats:
                    if rx.search(line):
                        hits.append(
                            {
                                "file": rel,
                                "line": i,
                                "kind": kind,
                                "match": label,
                                "text": line.strip()[:160],
                            }
                        )
                if cols:
                    for m in HEX.finditer(line):
                        if norm_hex(m.group(0)) in cols:
                            hits.append(
                                {
                                    "file": rel,
                                    "line": i,
                                    "kind": "color",
                                    "match": norm_hex(m.group(0)),
                                    "text": line.strip()[:160],
                                }
                            )
    return hits


def resolve_external_sweep(explicit: str | None) -> Path | None:
    if explicit:
        p = Path(explicit)
        return p if p.is_file() else None
    env_p = os.environ.get("SWEEP_PY", "").strip()
    if env_p:
        p = Path(env_p)
        return p if p.is_file() else None
    if DEFAULT_EXTERNAL_SWEEP.is_file():
        return DEFAULT_EXTERNAL_SWEEP
    return None


def run_external_sweep(sweep_script: Path, target_dir: Path, config_path: Path) -> list[dict]:
    proc = subprocess.run(
        [sys.executable, str(sweep_script), str(target_dir), "--config", str(config_path), "--json"],
        capture_output=True,
        text=True,
        check=False,
    )
    if proc.returncode not in (0, 1):
        print(
            f"brand_sweep: external sweep.py failed (exit {proc.returncode}): {proc.stderr.strip()}",
            file=sys.stderr,
        )
        sys.exit(2)
    try:
        data = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        print(f"brand_sweep: invalid JSON from external sweep.py: {exc}", file=sys.stderr)
        sys.exit(2)
    if not isinstance(data, list):
        print("brand_sweep: expected JSON list from external sweep.py", file=sys.stderr)
        sys.exit(2)
    return data


def normalize_rel_path(target: str, hit_file: str) -> str:
    combined = os.path.normpath(os.path.join(target, hit_file))
    return combined.replace("\\", "/")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Verify brand cleanliness with MIME allowlist gate.")
    ap.add_argument("--repo-root", default=".", help="Repository root directory")
    ap.add_argument("--config", default="replica/brand.json", help="Path to brand.json config")
    ap.add_argument("--targets", nargs="+", default=["crates", "packaging"], help="Target subdirs")
    ap.add_argument("--sweep-script", default=None, help="Explicit path to replica-brand/sweep.py")
    ap.add_argument(
        "--require-external-sweep",
        action="store_true",
        help="Fail with exit 2 if external replica-brand/sweep.py cannot be found",
    )
    args = ap.parse_args(argv)

    repo_root = Path(args.repo_root).resolve()
    config_path = (repo_root / args.config).resolve() if not Path(args.config).is_absolute() else Path(args.config)

    try:
        cfg = json.loads(config_path.read_text(encoding="utf-8"))
    except (OSError, ValueError) as exc:
        print(f"brand_sweep: failed to read config {config_path}: {exc}", file=sys.stderr)
        return 2

    avoid = list(cfg.get("avoid", []))
    domains = list(cfg.get("domains", []))
    colors = list(cfg.get("colors", []))
    allow_cfg = cfg.get("mime_allowlist")
    if not isinstance(allow_cfg, dict):
        print("brand_sweep: missing 'mime_allowlist' object in brand.json", file=sys.stderr)
        return 2
    raw_literals = allow_cfg.get("literals", [])
    raw_allowed_files = allow_cfg.get("allowed_files", [])
    if not raw_literals or not raw_allowed_files:
        print("brand_sweep: 'mime_allowlist.literals' and 'allowed_files' must be non-empty", file=sys.stderr)
        return 2

    literals = sorted({str(x) for x in raw_literals if str(x)}, key=len, reverse=True)
    # Bounded MIME token delimiters: never allow alphanumeric, '_', '.', '-', '+', or '/' adjacent to a literal
    mime_left_bound = r"(?<![A-Za-z0-9_.\-+/])"
    mime_right_bound = r"(?![A-Za-z0-9_.\-+/])"
    literal_patterns = [
        (lit, re.compile(f"{mime_left_bound}{re.escape(lit)}{mime_right_bound}"))
        for lit in literals
    ]
    allowed_files = {str(x).replace("\\", "/") for x in raw_allowed_files if str(x)}

    external_sweep = resolve_external_sweep(args.sweep_script)
    if args.require_external_sweep and external_sweep is None:
        print(
            "brand_sweep: --require-external-sweep specified but sweep.py was not found",
            file=sys.stderr,
        )
        return 2

    scanner_mode = f"external ({external_sweep})" if external_sweep else "embedded-stdlib"
    pats = build_patterns(avoid, domains)
    cols = {norm_hex(c) for c in colors if HEX.fullmatch(c.strip())}
    if not (pats or cols):
        print(
            "brand_sweep: 'avoid', 'domains', and 'colors' cannot all be empty in brand.json",
            file=sys.stderr,
        )
        return 2
    pat_by_key = {(p_kind, p_label): rx for p_kind, p_label, rx in pats}

    total_raw_hits = 0
    allowlisted_hits: list[tuple[str, int, str]] = []
    violations: list[tuple[str, int, str, str, str]] = []

    for target in args.targets:
        target_dir = repo_root / target
        if not target_dir.is_dir():
            print(f"brand_sweep: target directory does not exist: {target_dir}", file=sys.stderr)
            return 2

        hits = (
            run_external_sweep(external_sweep, target_dir, config_path)
            if external_sweep
            else embedded_sweep(target_dir, avoid, domains, colors)
        )
        total_raw_hits += len(hits)

        line_cache: dict[Path, list[str]] = {}
        for h in hits:
            rel_path = normalize_rel_path(target, str(h.get("file", "")))
            kind = str(h.get("kind", ""))
            line_no = int(h.get("line", 0) or 0)
            match_label = str(h.get("match", ""))
            preview = str(h.get("text", ""))

            if kind != "name":
                violations.append((rel_path, line_no, kind, match_label, f"non-name hit: {preview}"))
                continue
            if line_no <= 0:
                violations.append((rel_path, line_no, kind, match_label, f"invalid line hit: {preview}"))
                continue
            if rel_path not in allowed_files:
                violations.append((rel_path, line_no, kind, match_label, preview))
                continue

            full_file = repo_root / rel_path
            if full_file not in line_cache:
                try:
                    line_cache[full_file] = full_file.read_text(
                        encoding="utf-8", errors="replace"
                    ).splitlines()
                except OSError as exc:
                    violations.append((rel_path, line_no, kind, match_label, f"unreadable file: {exc}"))
                    continue

            file_lines = line_cache[full_file]
            if line_no > len(file_lines):
                violations.append((rel_path, line_no, kind, match_label, "line number out of range"))
                continue

            full_line = file_lines[line_no - 1]
            stripped = full_line
            matched_literals: list[str] = []
            for lit, lit_rx in literal_patterns:
                stripped, count = lit_rx.subn(" ", stripped)
                if count > 0:
                    matched_literals.append(lit)

            if not matched_literals:
                violations.append(
                    (
                        rel_path,
                        line_no,
                        kind,
                        match_label,
                        f"no delimited allowlisted MIME literal found on line: {full_line.strip()}",
                    )
                )
                continue

            hit_rx = pat_by_key.get((kind, match_label))
            if hit_rx is None or not any(hit_rx.search(lit) for lit in matched_literals):
                violations.append(
                    (
                        rel_path,
                        line_no,
                        kind,
                        match_label,
                        f"hit '{match_label}' is not contained in matched MIME literals {matched_literals}",
                    )
                )
                continue

            remainder_matches = [
                f"{p_kind}:{p_label}"
                for p_kind, p_label, rx in pats
                if rx.search(stripped)
            ]
            if cols:
                for m in HEX.finditer(stripped):
                    if norm_hex(m.group(0)) in cols:
                        remainder_matches.append(f"color:{norm_hex(m.group(0))}")

            if remainder_matches:
                violations.append(
                    (
                        rel_path,
                        line_no,
                        kind,
                        match_label,
                        f"disallowed remainder ({', '.join(remainder_matches)}): {stripped.strip()}",
                    )
                )
            else:
                allowlisted_hits.append((rel_path, line_no, match_label))

    by_file: dict[str, int] = {}
    for rel_path, _, _ in allowlisted_hits:
        by_file[rel_path] = by_file.get(rel_path, 0) + 1

    print(f"brand_sweep: scanner={scanner_mode}")
    print(
        f"brand_sweep: raw_hits={total_raw_hits}, allowlisted_mime_hits={len(allowlisted_hits)}, violations={len(violations)}"
    )
    for rel_path in sorted(by_file):
        print(f"  [allowlisted MIME] {rel_path}: {by_file[rel_path]} hit(s)")

    if violations:
        print("\nbrand_sweep: DISALLOWED NON-MIME HITS DETECTED:", file=sys.stderr)
        for rel_path, line_no, kind, match_label, detail in violations:
            print(
                f"  - {rel_path}:{line_no} [{kind}:{match_label}] {detail}",
                file=sys.stderr,
            )
        return 1

    print("brand_sweep: PASS (0 disallowed non-MIME hits)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
