"""Shared pure-Python stdlib helpers for the coree opaque-box E2E test suite.

Zero third-party dependencies, 100% offline-first, zero Cargo.lock contention
when prebuilt binaries exist.
"""

from __future__ import annotations

import csv
import gzip
import json
import math
import os
import re
import shutil
import statistics
import struct
import subprocess
import tempfile
import time
import xml.etree.ElementTree as ET
import zipfile
import zlib
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator, Literal, Optional


# ---------------------------------------------------------------------------
# Canonical 51 Roadmap IDs & Wave Constants
# ---------------------------------------------------------------------------

ALL_51_ROADMAP_IDS: tuple[str, ...] = (
    # Mandatory Blockers (5)
    "MND-01", "MND-02", "MND-03", "MND-04", "MND-05",
    # Core Requirements (12)
    "REQ-01", "REQ-02", "REQ-03", "REQ-04", "REQ-05", "REQ-06",
    "REQ-07", "REQ-08", "REQ-09", "REQ-10", "REQ-11", "REQ-12",
    # Additions (12)
    "ADD-01", "ADD-02", "ADD-03", "ADD-04", "ADD-05", "ADD-06",
    "ADD-07", "ADD-08", "ADD-09", "ADD-10", "ADD-11", "ADD-12",
    # Improvements (12)
    "IMP-01", "IMP-02", "IMP-03", "IMP-04", "IMP-05", "IMP-06",
    "IMP-07", "IMP-08", "IMP-09", "IMP-10", "IMP-11", "IMP-12",
    # Remediations (10)
    "REM-01", "REM-02", "REM-03", "REM-04", "REM-05", "REM-06",
    "REM-07", "REM-08", "REM-09", "REM-10",
)

VALID_WAVES: tuple[str, ...] = ("baseline", "W1", "W2", "W3", "W4", "W5")

DEFAULT_REPLICA_SKILLS_DIR = Path("/home/ahmed/.gemini/config/plugins/replica-skill/skills")
DEFAULT_ICC_DIR = Path("/usr/share/color/icc")


# ---------------------------------------------------------------------------
# Core Data Contracts (SCOPE.md)
# ---------------------------------------------------------------------------

@dataclass
class TestOutcome:
    status: Literal["PASS", "FAIL", "ERROR"]
    message: str
    duration_ms: float = 0.0
    metrics: dict[str, Any] = field(default_factory=dict)


@dataclass(frozen=True)
class TestCase:
    id: str
    tier: int
    title: str
    roadmap_ids: tuple[str, ...]
    feature_refs: tuple[str, ...]
    target_wave: str
    smoke: bool
    run: Callable[["RunContext"], TestOutcome]


@dataclass(frozen=True)
class FeatureSpec:
    id: str
    row_index: int
    csv_line: int
    feature: str
    area: str
    priority: str
    original: str
    clone: str
    notes: str
    category: Literal["counted", "skip", "extra"]


@dataclass(frozen=True)
class FeaturesCatalog:
    counted: tuple[FeatureSpec, ...]
    skipped: tuple[FeatureSpec, ...]
    extras: tuple[FeatureSpec, ...]
    by_id: dict[str, FeatureSpec]


@dataclass
class RunContext:
    repo_root: Path
    e2e_root: Path
    fixtures_dir: Path
    cli_bin: Path
    gui_bin: Optional[Path]
    replica_skills_dir: Optional[Path]
    timeout_scale: float
    scratch_root: Path
    cli_subcommands: frozenset[str]
    features_catalog: FeaturesCatalog
    icc_dir: Optional[Path]

    def make_scratch_dir(self, test_id: str) -> Path:
        safe = re.sub(r"[^A-Za-z0-9_.-]+", "_", test_id)
        d = self.scratch_root / safe
        d.mkdir(parents=True, exist_ok=True)
        return d


# ---------------------------------------------------------------------------
# replica/features.csv Loader & Validator
# ---------------------------------------------------------------------------

def load_features_csv(repo_root: Path) -> FeaturesCatalog:
    csv_path = repo_root / "replica" / "features.csv"
    if not csv_path.is_file():
        raise FileNotFoundError(f"Missing replica/features.csv at {csv_path}")

    counted: list[FeatureSpec] = []
    skipped: list[FeatureSpec] = []
    extras: list[FeatureSpec] = []
    by_id: dict[str, FeatureSpec] = {}
    seen_features: set[str] = set()

    with csv_path.open("r", encoding="utf-8-sig", newline="") as fh:
        reader = csv.DictReader(fh)
        if reader.fieldnames is None:
            raise ValueError("replica/features.csv has no header")
        required_cols = {"feature", "area", "priority", "original", "clone", "notes"}
        norm_fields = {f.strip().lower() for f in reader.fieldnames}
        missing_cols = required_cols - norm_fields
        if missing_cols:
            raise ValueError(f"replica/features.csv missing columns: {sorted(missing_cols)}")

        for data_idx, raw_row in enumerate(reader, start=1):
            row = {k.strip().lower(): (v or "").strip() for k, v in raw_row.items() if k is not None}
            csv_line = data_idx + 1
            norm_feat = " ".join(row["feature"].lower().split())
            if not norm_feat:
                raise ValueError(f"replica/features.csv line {csv_line} has empty feature name")
            if norm_feat in seen_features:
                raise ValueError(
                    f"Duplicate normalized feature in replica/features.csv line {csv_line}: {row['feature']!r}"
                )
            seen_features.add(norm_feat)
            orig = row["original"].lower()
            clone = row["clone"].lower()
            prio = row["priority"].lower()

            if orig in ("no", "n", "false"):
                cat: Literal["counted", "skip", "extra"] = "extra"
                fid = f"EXT-F{len(extras) + 1:02d}"
            elif clone == "skip":
                cat = "skip"
                fid = f"SKIP-{len(skipped) + 1:02d}"
            else:
                cat = "counted"
                fid = f"FEAT-{len(counted) + 1:02d}"

            spec = FeatureSpec(
                id=fid,
                row_index=data_idx,
                csv_line=csv_line,
                feature=row["feature"],
                area=row["area"].lower(),
                priority=prio,
                original=orig,
                clone=clone,
                notes=row["notes"],
                category=cat,
            )
            by_id[fid] = spec
            if cat == "counted":
                counted.append(spec)
            elif cat == "skip":
                skipped.append(spec)
            else:
                extras.append(spec)

    if len(counted) != 86:
        raise ValueError(f"Expected 86 counted features in features.csv, got {len(counted)}")
    if len(skipped) != 6:
        raise ValueError(f"Expected 6 skipped features in features.csv, got {len(skipped)}")
    if len(extras) != 12:
        raise ValueError(f"Expected 12 extra features in features.csv, got {len(extras)}")

    must_count = sum(1 for s in counted if s.priority == "must")
    should_count = sum(1 for s in counted if s.priority == "should")
    could_count = sum(1 for s in counted if s.priority == "could")
    if (must_count, should_count, could_count) != (68, 17, 1):
        raise ValueError(
            f"Unexpected priority split in counted features: "
            f"must={must_count}, should={should_count}, could={could_count}"
        )

    return FeaturesCatalog(
        counted=tuple(counted),
        skipped=tuple(skipped),
        extras=tuple(extras),
        by_id=by_id,
    )


# ---------------------------------------------------------------------------
# Binary, Replica Skill & ICC Discovery (Zero Cargo.lock Contention)
# ---------------------------------------------------------------------------

def _pick_newest_binary(candidates: list[Path]) -> Optional[Path]:
    existing = [p for p in candidates if p.is_file() and os.access(p, os.X_OK)]
    if not existing:
        return None
    # Pick newest mtime; tie-break prefers earlier entry in candidates (release before debug)
    return max(existing, key=lambda p: (p.stat().st_mtime, -candidates.index(p)))


def discover_binaries(
    repo_root: Path,
    cli_bin_override: Optional[Path] = None,
    bin_dir_override: Optional[Path] = None,
    allow_cargo_build: bool = True,
) -> tuple[Path, Optional[Path]]:
    exe_suffix = ".exe" if os.name == "nt" else ""
    cli_name = f"coree-cli{exe_suffix}"
    gui_name = f"coree{exe_suffix}"

    cli_bin: Optional[Path] = None
    gui_bin: Optional[Path] = None

    # 1. Explicit CLI override / env var
    env_cli = os.environ.get("COREE_CLI_BIN")
    if cli_bin_override is not None:
        p = cli_bin_override.resolve()
        if not p.is_file():
            raise FileNotFoundError(f"--cli-bin not found: {p}")
        if not os.access(p, os.X_OK):
            raise PermissionError(f"--cli-bin is not executable: {p}")
        cli_bin = p
    elif env_cli:
        p = Path(env_cli).resolve()
        if not p.is_file():
            raise FileNotFoundError(f"COREE_CLI_BIN not found: {p}")
        if not os.access(p, os.X_OK):
            raise PermissionError(f"COREE_CLI_BIN is not executable: {p}")
        cli_bin = p

    env_gui = os.environ.get("COREE_GUI_BIN")
    if env_gui:
        p = Path(env_gui).resolve()
        if p.is_file() and os.access(p, os.X_OK):
            gui_bin = p

    # 2. Explicit --bin-dir / COREE_BIN_DIR
    eff_bin_dir = bin_dir_override
    if eff_bin_dir is None and os.environ.get("COREE_BIN_DIR"):
        eff_bin_dir = Path(os.environ["COREE_BIN_DIR"])
    if eff_bin_dir is not None:
        bd = eff_bin_dir.resolve()
        if not bd.is_dir():
            raise FileNotFoundError(f"--bin-dir directory not found: {bd}")
        if cli_bin is None:
            cand = bd / cli_name
            if not cand.is_file():
                raise FileNotFoundError(f"{cli_name} not found in --bin-dir: {bd}")
            if not os.access(cand, os.X_OK):
                raise PermissionError(f"{cli_name} in --bin-dir is not executable: {cand}")
            cli_bin = cand
        gui_cand = bd / gui_name
        if gui_bin is None and gui_cand.is_file() and os.access(gui_cand, os.X_OK):
            gui_bin = gui_cand

    # 3. Prebuilt workspace binaries (target/release vs target/debug — zero Cargo.lock touch)
    if cli_bin is None:
        cli_bin = _pick_newest_binary([
            repo_root / "target" / "release" / cli_name,
            repo_root / "target" / "debug" / cli_name,
        ])
    if gui_bin is None:
        if cli_bin is not None and (cli_bin.parent / gui_name).is_file() and os.access(cli_bin.parent / gui_name, os.X_OK):
            gui_bin = cli_bin.parent / gui_name
        else:
            gui_bin = _pick_newest_binary([
                repo_root / "target" / "release" / gui_name,
                repo_root / "target" / "debug" / gui_name,
            ])

    # 4. Fallback ONLY if no prebuilt binary exists anywhere
    if cli_bin is None:
        if not allow_cargo_build:
            raise FileNotFoundError("No prebuilt coree-cli binary found in target/release or target/debug")
        env = os.environ.copy()
        cargo_bin = str(Path.home() / ".cargo" / "bin")
        if cargo_bin not in env.get("PATH", ""):
            env["PATH"] = f"{cargo_bin}:{env.get('PATH', '')}"
        proc = subprocess.run(
            ["cargo", "build", "-p", "coree-app", "--bin", "coree-cli"],
            cwd=str(repo_root),
            env=env,
            capture_output=True,
            text=True,
            timeout=600,
        )
        if proc.returncode != 0:
            raise RuntimeError(
                f"Fallback cargo build failed (rc={proc.returncode}): {proc.stderr[-800:]}"
            )
        cli_bin = _pick_newest_binary([
            repo_root / "target" / "debug" / cli_name,
            repo_root / "target" / "release" / cli_name,
        ])
        if cli_bin is None:
            raise FileNotFoundError("cargo build succeeded but coree-cli binary still missing")

    return cli_bin, gui_bin


def discover_replica_skills_dir() -> Optional[Path]:
    env_dir = os.environ.get("REPLICA_SKILLS_DIR")
    if env_dir:
        p = Path(env_dir).expanduser().resolve()
        return p if p.is_dir() else None
    return DEFAULT_REPLICA_SKILLS_DIR if DEFAULT_REPLICA_SKILLS_DIR.is_dir() else None


def discover_icc_dir() -> Optional[Path]:
    return DEFAULT_ICC_DIR if DEFAULT_ICC_DIR.is_dir() else None


# ---------------------------------------------------------------------------
# Subprocess Runner with Timeout & Accurate Per-Child Peak RSS
# ---------------------------------------------------------------------------

@dataclass
class SubprocessResult:
    cmd: list[str]
    returncode: int
    stdout: str
    stderr: str
    duration_ms: float
    max_rss_kb: Optional[int] = None
    timed_out: bool = False


def run_cmd(
    cmd: list[str],
    *,
    cwd: Optional[Path] = None,
    timeout: float = 30.0,
    timeout_scale: float = 1.0,
    env: Optional[dict[str, str]] = None,
    measure_rss: bool = True,
) -> SubprocessResult:
    eff_timeout = max(1.0, timeout * timeout_scale)
    eff_env = os.environ.copy()
    if env:
        eff_env.update(env)

    time_bin = Path("/usr/bin/time")
    rss_file: Optional[Path] = None
    actual_cmd = list(cmd)
    if measure_rss and time_bin.is_file():
        fd, tmp_name = tempfile.mkstemp(prefix="coree_rss_", suffix=".txt")
        os.close(fd)
        rss_file = Path(tmp_name)
        actual_cmd = [str(time_bin), "-o", str(rss_file), "-f", "%M", *cmd]

    t0 = time.perf_counter()
    try:
        proc = subprocess.run(
            actual_cmd,
            cwd=str(cwd) if cwd else None,
            env=eff_env,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=eff_timeout,
        )
        dt_ms = (time.perf_counter() - t0) * 1000.0
        max_rss: Optional[int] = None
        if rss_file is not None and rss_file.is_file():
            raw = rss_file.read_text(encoding="utf-8", errors="replace").strip()
            for tok in reversed(raw.splitlines()):
                tok = tok.strip()
                if tok.isdigit():
                    max_rss = int(tok)
                    break
        return SubprocessResult(
            cmd=list(cmd),
            returncode=proc.returncode,
            stdout=proc.stdout,
            stderr=proc.stderr,
            duration_ms=dt_ms,
            max_rss_kb=max_rss,
            timed_out=False,
        )
    except subprocess.TimeoutExpired as exc:
        dt_ms = (time.perf_counter() - t0) * 1000.0
        out = exc.stdout.decode("utf-8", errors="replace") if isinstance(exc.stdout, bytes) else (exc.stdout or "")
        err = exc.stderr.decode("utf-8", errors="replace") if isinstance(exc.stderr, bytes) else (exc.stderr or "")
        return SubprocessResult(
            cmd=list(cmd),
            returncode=-1,
            stdout=out,
            stderr=f"TIMEOUT after {eff_timeout:.1f}s\n{err}",
            duration_ms=dt_ms,
            max_rss_kb=None,
            timed_out=True,
        )
    finally:
        if rss_file is not None and rss_file.exists():
            try:
                rss_file.unlink()
            except OSError:
                pass


# ---------------------------------------------------------------------------
# Critical coree-cli [FILES]... Trap Mitigation
# ---------------------------------------------------------------------------

def discover_cli_subcommands(cli_bin: Path, timeout: float = 15.0) -> frozenset[str]:
    """Parse `coree-cli --help` Commands section so tests never blindly pass
    unregistered subcommands that clap would treat as positional `[FILES]...`."""
    with tempfile.TemporaryDirectory(prefix="coree_help_") as help_cwd:
        res = run_cmd(
            [str(cli_bin.resolve()), "--help"],
            cwd=Path(help_cwd),
            timeout=timeout,
            measure_rss=False,
        )
    if res.returncode != 0:
        raise RuntimeError(f"{cli_bin} --help failed (rc={res.returncode}): {res.stderr}")
    subcommands: set[str] = set()
    in_commands = False
    for line in res.stdout.splitlines():
        stripped = line.strip()
        if stripped == "Commands:":
            in_commands = True
            continue
        if in_commands:
            if not stripped:
                in_commands = False
                continue
            if stripped.endswith(":") and not line.startswith(" "):
                in_commands = False
                continue
            parts = stripped.split()
            if parts:
                subcommands.add(parts[0])
    return frozenset(subcommands)


def require_cli_subcommand(ctx: RunContext, name: str, target_wave: str) -> Optional[TestOutcome]:
    if name not in ctx.cli_subcommands:
        return TestOutcome(
            "FAIL",
            f"subcommand '{name}' not yet registered in coree-cli --help (target_wave={target_wave})",
            metrics={"registered_subcommands": sorted(ctx.cli_subcommands)},
        )
    return None


# ---------------------------------------------------------------------------
# Pure-Python Semantic Parsers (Anti-Illusion Oracles)
# ---------------------------------------------------------------------------

def _paeth_predictor(a: int, b: int, c: int) -> int:
    p = a + b - c
    pa = abs(p - a)
    pb = abs(p - b)
    pc = abs(p - c)
    if pa <= pb and pa <= pc:
        return a
    if pb <= pc:
        return b
    return c


_ADAM7_PASSES = (
    (0, 0, 8, 8),
    (4, 0, 8, 8),
    (0, 4, 4, 8),
    (2, 0, 4, 4),
    (0, 2, 2, 4),
    (1, 0, 2, 2),
    (0, 1, 1, 2),
)


def _iter_unfiltered_scanlines(
    raw: bytes,
    raw_off: int,
    pw: int,
    ph: int,
    bits_per_pixel: int,
    end_off_out: Optional[list[int]] = None,
) -> Iterator[bytes]:
    """Stream unfiltered PNG scanlines row-by-row using O(stride) memory."""
    stride = (pw * bits_per_pixel + 7) // 8
    filter_bpp = max(1, (bits_per_pixel + 7) // 8)
    expected = ph * (1 + stride)
    if raw_off + expected > len(raw):
        raise ValueError(f"PNG raw scanline size mismatch: need {raw_off + expected}, got {len(raw)}")
    prev = bytearray(stride)
    pos = raw_off
    for _ in range(ph):
        ftype = raw[pos]
        scan = bytearray(raw[pos + 1:pos + 1 + stride])
        pos += 1 + stride
        if ftype == 1:
            for i in range(filter_bpp, stride):
                scan[i] = (scan[i] + scan[i - filter_bpp]) & 0xFF
        elif ftype == 2:
            for i in range(stride):
                scan[i] = (scan[i] + prev[i]) & 0xFF
        elif ftype == 3:
            for i in range(stride):
                a = scan[i - filter_bpp] if i >= filter_bpp else 0
                scan[i] = (scan[i] + ((a + prev[i]) >> 1)) & 0xFF
        elif ftype == 4:
            for i in range(stride):
                a = scan[i - filter_bpp] if i >= filter_bpp else 0
                b = prev[i]
                c = prev[i - filter_bpp] if i >= filter_bpp else 0
                scan[i] = (scan[i] + _paeth_predictor(a, b, c)) & 0xFF
        elif ftype != 0:
            raise ValueError(f"Unsupported PNG filter {ftype}")
        prev = scan
        yield bytes(scan)
    if end_off_out is not None:
        end_off_out[:] = [pos]


def _png_scan_row_to_rgba(
    scan: bytes,
    pw: int,
    color_type: int,
    bit_depth: int,
    plte: Optional[bytes],
    trns: Optional[bytes],
) -> bytearray:
    out = bytearray(pw * 4)
    if color_type == 3:
        if plte is None:
            raise ValueError("Indexed PNG missing PLTE chunk")
        num_pal = len(plte) // 3
        for x in range(pw):
            if bit_depth == 8:
                idx = scan[x]
            else:
                ppb = 8 // bit_depth
                byte_val = scan[x // ppb]
                shift = (ppb - 1 - (x % ppb)) * bit_depth
                idx = (byte_val >> shift) & ((1 << bit_depth) - 1)
            if idx >= num_pal:
                raise ValueError(f"PNG palette index {idx} out of range ({num_pal})")
            off = x * 4
            out[off] = plte[idx * 3]
            out[off + 1] = plte[idx * 3 + 1]
            out[off + 2] = plte[idx * 3 + 2]
            out[off + 3] = trns[idx] if (trns is not None and idx < len(trns)) else 255
        return out

    if color_type == 0:
        trns_g = struct.unpack(">H", trns[:2])[0] if (trns is not None and len(trns) >= 2) else None
        for x in range(pw):
            if bit_depth == 16:
                val16 = (scan[x * 2] << 8) | scan[x * 2 + 1]
                v = scan[x * 2]
                a = 0 if (trns_g is not None and val16 == trns_g) else 255
            elif bit_depth == 8:
                v = scan[x]
                a = 0 if (trns_g is not None and v == trns_g) else 255
            else:
                ppb = 8 // bit_depth
                shift = (ppb - 1 - (x % ppb)) * bit_depth
                raw_v = (scan[x // ppb] >> shift) & ((1 << bit_depth) - 1)
                max_v = (1 << bit_depth) - 1
                v = (raw_v * 255) // max_v
                a = 0 if (trns_g is not None and raw_v == trns_g) else 255
            off = x * 4
            out[off] = v
            out[off + 1] = v
            out[off + 2] = v
            out[off + 3] = a
        return out

    if color_type == 2:
        trns_rgb = struct.unpack(">HHH", trns[:6]) if (trns is not None and len(trns) >= 6) else None
        if bit_depth == 16:
            for x in range(pw):
                so = x * 6
                r16 = (scan[so] << 8) | scan[so + 1]
                g16 = (scan[so + 2] << 8) | scan[so + 3]
                b16 = (scan[so + 4] << 8) | scan[so + 5]
                off = x * 4
                out[off] = scan[so]
                out[off + 1] = scan[so + 2]
                out[off + 2] = scan[so + 4]
                out[off + 3] = 0 if (trns_rgb is not None and (r16, g16, b16) == trns_rgb) else 255
        else:
            for x in range(pw):
                so = x * 3
                r, g, b = scan[so], scan[so + 1], scan[so + 2]
                off = x * 4
                out[off] = r
                out[off + 1] = g
                out[off + 2] = b
                out[off + 3] = 0 if (trns_rgb is not None and (r, g, b) == trns_rgb) else 255
        return out

    if color_type == 4:
        step = 4 if bit_depth == 16 else 2
        for x in range(pw):
            so = x * step
            v = scan[so]
            a = scan[so + (2 if bit_depth == 16 else 1)]
            off = x * 4
            out[off] = v
            out[off + 1] = v
            out[off + 2] = v
            out[off + 3] = a
        return out

    if color_type == 6:
        if bit_depth == 16:
            for x in range(pw):
                so = x * 8
                off = x * 4
                out[off] = scan[so]
                out[off + 1] = scan[so + 2]
                out[off + 2] = scan[so + 4]
                out[off + 3] = scan[so + 6]
        else:
            out[:] = scan[:pw * 4]
        return out

    raise ValueError(f"Unsupported PNG color_type={color_type}")


_MAX_PNG_RAW_BYTES = 64 * 1024 * 1024       # 67,108,864 B (safe under RLIMIT_AS=512 MiB)
_MAX_PNG_PIXELS = 16 * 1024 * 1024          # 16,777,216 px (single-image ceiling under RLIMIT_AS=512 MiB)
_MAX_PNG_COMPARE_PIXELS = 10 * 1024 * 1024  # 10,485,760 px (dual-image ceiling; covers A4@300 DPI = 8.70 MP)
_PNG_ZLIB_SLICE_BYTES = 262_144             # 256 KiB streaming zlib chunk slice


def _validate_png_ihdr_payload(payload: bytes) -> dict[str, int]:
    if len(payload) != 13:
        raise ValueError(f"Invalid PNG IHDR length {len(payload)} != 13")
    w, h, depth, ctype_id, comp, filt, interlace = struct.unpack(">IIBBBBB", payload)
    if w == 0 or h == 0:
        raise ValueError(f"Invalid PNG dimensions {w}x{h}")
    if w * h > _MAX_PNG_PIXELS:
        raise ValueError(f"PNG pixel count {w * h} exceeds limit {_MAX_PNG_PIXELS}")
    valid_depths = {
        0: (1, 2, 4, 8, 16),
        2: (8, 16),
        3: (1, 2, 4, 8),
        4: (8, 16),
        6: (8, 16),
    }
    if ctype_id not in valid_depths:
        raise ValueError(f"Invalid PNG color_type {ctype_id}")
    if depth not in valid_depths[ctype_id]:
        raise ValueError(f"Invalid PNG bit_depth {depth} for color_type {ctype_id}")
    if interlace not in (0, 1):
        raise ValueError(f"Invalid PNG interlace method {interlace}")
    if comp != 0:
        raise ValueError(f"Invalid PNG compression method {comp} (expected 0)")
    if filt != 0:
        raise ValueError(f"Invalid PNG filter method {filt} (expected 0)")
    ihdr = {
        "width": w,
        "height": h,
        "bit_depth": depth,
        "color_type": ctype_id,
        "compression": comp,
        "filter": filt,
        "interlace": interlace,
    }
    expected_raw_len = _expected_png_raw_len(ihdr)
    if expected_raw_len > _MAX_PNG_RAW_BYTES:
        raise ValueError(
            f"PNG IDAT expected raw size {expected_raw_len} exceeds limit {_MAX_PNG_RAW_BYTES}"
        )
    return ihdr


def _validate_png_ihdr(data: bytes) -> dict[str, int]:
    if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("Invalid PNG magic header")
    ihdr_len, ctype = struct.unpack(">I4s", data[8:16])
    if ctype != b"IHDR" or ihdr_len != 13:
        raise ValueError(f"Invalid PNG IHDR chunk (type={ctype!r}, len={ihdr_len})")
    return _validate_png_ihdr_payload(data[16:29])


def _expected_png_raw_len(ihdr: dict[str, int]) -> int:
    w, h = ihdr["width"], ihdr["height"]
    channels_count = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ihdr["color_type"]]
    bits_per_pixel = ihdr["bit_depth"] * channels_count
    if ihdr["interlace"] == 0:
        row_bytes = (w * bits_per_pixel + 7) // 8
        return h * (1 + row_bytes)
    expected_raw_len = 0
    for x0, y0, dx, dy in _ADAM7_PASSES:
        pw = (w - x0 + dx - 1) // dx if w > x0 else 0
        ph = (h - y0 + dy - 1) // dy if h > y0 else 0
        if pw > 0 and ph > 0:
            pass_row_bytes = (pw * bits_per_pixel + 7) // 8
            expected_raw_len += ph * (1 + pass_row_bytes)
    return expected_raw_len


def _validate_png_chunks(
    data: bytes, *, retain_raw: bool = True
) -> tuple[dict[str, int], Optional[bytes], Optional[bytes], Optional[tuple[int, int, int]], bytes]:
    if len(data) < 33 or data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError("Invalid PNG magic header")

    mv = memoryview(data)
    pos = 8
    ihdr: Optional[dict[str, int]] = None
    expected_raw_len = 0
    phys: Optional[tuple[int, int, int]] = None
    plte: Optional[bytes] = None
    trns: Optional[bytes] = None
    seen_idat = False
    seen_idat_end = False
    seen_iend = False
    dec = zlib.decompressobj()
    total_raw = 0
    raw_chunks: list[bytes] = []

    while pos + 8 <= len(data):
        chunk_len, ctype = struct.unpack(">I4s", mv[pos:pos + 8])
        pos += 8
        if pos + chunk_len + 4 > len(data):
            raise ValueError(f"Truncated PNG chunk {ctype!r}")
        pstart = pos
        pend = pos + chunk_len
        expected_crc = struct.unpack(">I", mv[pend:pend + 4])[0]
        actual_crc = zlib.crc32(mv[pstart:pend], zlib.crc32(ctype)) & 0xFFFFFFFF
        if actual_crc != expected_crc:
            raise ValueError(
                f"PNG CRC32 mismatch in {ctype!r}: {actual_crc:#010x} != {expected_crc:#010x}"
            )
        pos = pend + 4

        if ihdr is None and ctype != b"IHDR":
            raise ValueError(f"PNG first chunk must be IHDR, got {ctype!r}")
        if ctype != b"IDAT" and seen_idat:
            seen_idat_end = True

        if ctype == b"IHDR":
            if ihdr is not None:
                raise ValueError("Duplicate PNG IHDR chunk")
            ihdr = _validate_png_ihdr_payload(bytes(mv[pstart:pend]))
            expected_raw_len = _expected_png_raw_len(ihdr)
        elif ctype == b"PLTE":
            if plte is not None:
                raise ValueError("Duplicate PNG PLTE chunk")
            if ihdr["color_type"] in (0, 4):
                raise ValueError(f"PNG PLTE forbidden for color_type {ihdr['color_type']}")
            if seen_idat:
                raise ValueError("PNG PLTE chunk after IDAT")
            if chunk_len == 0 or chunk_len % 3 != 0 or chunk_len > 256 * 3:
                raise ValueError(f"Invalid PNG PLTE length {chunk_len}")
            plte = bytes(mv[pstart:pend])
        elif ctype == b"tRNS":
            if trns is not None:
                raise ValueError("Duplicate PNG tRNS chunk")
            if seen_idat:
                raise ValueError("PNG tRNS chunk after IDAT")
            ct = ihdr["color_type"]
            if ct in (4, 6):
                raise ValueError(f"PNG tRNS forbidden for color_type {ct}")
            if ct == 0 and chunk_len != 2:
                raise ValueError(f"Invalid PNG tRNS length {chunk_len} for color_type 0 (expected 2)")
            if ct == 2 and chunk_len != 6:
                raise ValueError(f"Invalid PNG tRNS length {chunk_len} for color_type 2 (expected 6)")
            if ct == 3 and (plte is None or chunk_len > len(plte) // 3):
                raise ValueError(f"Invalid PNG tRNS length {chunk_len} for indexed color_type 3")
            trns = bytes(mv[pstart:pend])
        elif ctype == b"pHYs" and chunk_len == 9:
            ppux, ppuy, unit = struct.unpack(">IIB", mv[pstart:pend])
            phys = (ppux, ppuy, unit)
        elif ctype == b"IDAT":
            if seen_idat_end:
                raise ValueError("Non-consecutive PNG IDAT chunk")
            seen_idat = True
            offset = pstart
            while offset < pend:
                sub_end = min(pend, offset + _PNG_ZLIB_SLICE_BYTES)
                pending: Any = mv[offset:sub_end]
                offset = sub_end
                if dec.eof:
                    raise ValueError("Trailing compressed data in PNG IDAT after zlib EOF")
                while len(pending) > 0:
                    if dec.eof:
                        raise ValueError("Trailing compressed data in PNG IDAT after zlib EOF")
                    budget = (expected_raw_len + 1) - total_raw
                    if budget <= 0:
                        raise ValueError(
                            f"PNG IDAT zlib stream exceeds expected {expected_raw_len} raw bytes"
                        )
                    step_max = budget if retain_raw else min(_PNG_ZLIB_SLICE_BYTES, budget)
                    try:
                        out_piece = dec.decompress(pending, max_length=step_max)
                    except zlib.error as exc:
                        raise ValueError(f"PNG IDAT zlib decompression failed: {exc}") from exc
                    if out_piece:
                        total_raw += len(out_piece)
                        if total_raw > expected_raw_len:
                            raise ValueError(
                                f"PNG IDAT zlib stream exceeds expected {expected_raw_len} raw bytes"
                            )
                        if retain_raw:
                            raw_chunks.append(out_piece)
                    if dec.unused_data:
                        raise ValueError(
                            f"Trailing unconsumed bytes after PNG IDAT zlib stream: {len(dec.unused_data)}"
                        )
                    pending = dec.unconsumed_tail
                    if not pending:
                        break
        elif ctype == b"IEND":
            if chunk_len != 0:
                raise ValueError(f"Invalid PNG IEND length {chunk_len} != 0")
            seen_iend = True
            break

    if ihdr is None or not seen_iend:
        raise ValueError("PNG missing IHDR or IEND")
    if ihdr["color_type"] == 3 and plte is None:
        raise ValueError("Indexed PNG missing PLTE chunk")
    if not seen_idat:
        raise ValueError("PNG missing IDAT data")

    while not dec.eof:
        budget = (expected_raw_len + 1) - total_raw
        if budget <= 0:
            raise ValueError(
                f"PNG IDAT zlib stream exceeds expected {expected_raw_len} raw bytes"
            )
        step_max = budget if retain_raw else min(_PNG_ZLIB_SLICE_BYTES, budget)
        try:
            out_piece = dec.flush(step_max)
        except zlib.error as exc:
            raise ValueError(f"PNG IDAT zlib flush failed: {exc}") from exc
        if out_piece:
            total_raw += len(out_piece)
            if total_raw > expected_raw_len:
                raise ValueError(
                    f"PNG IDAT zlib stream exceeds expected {expected_raw_len} raw bytes"
                )
            if retain_raw:
                raw_chunks.append(out_piece)
        if not out_piece and not dec.eof:
            break

    if not dec.eof:
        raise ValueError("Truncated PNG IDAT zlib stream (missing EOF)")
    if dec.unused_data:
        raise ValueError(
            f"Trailing unconsumed bytes after PNG IDAT zlib stream: {len(dec.unused_data)}"
        )
    if total_raw != expected_raw_len:
        raise ValueError(
            f"PNG IDAT raw byte length mismatch: got {total_raw} != expected {expected_raw_len}"
        )

    if not retain_raw:
        raw_out = b""
    elif len(raw_chunks) == 1:
        raw_out = raw_chunks[0]
    else:
        raw_out = b"".join(raw_chunks)
    return ihdr, plte, trns, phys, raw_out


def _parse_png_chunks(data: bytes) -> tuple[dict[str, int], Optional[bytes], Optional[bytes], Optional[tuple[int, int, int]], bytes]:
    return _validate_png_chunks(data, retain_raw=True)


def _validate_png_bytes_full(raw_bytes: bytes) -> dict[str, int]:
    """Validate PNG magic, IHDR limits, chunk CRCs, PLTE/tRNS/IEND rules, and bounded IDAT zlib stream."""
    ihdr_peek = _validate_png_ihdr(raw_bytes)
    ihdr, plte, _trns, _phys, raw = _validate_png_chunks(
        raw_bytes, retain_raw=(ihdr_peek["color_type"] == 3)
    )
    if ihdr["color_type"] == 3 and plte is not None:
        _validate_indexed_palette_indices(ihdr, plte, raw)
    del raw
    return ihdr


def _decode_png_rgba_bytes_stdlib(data: bytes) -> tuple[int, int, bytearray]:
    """Decode any standard PNG into a flat w*h*4 RGBA bytearray using pure stdlib."""
    ihdr, plte, trns, _phys, raw = _validate_png_chunks(data, retain_raw=True)
    w, h = ihdr["width"], ihdr["height"]
    ctype_id = ihdr["color_type"]
    depth = ihdr["bit_depth"]
    channels_count = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype_id]
    bits_per_pixel = depth * channels_count

    if w * h > _MAX_PNG_PIXELS:
        raise ValueError(f"PNG pixel count {w * h} exceeds limit {_MAX_PNG_PIXELS}")
    if ihdr["interlace"] == 0:
        rgba = bytearray(w * h * 4)
        end_off: list[int] = []
        row_bytes = w * 4
        for y, scan in enumerate(_iter_unfiltered_scanlines(raw, 0, w, h, bits_per_pixel, end_off)):
            row_rgba = _png_scan_row_to_rgba(scan, w, ctype_id, depth, plte, trns)
            dst_off = y * row_bytes
            rgba[dst_off:dst_off + row_bytes] = row_rgba
        raw_off = end_off[0] if end_off else 0
        if raw_off != len(raw):
            raise ValueError(f"PNG IDAT trailing bytes: {len(raw) - raw_off}")
        del raw
    else:
        rgba = bytearray(w * h * 4)
        raw_off = 0
        for x0, y0, dx, dy in _ADAM7_PASSES:
            pw = (w - x0 + dx - 1) // dx if w > x0 else 0
            ph = (h - y0 + dy - 1) // dy if h > y0 else 0
            if pw <= 0 or ph <= 0:
                continue
            end_off = []
            for py, scan in enumerate(
                _iter_unfiltered_scanlines(raw, raw_off, pw, ph, bits_per_pixel, end_off)
            ):
                row_rgba = _png_scan_row_to_rgba(scan, pw, ctype_id, depth, plte, trns)
                y = y0 + py * dy
                for px in range(pw):
                    x = x0 + px * dx
                    so = px * 4
                    do = (y * w + x) * 4
                    rgba[do:do + 4] = row_rgba[so:so + 4]
            raw_off = end_off[0] if end_off else raw_off
        if raw_off != len(raw):
            raise ValueError(f"PNG interlaced IDAT trailing bytes: {len(raw) - raw_off}")
        del raw

    return w, h, rgba


def _encode_png_rgba_stdlib(w: int, h: int, rgba: bytes | bytearray) -> bytes:
    def chunk(ctype: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(ctype + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + ctype + payload + struct.pack(">I", crc)

    raw_rows = bytearray(h * (1 + w * 4))
    row_bytes = w * 4
    for y in range(h):
        dst = y * (1 + row_bytes)
        raw_rows[dst] = 0
        src = y * row_bytes
        raw_rows[dst + 1:dst + 1 + row_bytes] = rgba[src:src + row_bytes]

    ihdr = struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)
    idat = zlib.compress(bytes(raw_rows), level=6)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"IDAT", idat)
        + chunk(b"IEND", b"")
    )


def _validate_indexed_palette_indices(
    ihdr: dict[str, int], plte: bytes, raw: bytes
) -> None:
    depth = ihdr["bit_depth"]
    num_pal = len(plte) // 3
    if num_pal >= (1 << depth):
        return
    w, h = ihdr["width"], ihdr["height"]

    def _check_rows(rows: Iterable[bytes], pw: int) -> None:
        if depth == 8:
            for scan in rows:
                if pw > 0 and max(scan[:pw]) >= num_pal:
                    bad_idx = next(b for b in scan[:pw] if b >= num_pal)
                    raise ValueError(f"PNG palette index {bad_idx} out of range ({num_pal})")
        else:
            ppb = 8 // depth
            mask = (1 << depth) - 1
            for scan in rows:
                for x in range(pw):
                    byte_val = scan[x // ppb]
                    shift = (ppb - 1 - (x % ppb)) * depth
                    idx = (byte_val >> shift) & mask
                    if idx >= num_pal:
                        raise ValueError(f"PNG palette index {idx} out of range ({num_pal})")

    if ihdr["interlace"] == 0:
        _check_rows(_iter_unfiltered_scanlines(raw, 0, w, h, depth), w)
    else:
        raw_off = 0
        for x0, y0, dx, dy in _ADAM7_PASSES:
            pw = (w - x0 + dx - 1) // dx if w > x0 else 0
            ph = (h - y0 + dy - 1) // dy if h > y0 else 0
            if pw <= 0 or ph <= 0:
                continue
            end_off: list[int] = []
            _check_rows(_iter_unfiltered_scanlines(raw, raw_off, pw, ph, depth, end_off), pw)
            raw_off = end_off[0] if end_off else raw_off


_PIL_16BIT_GRAY_MODES = ("I", "I;16", "I;16B", "I;16L")


def _pil_16bit_gray_to_l(im: Any) -> Any:
    """Scale a PIL I / I;16* image down to 8-bit 'L' via >> 8 without clamping."""
    from PIL import Image  # type: ignore
    if im.mode in ("I", "I;16"):
        return im.point(lambda v: v * (1.0 / 256.0)).convert("L")
    flat = getattr(im, "get_flattened_data", im.getdata)()
    scaled = bytes(max(0, min(255, int(v) >> 8)) for v in flat)
    return Image.frombytes("L", im.size, scaled)


def _pil_image_to_rgba_bytes(im: Any, raw_png_bytes: Optional[bytes] = None) -> bytes:
    """Convert a PIL Image to packed 8-bit RGBA bytes without clamping I;16 pixels or dropping sub/16-bit tRNS."""
    mode = getattr(im, "mode", None)
    has_pil_trns = hasattr(im, "info") and im.info.get("transparency") is not None
    if (
        raw_png_bytes is not None
        and len(raw_png_bytes) >= 26
        and raw_png_bytes.startswith(b"\x89PNG\r\n\x1a\n")
    ):
        bit_depth = raw_png_bytes[24]
        color_type = raw_png_bytes[25]
        if mode in _PIL_16BIT_GRAY_MODES or (
            has_pil_trns and color_type in (0, 2) and bit_depth != 8
        ):
            _, _, rgba = _decode_png_rgba_bytes_stdlib(raw_png_bytes)
            return bytes(rgba)
    if mode in _PIL_16BIT_GRAY_MODES:
        trns_val = im.info.get("transparency") if hasattr(im, "info") else None
        l_im = _pil_16bit_gray_to_l(im)
        if hasattr(l_im, "info"):
            l_im.info.pop("transparency", None)
        rgba_im = l_im.convert("RGBA")
        if isinstance(trns_val, int):
            buf = bytearray(rgba_im.tobytes())
            flat = getattr(im, "get_flattened_data", im.getdata)()
            for idx, val in enumerate(flat):
                if val == trns_val:
                    buf[idx * 4 + 3] = 0
            return bytes(buf)
        return rgba_im.tobytes()
    return im.convert("RGBA").tobytes()


def _pil_image_to_rgb_image(im: Any, raw_png_bytes: Optional[bytes] = None) -> Any:
    """Convert a PIL Image to an 8-bit 'RGB' PIL Image, compositing any alpha/tRNS onto white."""
    from PIL import Image  # type: ignore
    mode = getattr(im, "mode", None)
    has_trns = hasattr(im, "info") and im.info.get("transparency") is not None
    has_alpha = mode in ("RGBA", "LA", "PA", "RGBa", "La") or has_trns
    if mode in _PIL_16BIT_GRAY_MODES or has_alpha:
        if (
            raw_png_bytes is not None
            and len(raw_png_bytes) >= 26
            and raw_png_bytes.startswith(b"\x89PNG\r\n\x1a\n")
        ):
            bit_depth = raw_png_bytes[24]
            color_type = raw_png_bytes[25]
            if mode in _PIL_16BIT_GRAY_MODES or (
                has_trns and color_type in (0, 2) and bit_depth != 8
            ):
                w, h, rgba = _decode_png_rgba_bytes_stdlib(raw_png_bytes)
                rgba_im = Image.frombytes("RGBA", (w, h), bytes(rgba))
                del rgba
                white_bg = Image.new("RGBA", (w, h), (255, 255, 255, 255))
                comp = Image.alpha_composite(white_bg, rgba_im)
                del white_bg, rgba_im
                return comp.convert("RGB")
        if mode in _PIL_16BIT_GRAY_MODES:
            rgba_bytes = _pil_image_to_rgba_bytes(im, raw_png_bytes=raw_png_bytes)
            rgba_im = Image.frombytes("RGBA", im.size, rgba_bytes)
            white_bg = Image.new("RGBA", im.size, (255, 255, 255, 255))
            return Image.alpha_composite(white_bg, rgba_im).convert("RGB")
        rgba_im = im.convert("RGBA")
        white_bg = Image.new("RGBA", rgba_im.size, (255, 255, 255, 255))
        return Image.alpha_composite(white_bg, rgba_im).convert("RGB")
    return im.convert("RGB")


def _pil_image_to_rgb_bytes(im: Any, raw_png_bytes: Optional[bytes] = None) -> bytes:
    """Convert a PIL Image to packed 8-bit RGB bytes without clamping I;16 pixels to 255."""
    return _pil_image_to_rgb_image(im, raw_png_bytes=raw_png_bytes).tobytes()


_PNG_TALL_HEIGHT_PIL_BYPASS = 65_536


def _peek_png_height(path: Path) -> int:
    with path.open("rb") as fh:
        hdr = fh.read(33)
    if len(hdr) >= 24 and hdr[:8] == b"\x89PNG\r\n\x1a\n" and hdr[12:16] == b"IHDR":
        return struct.unpack(">I", hdr[20:24])[0]
    return 0


def parse_png_info(data: bytes) -> dict[str, Any]:
    """Decode PNG header, CRC32, pHYs, and pixel stats (8/16-bit, indexed, interlaced)."""
    ihdr_peek = _validate_png_ihdr(data)
    stream_tall = ihdr_peek["interlace"] == 0 and ihdr_peek["height"] > _PNG_TALL_HEIGHT_PIL_BYPASS
    ihdr, plte, trns, phys, raw = _validate_png_chunks(
        data, retain_raw=(ihdr_peek["color_type"] == 3 or stream_tall)
    )
    w, h = ihdr["width"], ihdr["height"]

    if ihdr["color_type"] == 3 and plte is not None:
        _validate_indexed_palette_indices(ihdr, plte, raw)

    lumas: list[float] = []
    non_white = 0
    non_transparent = 0
    unique_sample: set[tuple[int, ...]] = set()
    min_x, min_y, max_x, max_y = w, h, -1, -1

    if w * h > 40_000:
        step_x = max(1, w // 128)
        max_rows = max(1, 32_768 // max(1, (w + step_x - 1) // step_x))
        step_y = max(1, (h + max_rows - 1) // max_rows)
    else:
        step_x = 1
        step_y = 1

    if stream_tall:
        ctype_id = ihdr["color_type"]
        depth = ihdr["bit_depth"]
        channels_count = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype_id]
        bits_per_pixel = depth * channels_count
        end_off: list[int] = []
        corner_px = (0, 0, 0, 0)
        center_px = (0, 0, 0, 0)
        mid_y = h // 2
        coff = (w // 2) * 4
        for y, scan in enumerate(_iter_unfiltered_scanlines(raw, 0, w, h, bits_per_pixel, end_off)):
            need_sample = (y % step_y == 0)
            if not (need_sample or y == 0 or y == mid_y):
                continue
            row_rgba = _png_scan_row_to_rgba(scan, w, ctype_id, depth, plte, trns)
            if y == 0:
                corner_px = (row_rgba[0], row_rgba[1], row_rgba[2], row_rgba[3])
            if y == mid_y:
                center_px = (row_rgba[coff], row_rgba[coff + 1], row_rgba[coff + 2], row_rgba[coff + 3])
            if need_sample:
                for x in range(0, w, step_x):
                    off = x * 4
                    r, g, b, a = row_rgba[off], row_rgba[off + 1], row_rgba[off + 2], row_rgba[off + 3]
                    if a > 0:
                        non_transparent += 1
                    if (r < 250 or g < 250 or b < 250) and a > 10:
                        non_white += 1
                        if x < min_x:
                            min_x = x
                        if y < min_y:
                            min_y = y
                        if x > max_x:
                            max_x = x
                        if y > max_y:
                            max_y = y
                    luma = 0.2126 * r + 0.7152 * g + 0.0722 * b
                    lumas.append(luma)
                    if len(unique_sample) < 256:
                        unique_sample.add((r, g, b, a))
        raw_off = end_off[0] if end_off else 0
        if raw_off != len(raw):
            raise ValueError(f"PNG IDAT trailing bytes: {len(raw) - raw_off}")
        del raw
    else:
        del raw
        # Decode RGBA pixels via PIL (fast C for large/interlaced <= 65536 height) or pure stdlib
        rgba: Optional[bytearray] = None
        if (
            h <= _PNG_TALL_HEIGHT_PIL_BYPASS
            and (ihdr["interlace"] == 1 or ihdr["bit_depth"] < 8 or w * h > 200_000)
            and not (
                (ihdr["color_type"] == 0 and ihdr["bit_depth"] == 16)
                or (trns is not None and ihdr["color_type"] in (0, 2) and ihdr["bit_depth"] != 8)
            )
        ):
            try:
                from PIL import Image  # type: ignore
                with Image.open(io_bytes(data)) as im:
                    rgba = bytearray(_pil_image_to_rgba_bytes(im, raw_png_bytes=data))
            except ImportError:
                pass

        if rgba is None:
            _, _, rgba = _decode_png_rgba_bytes_stdlib(data)

        for y in range(0, h, step_y):
            row_off = y * w * 4
            for x in range(0, w, step_x):
                off = row_off + x * 4
                r, g, b, a = rgba[off], rgba[off + 1], rgba[off + 2], rgba[off + 3]
                if a > 0:
                    non_transparent += 1
                if (r < 250 or g < 250 or b < 250) and a > 10:
                    non_white += 1
                    if x < min_x:
                        min_x = x
                    if y < min_y:
                        min_y = y
                    if x > max_x:
                        max_x = x
                    if y > max_y:
                        max_y = y

                luma = 0.2126 * r + 0.7152 * g + 0.0722 * b
                lumas.append(luma)
                if len(unique_sample) < 256:
                    unique_sample.add((r, g, b, a))

        corner_px = (rgba[0], rgba[1], rgba[2], rgba[3])
        coff = ((h // 2) * w + (w // 2)) * 4
        center_px = (rgba[coff], rgba[coff + 1], rgba[coff + 2], rgba[coff + 3])

    idat_total = 0
    pos = 8
    while pos + 8 <= len(data):
        clen, ctype = struct.unpack(">I4s", data[pos:pos + 8])
        if ctype == b"IDAT":
            idat_total += clen
        pos += 8 + clen + 4
        if ctype == b"IEND":
            break

    return {
        "width": w,
        "height": h,
        "bit_depth": ihdr["bit_depth"],
        "color_type": ihdr["color_type"],
        "interlace": ihdr["interlace"],
        "phys": phys,
        "idat_bytes": idat_total,
        "sampled_pixels": len(lumas),
        "non_white_pixels": non_white,
        "non_transparent_pixels": non_transparent,
        "luma_min": min(lumas) if lumas else 0.0,
        "luma_max": max(lumas) if lumas else 0.0,
        "luma_stddev": statistics.pstdev(lumas) if len(lumas) > 1 else 0.0,
        "unique_colors_sample": len(unique_sample),
        "bbox_non_white": (min_x, min_y, max_x, max_y) if max_x >= 0 else None,
        "corner_pixel": corner_px,
        "center_pixel": center_px,
    }


def _classify_loda_payload(payload: bytes, endian: str = "<") -> str:
    if len(payload) < 12:
        raise ValueError(f"Truncated loda chunk ({len(payload)} < 12 bytes)")
    if len(payload) >= 20:
        u0, u1, u2, u3, _u4 = struct.unpack(f"{endian}IIIII", payload[:20])
        if (
            u0 == len(payload)
            and u1 <= 1024
            and u2 >= 20
            and u3 >= u2 + u1 * 4
            and u3 + u1 * 4 <= len(payload)
        ):
            if u1 == 0:
                if (u0 == 20 and u2 == 20 and u3 == 20) or (
                    u0 == 24
                    and u2 == 20
                    and u3 == 24
                    and struct.unpack(f"{endian}I", payload[20:24])[0] == 24
                ):
                    return "20B_authentic"
            else:
                offsets = struct.unpack(f"{endian}{u1}I", payload[u2:u2 + u1 * 4])
                if all(offsets[i] <= offsets[i + 1] for i in range(u1 - 1)):
                    has_end = u3 >= u2 + (u1 + 1) * 4
                    end_off = (
                        struct.unpack(f"{endian}I", payload[u2 + u1 * 4:u2 + (u1 + 1) * 4])[0]
                        if has_end
                        else None
                    )
                    data_start = u3 + u1 * 4
                    # (a) Authentic CorelDRAW post-table layout (v6-v15 RIFF & v16-v26 ZCF)
                    if all(data_start <= off <= len(payload) for off in offsets) and (
                        not has_end or offsets[-1] <= end_off <= len(payload)
                    ):
                        return "20B_authentic"
                    # (b) Pre-table 20B layout (payloads in [20, u2), tables end at len(payload))
                    if (
                        u3 + u1 * 4 == len(payload)
                        and all(20 <= off <= u2 for off in offsets)
                        and (not has_end or offsets[-1] <= end_off <= u2)
                    ):
                        return "20B_authentic"
    return "12B_synthetic"


_RIFF_CMPR_MAX_BYTES = 64 * 1024 * 1024
_RIFF_MAX_NESTING_DEPTH = 32


def _bounded_riff_zlib_decompress(payload: bytes, max_bytes: int, label: str) -> bytes:
    if max_bytes < 0:
        raise ValueError(
            f"{label} exceeds cumulative RIFF decompression limit ({_RIFF_CMPR_MAX_BYTES} bytes)"
        )
    try:
        dec = zlib.decompressobj()
        out = dec.decompress(payload, max_length=max_bytes + 1)
        if not dec.eof and not dec.unconsumed_tail and len(out) <= max_bytes:
            out += dec.flush()
    except zlib.error as exc:
        raise ValueError(f"Corrupt zlib stream in {label}: {exc}") from exc
    if dec.unconsumed_tail or len(out) > max_bytes:
        raise ValueError(
            f"{label} decompressed size exceeds limit ({_RIFF_CMPR_MAX_BYTES} bytes)"
        )
    if not dec.eof:
        raise ValueError(f"Truncated zlib stream in {label} (missing EOF)")
    return out


def parse_riff_chunks(data: bytes) -> dict[str, Any]:
    """Parse RIFF/RIFX chunk tree, version, payloads, and loda header layout (12B vs 20B)."""
    if len(data) < 12 or data[:4] not in (b"RIFF", b"RIFX"):
        raise ValueError("Not a RIFF/RIFX container")
    endian = "<" if data[:4] == b"RIFF" else ">"
    declared_len = struct.unpack(f"{endian}I", data[4:8])[0]
    if declared_len < 4 or 8 + declared_len > len(data):
        raise ValueError(
            f"Truncated RIFF container: declared 8+{declared_len}={8 + declared_len} > {len(data)}"
        )
    form_type = data[8:12].decode("ascii", errors="replace")

    chunks: list[str] = []
    loda_headers: list[dict[str, Any]] = []
    chunk_payloads: list[tuple[str, bytes]] = []
    payloads_by_fourcc: dict[str, list[bytes]] = {}
    version: Optional[int] = None
    total_decompressed = 0

    def walk(
        buf: bytes,
        start: int,
        end: int,
        size_table: Optional[list[int]] = None,
        depth: int = 0,
    ) -> None:
        nonlocal version, total_decompressed
        if depth > _RIFF_MAX_NESTING_DEPTH:
            raise ValueError("RIFF LIST:cmpr nesting exceeds maximum depth")
        pos = start
        while pos < end:
            if pos + 8 > end:
                raise ValueError(f"Truncated RIFF chunk header at offset {pos} (end={end})")
            fourcc = buf[pos:pos + 4].decode("ascii", errors="replace")
            raw_size = struct.unpack(f"{endian}I", buf[pos + 4:pos + 8])[0]
            if size_table is not None:
                if raw_size >= len(size_table):
                    raise ValueError(
                        f"LIST:cmpr CPng size table index {raw_size} out of bounds (len={len(size_table)})"
                    )
                size = size_table[raw_size]
            else:
                size = raw_size
            payload_start = pos + 8
            payload_end = payload_start + size
            if payload_end > end:
                raise ValueError(
                    f"Truncated RIFF chunk {fourcc!r} at {pos}: payload_end {payload_end} > {end}"
                )
            if fourcc == "LIST":
                if payload_start + 4 > payload_end:
                    raise ValueError(f"Truncated RIFF LIST chunk at {pos} (size={size})")
                list_type = buf[payload_start:payload_start + 4].decode("ascii", errors="replace")
                chunks.append(f"LIST:{list_type}")
                if list_type == "cmpr":
                    if payload_start + 24 > payload_end:
                        raise ValueError(f"Truncated LIST:cmpr header at {pos}")
                    # 20-byte cmpr header after LIST type `cmpr`: compressed_len, uncomp_len, blocks_len, sig(4), method(2), reserved(2)
                    cmpr_hdr_off = payload_start + 4
                    comp_len, uncomp_len, blocks_len, _sig, _method, _res = struct.unpack(
                        f"{endian}III4sHH", buf[cmpr_hdr_off:cmpr_hdr_off + 20]
                    )
                    if uncomp_len > _RIFF_CMPR_MAX_BYTES:
                        raise ValueError(
                            f"LIST:cmpr declared uncompressed size {uncomp_len} exceeds limit {_RIFF_CMPR_MAX_BYTES}"
                        )
                    if total_decompressed + uncomp_len > _RIFF_CMPR_MAX_BYTES:
                        raise ValueError(
                            f"LIST:cmpr declared uncompressed size {uncomp_len} exceeds cumulative limit {_RIFF_CMPR_MAX_BYTES}"
                        )
                    stream_start = cmpr_hdr_off + 20
                    stream_end = stream_start + comp_len
                    if stream_end > payload_end:
                        raise ValueError(
                            f"Truncated LIST:cmpr compressed stream: {stream_end} > {payload_end}"
                        )
                    dec = _bounded_riff_zlib_decompress(
                        buf[stream_start:stream_end],
                        _RIFF_CMPR_MAX_BYTES - total_decompressed,
                        "LIST:cmpr",
                    )
                    if uncomp_len > 0 and len(dec) != uncomp_len:
                        raise ValueError(
                            f"LIST:cmpr decompressed size {len(dec)} != declared {uncomp_len}"
                        )
                    total_decompressed += len(dec)
                    sub_table: Optional[list[int]] = None
                    if blocks_len > 0:
                        if stream_end + blocks_len > payload_end:
                            raise ValueError(
                                f"Truncated LIST:cmpr blocks table: {stream_end + blocks_len} > {payload_end}"
                            )
                        raw_tbl = _bounded_riff_zlib_decompress(
                            buf[stream_end:stream_end + blocks_len],
                            _RIFF_CMPR_MAX_BYTES - total_decompressed,
                            "LIST:cmpr blocks table",
                        )
                        total_decompressed += len(raw_tbl)
                        if len(raw_tbl) % 4 != 0:
                            raise ValueError(
                                f"LIST:cmpr CPng blocks table byte length {len(raw_tbl)} is not a multiple of 4"
                            )
                        sub_table = [
                            struct.unpack(f"{endian}I", raw_tbl[i:i + 4])[0]
                            for i in range(0, len(raw_tbl), 4)
                        ]
                    walk(dec, 0, len(dec), sub_table, depth + 1)
                else:
                    walk(buf, payload_start + 4, payload_end, size_table, depth + 1)
            else:
                chunks.append(fourcc)
                payload = buf[payload_start:payload_end]
                chunk_payloads.append((fourcc, payload))
                payloads_by_fourcc.setdefault(fourcc, []).append(payload)
                if fourcc == "vrsn" and len(payload) >= 2 and version is None:
                    version = struct.unpack(f"{endian}H", payload[:2])[0]
                elif fourcc == "loda":
                    if len(payload) < 12:
                        raise ValueError(f"Truncated loda chunk ({len(payload)} < 12 bytes)")
                    num_args, off1, off2 = struct.unpack(f"{endian}III", payload[:12])
                    header_kind = _classify_loda_payload(payload, endian)
                    loda_headers.append({
                        "byte_len": len(payload),
                        "first_3_u32": (num_args, off1, off2),
                        "header_kind": header_kind,
                    })
            pos = payload_start + size + (size & 1)

    walk(data, 12, 8 + declared_len)
    return {
        "container": data[:4].decode("ascii"),
        "form_type": form_type,
        "version": version,
        "chunks": chunks,
        "loda_headers": loda_headers,
        "chunk_payloads": chunk_payloads,
        "payloads_by_fourcc": payloads_by_fourcc,
    }


_ZIP_MAX_ENTRY_BYTES = 64 * 1024 * 1024
_ZIP_MAX_TOTAL_BYTES = 128 * 1024 * 1024
_ZIP_MAX_COMPRESSION_RATIO = 100
_ZIP_RATIO_MIN_BYTES = 256 * 1024


def _validate_zip_entry_name(name: str) -> None:
    if not name or "\x00" in name:
        raise ValueError(f"Unsafe empty or NUL ZIP entry name: {name!r}")
    if name.startswith("/") or name.startswith("\\"):
        raise ValueError(f"Unsafe ZIP entry path (leading separator): {name!r}")
    if len(name) >= 2 and name[1] == ":":
        raise ValueError(f"Unsafe ZIP entry path (Windows drive prefix): {name!r}")
    segments = re.split(r"[\\/]", name)
    if ".." in segments:
        raise ValueError(f"Unsafe ZIP entry path (traversal '..'): {name!r}")


def _validate_zip_infolist(zf: zipfile.ZipFile) -> dict[str, zipfile.ZipInfo]:
    info_by_name: dict[str, zipfile.ZipInfo] = {}
    total_declared = 0
    for info in zf.infolist():
        _validate_zip_entry_name(info.filename)
        if info.file_size > _ZIP_MAX_ENTRY_BYTES:
            raise ValueError(
                f"ZIP entry {info.filename!r} declared size {info.file_size} exceeds {_ZIP_MAX_ENTRY_BYTES}"
            )
        total_declared += info.file_size
        if total_declared > _ZIP_MAX_TOTAL_BYTES:
            raise ValueError(
                f"ZIP cumulative declared size {total_declared} exceeds {_ZIP_MAX_TOTAL_BYTES}"
            )
        if (
            info.file_size > _ZIP_RATIO_MIN_BYTES
            and info.file_size > max(1, info.compress_size) * _ZIP_MAX_COMPRESSION_RATIO
        ):
            raise ValueError(
                f"ZIP entry {info.filename!r} declared compression ratio exceeds {_ZIP_MAX_COMPRESSION_RATIO}:1"
            )
        info_by_name[info.filename] = info
    return info_by_name


def _bounded_zip_verify_crcs(zf: zipfile.ZipFile) -> Optional[str]:
    total_read = 0
    for info in zf.infolist():
        if info.is_dir():
            continue
        entry_read = 0
        try:
            with zf.open(info, "r") as fp:
                while True:
                    chunk = fp.read(65536)
                    if not chunk:
                        break
                    entry_read += len(chunk)
                    total_read += len(chunk)
                    if entry_read > _ZIP_MAX_ENTRY_BYTES:
                        raise ValueError(
                            f"ZIP entry {info.filename!r} exceeds {_ZIP_MAX_ENTRY_BYTES} bytes"
                        )
                    if total_read > _ZIP_MAX_TOTAL_BYTES:
                        raise ValueError(
                            f"ZIP total decompressed size exceeds {_ZIP_MAX_TOTAL_BYTES} bytes"
                        )
                    if (
                        entry_read > _ZIP_RATIO_MIN_BYTES
                        and entry_read > max(1, info.compress_size) * _ZIP_MAX_COMPRESSION_RATIO
                    ):
                        raise ValueError(
                            f"ZIP entry {info.filename!r} compression ratio exceeds {_ZIP_MAX_COMPRESSION_RATIO}:1"
                        )
        except (zipfile.BadZipFile, zlib.error, RuntimeError, NotImplementedError, EOFError):
            return info.filename
    return None


def _bounded_zip_read_info(
    zf: zipfile.ZipFile,
    info: zipfile.ZipInfo,
    *,
    remaining_budget: list[int],
    cache: Optional[dict[str, bytes]] = None,
) -> bytes:
    if cache is not None and info.filename in cache:
        return cache[info.filename]
    if info.file_size > _ZIP_MAX_ENTRY_BYTES:
        raise ValueError(
            f"ZIP entry {info.filename!r} declared size {info.file_size} exceeds {_ZIP_MAX_ENTRY_BYTES}"
        )
    if info.file_size > remaining_budget[0]:
        raise ValueError(
            f"ZIP total decompressed reads exceed {_ZIP_MAX_TOTAL_BYTES} bytes"
        )
    chunks: list[bytes] = []
    entry_read = 0
    try:
        with zf.open(info, "r") as fp:
            while True:
                chunk = fp.read(65536)
                if not chunk:
                    break
                entry_read += len(chunk)
                remaining_budget[0] -= len(chunk)
                if entry_read > _ZIP_MAX_ENTRY_BYTES:
                    raise ValueError(
                        f"ZIP entry {info.filename!r} exceeds {_ZIP_MAX_ENTRY_BYTES} bytes"
                    )
                if remaining_budget[0] < 0:
                    raise ValueError(
                        f"ZIP total decompressed reads exceed {_ZIP_MAX_TOTAL_BYTES} bytes"
                    )
                if (
                    entry_read > _ZIP_RATIO_MIN_BYTES
                    and entry_read > max(1, info.compress_size) * _ZIP_MAX_COMPRESSION_RATIO
                ):
                    raise ValueError(
                        f"ZIP entry {info.filename!r} compression ratio exceeds {_ZIP_MAX_COMPRESSION_RATIO}:1"
                    )
                chunks.append(chunk)
    except (zipfile.BadZipFile, zlib.error, RuntimeError, NotImplementedError, EOFError) as exc:
        raise ValueError(f"Corrupt ZIP entry {info.filename!r}: {exc}") from exc
    payload = b"".join(chunks)
    if cache is not None:
        cache[info.filename] = payload
    return payload


def parse_zip_entries(data: bytes) -> dict[str, Any]:
    with zipfile.ZipFile(io_bytes(data), "r") as zf:
        info_by_name = _validate_zip_infolist(zf)
        bad = _bounded_zip_verify_crcs(zf)
        names = list(info_by_name.keys())
        if bad is not None:
            return {
                "bad_crc_entry": bad,
                "entries": names,
                "mimetype": None,
                "riff_info": None,
            }
        names_lower = {n.lower(): n for n in names}
        budget = [_ZIP_MAX_TOTAL_BYTES]
        mime_entry = names_lower.get("mimetype")
        mimetype = (
            _bounded_zip_read_info(zf, info_by_name[mime_entry], remaining_budget=budget)
            .decode("utf-8", errors="replace")
            .strip()
            if mime_entry
            else None
        )
        riff_info = None
        for cand in ("content/root.dat", "content/riffdata.cdr"):
            matched = names_lower.get(cand)
            if matched is not None:
                raw = _bounded_zip_read_info(zf, info_by_name[matched], remaining_budget=budget)
                if raw[:4] in (b"RIFF", b"RIFX"):
                    riff_info = parse_riff_chunks(raw)
                break
        return {
            "bad_crc_entry": bad,
            "entries": names,
            "mimetype": mimetype,
            "riff_info": riff_info,
        }


def io_bytes(data: bytes):
    import io
    return io.BytesIO(data)


def parse_pdf_structure(data: bytes) -> dict[str, Any]:
    if not data.startswith(b"%PDF-"):
        raise ValueError("Missing %PDF- signature")
    first_line = data[:64].split(b"\n", 1)[0].split(b"\r", 1)[0].decode("ascii", errors="replace").strip()
    m_ver = re.match(r"^%PDF-\d+\.\d+", first_line)
    header_line = m_ver.group(0) if m_ver else first_line[:24]
    text = data.decode("latin-1")

    # Strip % comment lines/tails and (...) string literal contents outside streams while retaining %PDF-x.y and %%EOF
    kept_lines: list[str] = []
    in_stream = False
    stream_count = 0
    paren_depth = 0
    for ln in re.split(r"\r\n|\r|\n", text):
        if in_stream:
            m_end = re.search(r"endstream(?=\s|[%()\[\]<>{}/]|$)", ln)
            if not m_end:
                continue
            in_stream = False
            kept_lines.append("endstream")
            ln = ln[m_end.end():]
            if not ln.strip():
                continue
        stripped_l = ln.lstrip()
        if paren_depth == 0 and stripped_l.startswith("%"):
            if ln.startswith("%PDF-"):
                m_hdr = re.match(r"^%PDF-\d+\.\d+", ln)
                if m_hdr:
                    kept_lines.append(m_hdr.group(0))
            elif re.match(r"^%%EOF(?:\s|$)", stripped_l):
                kept_lines.append("%%EOF")
            continue
        cleaned: list[str] = []
        escaped = False
        pos = 0
        n_ln = len(ln)
        while pos < n_ln:
            ch = ln[pos]
            if paren_depth > 0:
                if escaped:
                    cleaned.append(" ")
                    escaped = False
                elif ch == "\\":
                    cleaned.append(" ")
                    escaped = True
                elif ch == "(":
                    paren_depth += 1
                    cleaned.append(" ")
                elif ch == ")":
                    paren_depth -= 1
                    cleaned.append(")" if paren_depth == 0 else " ")
                else:
                    cleaned.append(" ")
                pos += 1
            else:
                if ch == "(":
                    paren_depth = 1
                    cleaned.append("(")
                    pos += 1
                elif ch == "%":
                    break
                else:
                    cleaned.append(ch)
                    pos += 1
                    if ch == "m" and (pos == n_ln or ln[pos].isspace()):
                        buf_so_far = "".join(cleaned)
                        if re.search(r"(?:^|\s|>>)stream$", buf_so_far):
                            m_end = re.search(r"endstream(?=\s|[%()\[\]<>{}/]|$)", ln[pos:])
                            if m_end is not None:
                                cleaned.append(" endstream")
                                stream_count += 1
                                pos += m_end.end()
                            else:
                                stream_count += 1
                                in_stream = True
                                break
        clean_line = "".join(cleaned).rstrip() if in_stream else "".join(cleaned)
        if clean_line:
            kept_lines.append(clean_line)

    filtered = "\n".join(kept_lines)
    pdf_name_end = r"(?=\s|[()\[\]<>{}/%]|$)"
    return {
        "header": header_line,
        "has_xref": bool(
            re.search(r"(?m)^xref\b", filtered)
            or re.search(rf"/Type\s*/XRef{pdf_name_end}", filtered)
        ),
        "has_eof": bool(re.search(r"(?m)^%%EOF\b", filtered)),
        "has_catalog": bool(re.search(rf"/Type\s*/Catalog{pdf_name_end}", filtered)),
        "has_pages": bool(re.search(rf"/Type\s*/Pages{pdf_name_end}", filtered)),
        "has_mediabox": bool(re.search(r"/MediaBox\s*\[", filtered)),
        "has_pdfx_version": bool(re.search(rf"/GTS_PDFXVersion{pdf_name_end}", filtered)),
        "has_output_intents": bool(re.search(rf"/OutputIntents{pdf_name_end}", filtered)),
        "has_trapped": bool(re.search(rf"/Trapped\s*/(?:True|False|Unknown){pdf_name_end}", filtered)),
        "has_cid_font_type2": bool(re.search(rf"/Subtype\s*/CIDFontType2{pdf_name_end}", filtered)),
        "has_tounicode": bool(re.search(rf"/ToUnicode\s+\d+\s+\d+\s+R{pdf_name_end}", filtered)),
        "has_type0": bool(re.search(rf"/Subtype\s*/Type0{pdf_name_end}", filtered)),
        "has_device_cmyk": bool(re.search(rf"/DeviceCMYK{pdf_name_end}", filtered)),
        "has_device_rgb": bool(re.search(rf"/DeviceRGB{pdf_name_end}", filtered)),
        "has_separation": bool(re.search(rf"/Separation{pdf_name_end}", filtered)),
        "stream_count": stream_count,
    }


def parse_svg_structure(data: bytes) -> dict[str, Any]:
    root = ET.fromstring(data.decode("utf-8"))
    tag_name = root.tag.split("}", 1)[-1]
    if tag_name != "svg":
        raise ValueError(f"Expected <svg> root, got <{tag_name}>")
    counts: dict[str, int] = {}
    layers: list[str] = []
    for elem in root.iter():
        local = elem.tag.split("}", 1)[-1]
        counts[local] = counts.get(local, 0) + 1
        dl = elem.attrib.get("data-layer")
        if dl:
            layers.append(dl)
    return {
        "viewBox": root.attrib.get("viewBox"),
        "width": root.attrib.get("width"),
        "height": root.attrib.get("height"),
        "counts": counts,
        "layers": layers,
        "vector_elements": sum(counts.get(k, 0) for k in ("path", "rect", "circle", "ellipse", "polygon", "polyline", "line")),
    }


def parse_dxf_structure(data: bytes) -> dict[str, Any]:
    lines = [ln.strip() for ln in data.decode("utf-8", errors="replace").splitlines()]
    sections: list[str] = []
    entities: list[str] = []
    in_entities = False
    i = 0
    while i + 1 < len(lines):
        code, val = lines[i], lines[i + 1]
        if code == "0" and val == "SECTION" and i + 3 < len(lines) and lines[i + 2] == "2":
            sec = lines[i + 3]
            sections.append(sec)
            in_entities = (sec == "ENTITIES")
        elif code == "0" and val == "ENDSEC":
            in_entities = False
        elif in_entities and code == "0":
            entities.append(val)
        i += 2
    return {
        "sections": sections,
        "entities": entities,
        "has_eof": "EOF" in lines,
    }


def parse_plt_commands(data: bytes) -> dict[str, Any]:
    raw = data.decode("ascii", errors="replace")
    cmds = [c.strip() for c in raw.split(";") if c.strip()]
    opcodes = [c[:2].upper() for c in cmds if len(c) >= 2]
    return {
        "command_count": len(cmds),
        "opcodes": opcodes,
        "has_ci_or_aa": any(op in ("CI", "AA") for op in opcodes),
        "has_lb": "LB" in opcodes,
        "has_pu_pd": ("PU" in opcodes and "PD" in opcodes),
    }


def inspect_coree_zip(path: Path) -> tuple[dict[str, Any], dict[str, Any]]:
    with zipfile.ZipFile(path, "r") as zf:
        info_by_name = _validate_zip_infolist(zf)
        bad = _bounded_zip_verify_crcs(zf)
        if bad is not None:
            raise ValueError(f"Corrupt ZIP entry in {path}: {bad}")
        budget = [_ZIP_MAX_TOTAL_BYTES]
        manifest_info = info_by_name.get("manifest.json")
        document_info = info_by_name.get("document.json")
        if manifest_info is None or document_info is None:
            raise KeyError(f"Missing manifest.json or document.json in {path}")
        manifest = json.loads(
            _bounded_zip_read_info(zf, manifest_info, remaining_budget=budget).decode("utf-8")
        )
        document = json.loads(
            _bounded_zip_read_info(zf, document_info, remaining_budget=budget).decode("utf-8")
        )
    if (
        not isinstance(manifest, dict)
        or manifest.get("app") != "coree"
        or type(manifest.get("format_version")) is not int
        or manifest.get("format_version") != 1
    ):
        raise ValueError(f"Invalid .coree manifest: {manifest}")
    if not isinstance(document, dict):
        raise ValueError(f"Invalid .coree document.json (expected dict, got {type(document).__name__})")
    return manifest, document


# ---------------------------------------------------------------------------
# External Verification Oracles (Ghostscript, Poppler, file, dpkg-deb, PIL)
# ---------------------------------------------------------------------------

def _require_tool(name: str) -> Optional[Path]:
    found = shutil.which(name)
    return Path(found) if found else None


def _compute_png_alpha_stats(png_path: Path) -> dict[str, int]:
    raw_bytes = png_path.read_bytes()
    ihdr_peek = _validate_png_bytes_full(raw_bytes)
    if ihdr_peek["height"] > _PNG_TALL_HEIGHT_PIL_BYPASS and ihdr_peek["interlace"] == 0:
        ihdr, plte, trns, _phys, raw = _validate_png_chunks(raw_bytes, retain_raw=True)
        del raw_bytes
        w, h = ihdr["width"], ihdr["height"]
        ctype_id = ihdr["color_type"]
        depth = ihdr["bit_depth"]
        channels_count = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype_id]
        bits_per_pixel = depth * channels_count
        total = w * h
        transparent = 0
        non_transparent = 0
        opaque_white = 0
        near_white_opaque = 0
        end_off: list[int] = []
        for scan in _iter_unfiltered_scanlines(raw, 0, w, h, bits_per_pixel, end_off):
            row_rgba = _png_scan_row_to_rgba(scan, w, ctype_id, depth, plte, trns)
            for off in range(0, len(row_rgba), 4):
                r, g, b, a = row_rgba[off], row_rgba[off + 1], row_rgba[off + 2], row_rgba[off + 3]
                if a == 0:
                    transparent += 1
                else:
                    non_transparent += 1
                    if r == 255 and g == 255 and b == 255 and a == 255:
                        opaque_white += 1
                    if r >= 250 and g >= 250 and b >= 250:
                        near_white_opaque += 1
        raw_off = end_off[0] if end_off else 0
        if raw_off != len(raw):
            raise ValueError(f"PNG IDAT trailing bytes: {len(raw) - raw_off}")
        del raw
        return {
            "total_pixels": total,
            "transparent_pixels": transparent,
            "non_transparent_pixels": non_transparent,
            "opaque_white_pixels": opaque_white,
            "near_white_opaque_pixels": near_white_opaque,
        }

    buf: Optional[bytes | bytearray] = None
    if ihdr_peek["height"] <= _PNG_TALL_HEIGHT_PIL_BYPASS:
        try:
            from PIL import Image  # type: ignore
            with Image.open(io_bytes(raw_bytes)) as im:
                w, h = im.size
                buf = _pil_image_to_rgba_bytes(im, raw_png_bytes=raw_bytes)
            del raw_bytes
        except ImportError:
            pass
    if buf is None:
        w, h, buf = _decode_png_rgba_bytes_stdlib(raw_bytes)
        del raw_bytes

    total = w * h
    transparent = 0
    non_transparent = 0
    opaque_white = 0
    near_white_opaque = 0
    for off in range(0, len(buf), 4):
        r, g, b, a = buf[off], buf[off + 1], buf[off + 2], buf[off + 3]
        if a == 0:
            transparent += 1
        else:
            non_transparent += 1
            if r == 255 and g == 255 and b == 255 and a == 255:
                opaque_white += 1
            if r >= 250 and g >= 250 and b >= 250:
                near_white_opaque += 1
    return {
        "total_pixels": total,
        "transparent_pixels": transparent,
        "non_transparent_pixels": non_transparent,
        "opaque_white_pixels": opaque_white,
        "near_white_opaque_pixels": near_white_opaque,
    }


def run_ghostscript_render(
    input_path: Path,
    out_png: Path,
    *,
    dpi: int = 150,
    device: str = "pngalpha",
    timeout_scale: float = 1.0,
) -> dict[str, Any]:
    gs_bin = _require_tool("gs")
    if gs_bin is None:
        return {"ok": False, "tool_missing": "gs", "error": "Ghostscript (/usr/bin/gs) not found"}
    if not input_path.is_file():
        return {
            "ok": False,
            "returncode": -1,
            "stdout": "",
            "stderr": f"Input file not found: {input_path}",
            "png_info": None,
            "alpha_stats": None,
        }
    with tempfile.TemporaryDirectory(prefix="coree_gs_") as tmp_dir:
        tmp_root = Path(tmp_dir)
        staged_in = tmp_root / f"input{input_path.suffix.lower()}"
        staged_out = tmp_root / "output.png"
        shutil.copyfile(input_path, staged_in)
        cmd = [
            str(gs_bin),
            "-dSAFER",
            "-dBATCH",
            "-dNOPAUSE",
        ]
        if input_path.suffix.lower() in {".eps", ".ps"}:
            cmd.append("-dEPSCrop")
        cmd.extend([
            f"-sDEVICE={device}",
            f"-r{dpi}",
            f"-sOutputFile={staged_out}",
            str(staged_in),
        ])
        res = run_cmd(cmd, cwd=tmp_root, timeout=30.0, timeout_scale=timeout_scale)
        png_info: Optional[dict[str, Any]] = None
        alpha_stats: Optional[dict[str, int]] = None
        if res.returncode == 0 and staged_out.is_file():
            out_png.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(staged_out, out_png)
            png_info = parse_png_info(out_png.read_bytes())
            alpha_stats = _compute_png_alpha_stats(out_png)
    return {
        "ok": res.returncode == 0 and out_png.is_file() and png_info is not None,
        "returncode": res.returncode,
        "stdout": res.stdout,
        "stderr": res.stderr,
        "png_info": png_info,
        "alpha_stats": alpha_stats,
    }


def run_poppler_info(pdf_path: Path, *, timeout_scale: float = 1.0) -> dict[str, Any]:
    tool = _require_tool("pdfinfo")
    if tool is None:
        return {"ok": False, "tool_missing": "pdfinfo", "error": "pdfinfo not found"}
    with tempfile.TemporaryDirectory(prefix="coree_pdfinfo_") as tmp_dir:
        res = run_cmd(
            [str(tool), "-box", str(pdf_path.resolve())],
            cwd=Path(tmp_dir),
            timeout=20.0,
            timeout_scale=timeout_scale,
        )
    fields: dict[str, str] = {}
    for line in res.stdout.splitlines():
        if ":" in line:
            k, v = line.split(":", 1)
            fields[k.strip()] = v.strip()
    return {
        "ok": res.returncode == 0,
        "returncode": res.returncode,
        "fields": fields,
        "raw": res.stdout,
        "stderr": res.stderr,
    }


_PDF_FONT_ROW_RX = re.compile(
    r"^(?P<name>.+?)\s+"
    r"(?P<type>CID TrueType \(OT\)|CID TrueType|CID Type 0C \(OT\)|CID Type 0C|CID Type 0 \(OT\)|CID Type 0|TrueType \(CID\)|Type 1C \(OT\)|Type 1C|Type 1|Type 0|Type 3|TrueType)\s+"
    r"(?P<encoding>\S+)\s+"
    r"(?P<emb>yes|no)\s+(?P<sub>yes|no)\s+(?P<uni>yes|no)\s+"
    r"(?P<obj>\d+)\s+(?P<gen>\d+)\s*$"
)


def run_poppler_fonts(pdf_path: Path, *, timeout_scale: float = 1.0) -> dict[str, Any]:
    tool = _require_tool("pdffonts")
    if tool is None:
        return {"ok": False, "tool_missing": "pdffonts", "error": "pdffonts not found"}
    with tempfile.TemporaryDirectory(prefix="coree_pdffonts_") as tmp_dir:
        res = run_cmd(
            [str(tool), str(pdf_path.resolve())],
            cwd=Path(tmp_dir),
            timeout=20.0,
            timeout_scale=timeout_scale,
        )
    fonts: list[dict[str, str]] = []
    lines = res.stdout.splitlines()

    # Compute column spans from dashed separator line[1] as fallback
    col_spans: list[tuple[int, int]] = []
    if len(lines) >= 2 and set(lines[1].strip()) <= {"-", " "}:
        col_spans = [(m.start(), m.end()) for m in re.finditer(r"-+", lines[1])]

    for line in lines[2:]:
        if not line.strip():
            continue
        m = _PDF_FONT_ROW_RX.match(line)
        if m:
            fonts.append({
                "name": m.group("name").strip(),
                "type": m.group("type").strip(),
                "encoding": m.group("encoding").strip(),
                "emb": m.group("emb"),
                "sub": m.group("sub"),
                "uni": m.group("uni"),
            })
        elif len(col_spans) >= 6:
            # Slice by separator spans; last column may extend beyond header width
            vals = [line[s:e].strip() for s, e in col_spans[:6]]
            if len(vals) == 6 and vals[3] in ("yes", "no"):
                fonts.append({
                    "name": vals[0],
                    "type": vals[1],
                    "encoding": vals[2],
                    "emb": vals[3],
                    "sub": vals[4],
                    "uni": vals[5],
                })
    return {
        "ok": res.returncode == 0,
        "returncode": res.returncode,
        "fonts": fonts,
        "raw": res.stdout,
        "stderr": res.stderr,
    }


def run_poppler_text(pdf_path: Path, *, timeout_scale: float = 1.0) -> dict[str, Any]:
    tool = _require_tool("pdftotext")
    if tool is None:
        return {"ok": False, "tool_missing": "pdftotext", "error": "pdftotext not found"}
    with tempfile.TemporaryDirectory(prefix="coree_pdftotext_") as tmp_dir:
        res = run_cmd(
            [str(tool), "-enc", "UTF-8", str(pdf_path.resolve()), "-"],
            cwd=Path(tmp_dir),
            timeout=20.0,
            timeout_scale=timeout_scale,
        )
    return {
        "ok": res.returncode == 0,
        "returncode": res.returncode,
        "text": res.stdout,
        "stderr": res.stderr,
    }


def run_pdftoppm(
    pdf_path: Path,
    out_prefix: Path,
    *,
    dpi: int = 150,
    single_file: bool = True,
    timeout_scale: float = 1.0,
) -> dict[str, Any]:
    tool = _require_tool("pdftoppm")
    if tool is None:
        return {"ok": False, "tool_missing": "pdftoppm", "error": "pdftoppm not found"}
    prefix_name = f"{out_prefix.name}-"
    prefix_len = len(prefix_name)

    def _matching_pages() -> list[Path]:
        if not out_prefix.parent.is_dir():
            return []
        return [
            p
            for p in out_prefix.parent.iterdir()
            if p.is_file()
            and p.name.startswith(prefix_name)
            and p.suffix == ".png"
            and p.stem[prefix_len:].isascii()
            and p.stem[prefix_len:].isdigit()
        ]

    pre_existing: dict[Path, tuple[int, int]] = {}
    if not single_file:
        for p in _matching_pages():
            st = p.stat()
            pre_existing[p] = (st.st_ino, st.st_mtime_ns)

    cmd = [str(tool), "-png", "-r", str(dpi)]
    if single_file:
        cmd.append("-singlefile")
    cmd.extend([str(pdf_path.resolve()), str(out_prefix.resolve())])
    with tempfile.TemporaryDirectory(prefix="coree_pdftoppm_") as tmp_dir:
        res = run_cmd(cmd, cwd=Path(tmp_dir), timeout=30.0, timeout_scale=timeout_scale)
    if single_file:
        out_png = Path(f"{out_prefix}.png")
        pages: list[Path] = [out_png] if (res.returncode == 0 and out_png.is_file()) else []
    else:
        candidates: list[Path] = []
        if res.returncode == 0:
            for p in _matching_pages():
                prev = pre_existing.get(p)
                if prev is not None:
                    st = p.stat()
                    if (st.st_ino, st.st_mtime_ns) == prev:
                        continue
                candidates.append(p)
        pages = sorted(candidates, key=lambda p: int(p.stem[prefix_len:]))
        out_png = pages[0] if pages else out_prefix.parent / f"{out_prefix.name}-1.png"
    png_info = parse_png_info(out_png.read_bytes()) if (res.returncode == 0 and out_png.is_file()) else None
    return {
        "ok": res.returncode == 0 and out_png.is_file(),
        "returncode": res.returncode,
        "out_png": out_png,
        "pages": [str(p) for p in pages],
        "png_info": png_info,
        "stderr": res.stderr,
    }


def run_file_magic(target_path: Path, *, timeout_scale: float = 1.0) -> dict[str, Any]:
    tool = _require_tool("file")
    if tool is None:
        return {"ok": False, "tool_missing": "file", "error": "/usr/bin/file not found"}
    resolved = str(target_path.resolve())
    with tempfile.TemporaryDirectory(prefix="coree_file_") as tmp_dir:
        tmp_cwd = Path(tmp_dir)
        res = run_cmd([str(tool), "-b", "--mime-type", resolved], cwd=tmp_cwd, timeout=10.0, timeout_scale=timeout_scale)
        res_desc = run_cmd([str(tool), "-b", resolved], cwd=tmp_cwd, timeout=10.0, timeout_scale=timeout_scale)
    return {
        "ok": res.returncode == 0 and res_desc.returncode == 0,
        "mime": res.stdout.strip(),
        "description": res_desc.stdout.strip(),
    }


def run_dpkg_deb_inspect(deb_path: Path, *, timeout_scale: float = 1.0) -> dict[str, Any]:
    tool = _require_tool("dpkg-deb")
    if tool is None:
        return {"ok": False, "tool_missing": "dpkg-deb", "error": "dpkg-deb not found"}
    resolved = str(deb_path.resolve())
    with tempfile.TemporaryDirectory(prefix="coree_dpkg_") as tmp_dir:
        tmp_cwd = Path(tmp_dir)
        info = run_cmd([str(tool), "--info", resolved], cwd=tmp_cwd, timeout=20.0, timeout_scale=timeout_scale)
        contents = run_cmd([str(tool), "--contents", resolved], cwd=tmp_cwd, timeout=20.0, timeout_scale=timeout_scale)
    return {
        "ok": info.returncode == 0 and contents.returncode == 0,
        "info": info.stdout,
        "contents": contents.stdout,
        "stderr": info.stderr + contents.stderr,
    }


# ---------------------------------------------------------------------------
# Standalone Clean-Room Binary Dissectors (Never Calling coree_cdr/coree_io)
# ---------------------------------------------------------------------------

def dissect_cdr_riff(data: bytes) -> dict[str, Any]:
    """Independent Python dissector for RIFF/RIFX .cdr/.cdt containers."""
    base = parse_riff_chunks(data)
    loda_list = base["loda_headers"]
    has_20b_loda = bool(loda_list) and all(h["header_kind"] == "20B_authentic" for h in loda_list)
    return {
        **base,
        "has_20b_loda": has_20b_loda,
        "loda_count": len(loda_list),
    }


def dissect_cdr_zcf(data: bytes) -> dict[str, Any]:
    """Independent Python dissector for X4+ / X6+ ZCF ZIP .cdr/.cdt containers."""
    zinfo = parse_zip_entries(data)
    entries = set(zinfo["entries"])
    entries_lower = {name.lower(): name for name in entries}
    xml_checks: dict[str, bool] = {}
    cdss_valid = False
    cdss_key_count = 0
    doc_palette_color_count = 0
    riff_info = zinfo.get("riff_info")
    has_cdro_root = bool(riff_info is not None and riff_info.get("form_type") == "CDRO")
    inline_pointer_count = 0
    external_pointer_count = 0
    invalid_pointer_count = 0
    unindirected_loda_count = 0
    deref_20b_loda_count = 0
    deref_12b_loda_count = 0

    def _xml_has_content(node: ET.Element) -> bool:
        return bool(node.attrib) or bool((node.text or "").strip()) or any(
            _xml_has_content(ch) for ch in node
        )

    if zinfo.get("bad_crc_entry") is None:
        with zipfile.ZipFile(io_bytes(data), "r") as zf:
            info_by_name = _validate_zip_infolist(zf)
            budget = [_ZIP_MAX_TOTAL_BYTES]
            entry_cache: dict[str, bytes] = {}
            for xml_path in (
                "styles/default.xml",
                "color/docPalette.xml",
                "color/documentPalette.xml",
                "metadata/metadata.xml",
                "META-INF/metadata.xml",
                "META-INF/textinfo.xml",
            ):
                matched_xml = entries_lower.get(xml_path.lower())
                if matched_xml is not None:
                    try:
                        raw_xml = _bounded_zip_read_info(
                            zf, info_by_name[matched_xml], remaining_budget=budget, cache=entry_cache
                        )
                        root = ET.fromstring(raw_xml.decode("utf-8-sig"))
                        xml_checks[xml_path] = True
                        if xml_path in ("color/docPalette.xml", "color/documentPalette.xml"):
                            cnt = sum(
                                1
                                for el in root.iter()
                                if el.tag.split("}", 1)[-1] == "color" and _xml_has_content(el)
                            )
                            doc_palette_color_count = max(doc_palette_color_count, cnt)
                    except Exception:
                        xml_checks[xml_path] = False

            cdss_entry = entries_lower.get("styles/document.cdss")
            if cdss_entry is not None:
                try:
                    raw_cdss = _bounded_zip_read_info(
                        zf, info_by_name[cdss_entry], remaining_budget=budget, cache=entry_cache
                    ).decode("utf-8-sig").strip()
                    if raw_cdss.startswith("{") or raw_cdss.startswith("["):
                        parsed = json.loads(raw_cdss)
                        cdss_valid = isinstance(parsed, (dict, list))
                        if isinstance(parsed, dict):
                            style_map = parsed.get("style")
                            if isinstance(style_map, dict):
                                cdss_key_count = sum(1 for v in style_map.values() if bool(v))
                            else:
                                cdss_key_count = sum(
                                    1 for k, v in parsed.items() if k != "version" and bool(v)
                                )
                        elif isinstance(parsed, list):
                            cdss_key_count = sum(1 for v in parsed if bool(v))
                    else:
                        cdss_root = ET.fromstring(raw_cdss)
                        cdss_valid = True
                        cdss_key_count = sum(
                            1
                            for el in cdss_root.iter()
                            if el is not cdss_root
                            and (bool(el.attrib) or bool((el.text or "").strip()))
                        )
                except Exception:
                    cdss_valid = False
                    cdss_key_count = 0
                xml_checks["styles/document.cdss"] = cdss_valid

            data_file_bytes: list[Optional[bytes]] = []
            if "content/datafilelist.dat" in entries_lower:
                list_entry = entries_lower["content/datafilelist.dat"]
                list_text = _bounded_zip_read_info(
                    zf, info_by_name[list_entry], remaining_budget=budget, cache=entry_cache
                ).decode("utf-8-sig", errors="replace")
                for raw_line in list_text.splitlines():
                    trimmed = raw_line.replace("\\", "/").strip()
                    while trimmed.startswith("./"):
                        trimmed = trimmed[2:]
                    if not trimmed:
                        continue
                    if (
                        trimmed == "."
                        or trimmed.startswith("/")
                        or (len(trimmed) >= 2 and trimmed[1] == ":")
                    ):
                        raise ValueError(f"Unsafe dataFileList.dat entry path: {raw_line!r}")
                    if ".." in trimmed.split("/"):
                        raise ValueError(f"Unsafe dataFileList.dat traversal path: {raw_line!r}")
                    low = trimmed.lower()
                    if low in ("content/root.dat", "content/riffdata.cdr", "root.dat"):
                        data_file_bytes.append(None)
                        continue
                    if low.startswith("content/"):
                        matched = entries_lower.get(low)
                    else:
                        matched = (
                            entries_lower.get(f"content/data/{low}")
                            or entries_lower.get(f"content/{low}")
                            or entries_lower.get(low)
                        )
                    if matched is not None:
                        data_file_bytes.append(
                            _bounded_zip_read_info(
                                zf, info_by_name[matched], remaining_budget=budget, cache=entry_cache
                            )
                        )
                    else:
                        data_file_bytes.append(None)

            if riff_info is not None:
                is_zcf_indirect_tree = riff_info.get("form_type") in ("CDRO", "CDTO")
                for fourcc, payload in riff_info.get("chunk_payloads", []):
                    if is_zcf_indirect_tree and fourcc == "loda" and len(payload) != 16:
                        unindirected_loda_count += 1
                    if len(payload) == 16:
                        file_index, data_len, data_offset, _reserved = struct.unpack("<IIII", payload)
                        if file_index == 0xFFFFFFFF:
                            if is_zcf_indirect_tree and fourcc == "loda":
                                invalid_pointer_count += 1
                            elif data_len <= 8:
                                inline_pointer_count += 1
                            elif is_zcf_indirect_tree:
                                invalid_pointer_count += 1
                        elif is_zcf_indirect_tree:
                            if (
                                file_index < len(data_file_bytes)
                                and data_file_bytes[file_index] is not None
                                and data_len > 0
                                and data_offset + data_len <= len(data_file_bytes[file_index])
                            ):
                                if fourcc == "loda":
                                    deref = data_file_bytes[file_index][data_offset:data_offset + data_len]
                                    try:
                                        loda_kind = _classify_loda_payload(deref, "<")
                                    except ValueError:
                                        invalid_pointer_count += 1
                                        continue
                                    if loda_kind == "20B_authentic":
                                        deref_20b_loda_count += 1
                                    else:
                                        deref_12b_loda_count += 1
                                external_pointer_count += 1
                            else:
                                invalid_pointer_count += 1

    indirect_pointer_count = inline_pointer_count + external_pointer_count
    has_valid_indirect_pointers = (indirect_pointer_count >= 1 and invalid_pointer_count == 0)
    has_20b_deref_loda = (deref_20b_loda_count >= 1 and deref_12b_loda_count == 0)
    has_doc_palette = bool(
        xml_checks.get("color/docPalette.xml") or xml_checks.get("color/documentPalette.xml")
    )
    return {
        **zinfo,
        "xml_checks": xml_checks,
        "has_cdro_root": has_cdro_root,
        "inline_pointer_count": inline_pointer_count,
        "external_pointer_count": external_pointer_count,
        "invalid_pointer_count": invalid_pointer_count,
        "indirect_pointer_count": indirect_pointer_count,
        "unindirected_loda_count": unindirected_loda_count,
        "deref_20b_loda_count": deref_20b_loda_count,
        "deref_12b_loda_count": deref_12b_loda_count,
        "has_20b_deref_loda": has_20b_deref_loda,
        "has_valid_indirect_pointers": has_valid_indirect_pointers,
        "has_cdss": cdss_valid,
        "cdss_key_count": cdss_key_count,
        "has_doc_palette_xml": has_doc_palette,
        "doc_palette_color_count": doc_palette_color_count,
        "has_metadata_xml": bool(
            xml_checks.get("META-INF/metadata.xml") or xml_checks.get("metadata/metadata.xml")
        ),
        "has_textinfo_xml": bool(xml_checks.get("META-INF/textinfo.xml")),
    }


def dissect_psd(data: bytes) -> dict[str, Any]:
    if len(data) < 26 or data[:4] != b"8BPS":
        raise ValueError("Invalid PSD magic (expected 8BPS)")
    version, _reserved, channels, height, width, depth, color_mode = struct.unpack(">H6sHIIHH", data[4:26])
    if version not in (1, 2):
        raise ValueError(f"Invalid PSD version {version} (expected 1 or 2)")
    if channels == 0 or width == 0 or height == 0:
        raise ValueError(f"Invalid PSD dimensions/channels: {width}x{height} ch={channels}")
    if depth not in (1, 8, 16, 32):
        raise ValueError(f"Invalid PSD bit depth {depth}")
    min_channels_by_mode = {0: 1, 1: 1, 2: 1, 3: 3, 4: 4, 7: 1, 8: 1, 9: 3}
    if color_mode not in min_channels_by_mode:
        raise ValueError(f"Invalid PSD color_mode={color_mode}")
    valid_depths_by_mode = {
        0: (1,),
        1: (8, 16, 32),
        2: (8,),
        3: (8, 16, 32),
        4: (8, 16, 32),
        7: (8, 16, 32),
        8: (8,),
        9: (8, 16, 32),
    }
    if depth not in valid_depths_by_mode[color_mode]:
        raise ValueError(
            f"Invalid PSD depth={depth} for color_mode={color_mode} "
            f"(expected {valid_depths_by_mode[color_mode]})"
        )
    min_channels = min_channels_by_mode[color_mode]
    if channels < min_channels:
        raise ValueError(
            f"Invalid PSD channel count {channels} for color_mode={color_mode} (expected >= {min_channels})"
        )

    pos = 26
    if pos + 4 > len(data):
        raise ValueError("Truncated PSD color mode data length")
    cmd_len = struct.unpack(">I", data[pos:pos + 4])[0]
    pos += 4
    if pos + cmd_len > len(data):
        raise ValueError(f"Truncated PSD color mode data: {pos + cmd_len} > {len(data)}")
    if color_mode == 2 and cmd_len != 768:
        raise ValueError(f"Invalid PSD Indexed color_mode_data_len={cmd_len} (expected 768)")
    pos += cmd_len

    if pos + 4 > len(data):
        raise ValueError("Truncated PSD image resources length")
    res_len = struct.unpack(">I", data[pos:pos + 4])[0]
    pos += 4
    if pos + res_len > len(data):
        raise ValueError(f"Truncated PSD image resources: {pos + res_len} > {len(data)}")
    pos += res_len

    len_bytes = 8 if version == 2 else 4
    len_fmt = ">Q" if version == 2 else ">I"
    if pos + len_bytes > len(data):
        raise ValueError("Truncated PSD layer and mask length")
    layer_mask_len = struct.unpack(len_fmt, data[pos:pos + len_bytes])[0]
    img_data_off = pos + len_bytes + layer_mask_len
    if img_data_off + 2 > len(data):
        raise ValueError(
            f"Truncated PSD layer/image data: {img_data_off + 2} > {len(data)}"
        )
    if layer_mask_len > 0:
        if layer_mask_len < len_bytes:
            raise ValueError(
                f"Invalid PSD layer_mask_len={layer_mask_len} < {len_bytes}"
            )
        sec_start = pos + len_bytes
        if not any(data[sec_start:img_data_off]):
            raise ValueError("PSD layer and mask section is all zeros (stub)")
        layer_info_len = struct.unpack(len_fmt, data[sec_start:sec_start + len_bytes])[0]
        if layer_info_len > layer_mask_len - len_bytes:
            raise ValueError(
                f"Invalid PSD layer_info_len={layer_info_len} > available {layer_mask_len - len_bytes}"
            )
        if layer_info_len > 0:
            if layer_info_len < 2:
                raise ValueError(
                    f"Invalid PSD layer_info_len={layer_info_len} < 2 (cannot hold layer_count)"
                )
            li_start = sec_start + len_bytes
            li_end = li_start + layer_info_len
            layer_count = struct.unpack(">h", data[li_start:li_start + 2])[0]
            abs_layers = abs(layer_count)
            if abs_layers == 0:
                raise ValueError("Invalid PSD layer_count=0 inside non-empty Layer Info section")
            ch_ref_size = 10 if version == 2 else 6
            lpos = li_start + 2
            for lidx in range(abs_layers):
                if lpos + 18 > li_end:
                    raise ValueError(f"Truncated PSD layer[{lidx}] header at offset {lpos}")
                num_ch = struct.unpack(">H", data[lpos + 16:lpos + 18])[0]
                lpos += 18 + num_ch * ch_ref_size
                if lpos + 16 > li_end:
                    raise ValueError(f"Truncated PSD layer[{lidx}] blend/extra header at offset {lpos}")
                if data[lpos:lpos + 4] != b"8BIM":
                    raise ValueError(
                        f"Invalid PSD layer[{lidx}] blend signature {data[lpos:lpos + 4]!r} (expected b'8BIM')"
                    )
                extra_len = struct.unpack(">I", data[lpos + 12:lpos + 16])[0]
                lpos += 16 + extra_len
                if lpos > li_end:
                    raise ValueError(f"Truncated PSD layer[{lidx}] extra data past {li_end}")
    compression = struct.unpack(">H", data[img_data_off:img_data_off + 2])[0]
    if compression not in (0, 1, 2, 3):
        raise ValueError(f"Invalid PSD image compression {compression}")
    row_bytes = (width * depth + 7) // 8 if depth == 1 else width * max(1, depth // 8)
    expected_raw_bytes = row_bytes * channels * height
    if compression == 0:
        raw_end = img_data_off + 2 + expected_raw_bytes
        if raw_end > len(data):
            raise ValueError(
                f"Truncated PSD raw image data: {raw_end} > {len(data)}"
            )
        if raw_end < len(data):
            raise ValueError(
                f"Trailing unconsumed bytes after PSD raw image data: {len(data) - raw_end}"
            )
    elif compression == 1:
        n_scanlines = height * channels
        count_bytes = n_scanlines * (4 if version == 2 else 2)
        count_off = img_data_off + 2
        if count_off + count_bytes > len(data):
            raise ValueError(
                f"Truncated PSD RLE scanline byte counts: {count_off + count_bytes} > {len(data)}"
            )
        count_fmt = f">{n_scanlines}I" if version == 2 else f">{n_scanlines}H"
        counts = struct.unpack(count_fmt, data[count_off:count_off + count_bytes])
        rle_payload_bytes = sum(counts)
        if width > 0 and height > 0 and rle_payload_bytes == 0:
            raise ValueError("Invalid empty PSD RLE payload (sum of scanline byte counts == 0)")
        scan_pos = count_off + count_bytes
        if scan_pos + rle_payload_bytes > len(data):
            raise ValueError(
                f"Truncated PSD RLE image data: {scan_pos + rle_payload_bytes} > {len(data)}"
            )
        for idx, scan_len in enumerate(counts):
            scan_end = scan_pos + scan_len
            decoded = 0
            p = scan_pos
            while p < scan_end:
                b = data[p]
                p += 1
                if b == 128:
                    continue
                if b < 128:
                    lit_count = b + 1
                    if p + lit_count > scan_end:
                        raise ValueError(
                            f"PSD PackBits literal overrun in scanline {idx}: "
                            f"need {lit_count}, remaining {scan_end - p}"
                        )
                    p += lit_count
                    decoded += lit_count
                else:
                    if p >= scan_end:
                        raise ValueError(
                            f"PSD PackBits repeat byte missing in scanline {idx}"
                        )
                    p += 1
                    decoded += 257 - b
            if decoded != row_bytes:
                raise ValueError(
                    f"PSD PackBits scanline {idx} decoded {decoded} bytes != expected row_bytes={row_bytes}"
                )
            scan_pos = scan_end
        if scan_pos < len(data):
            raise ValueError(
                f"Trailing unconsumed bytes after PSD RLE image data: {len(data) - scan_pos}"
            )
    elif compression in (2, 3):
        if img_data_off + 4 > len(data):
            raise ValueError(
                f"Truncated PSD ZIP image data: {img_data_off + 4} > {len(data)}"
            )
        max_psd_raw_bytes = 256 * 1024 * 1024
        if expected_raw_bytes > max_psd_raw_bytes:
            raise ValueError(
                f"PSD ZIP expected raw size {expected_raw_bytes} exceeds limit {max_psd_raw_bytes}"
            )
        try:
            dec = zlib.decompressobj()
            decompressed = dec.decompress(
                data[img_data_off + 2:], max_length=expected_raw_bytes + 1
            )
            if not dec.eof and not dec.unconsumed_tail and len(decompressed) <= expected_raw_bytes:
                decompressed += dec.flush()
        except zlib.error as exc:
            raise ValueError(f"Invalid PSD ZIP image data: {exc}") from exc
        if dec.unconsumed_tail or len(decompressed) > expected_raw_bytes:
            raise ValueError(
                f"PSD ZIP image data exceeds expected {expected_raw_bytes} bytes"
            )
        if not dec.eof:
            raise ValueError("Truncated PSD ZIP zlib stream (missing EOF)")
        if dec.unused_data:
            raise ValueError(
                f"Trailing unconsumed bytes after PSD ZIP zlib stream: {len(dec.unused_data)}"
            )
        if len(decompressed) != expected_raw_bytes:
            raise ValueError(
                f"PSD ZIP image data length mismatch: got {len(decompressed)} != expected {expected_raw_bytes}"
            )

    return {
        "version": version,
        "channels": channels,
        "width": width,
        "height": height,
        "depth": depth,
        "color_mode": color_mode,
        "color_mode_data_len": cmd_len,
        "image_resources_len": res_len,
        "layer_mask_len": layer_mask_len,
    }


def dissect_emf(data: bytes) -> dict[str, Any]:
    if len(data) < 88:
        raise ValueError("Truncated EMF header (< 88 bytes)")
    rec_type, rec_size = struct.unpack("<II", data[:8])
    sig = data[40:44]
    if rec_type != 1 or rec_size < 88 or sig != b" EMF":
        raise ValueError(f"Invalid EMF EMR_HEADER: type={rec_type}, size={rec_size}, sig={sig!r}")
    n_bytes, num_records = struct.unpack("<II", data[48:56])
    if n_bytes < rec_size or n_bytes > len(data):
        raise ValueError(f"Invalid EMF nBytes={n_bytes} (rec_size={rec_size}, len={len(data)})")
    if n_bytes != len(data):
        raise ValueError(f"EMF trailing bytes after nBytes={n_bytes}: len={len(data)}")
    if num_records < 2:
        raise ValueError(f"Invalid EMF nRecords={num_records} (expected >= 2)")

    pos = 0
    walked = 0
    saw_eof = False
    while pos < n_bytes:
        if pos + 8 > n_bytes:
            raise ValueError(f"Truncated EMF record header at offset {pos}")
        rtype, rsize = struct.unpack("<II", data[pos:pos + 8])
        if rsize < 8 or (rsize % 4) != 0 or pos + rsize > n_bytes:
            raise ValueError(f"Invalid EMF record (type={rtype}, size={rsize}) at offset {pos}")
        walked += 1
        pos += rsize
        if rtype == 14:  # EMR_EOF
            if rsize < 20:
                raise ValueError(f"Invalid EMF EMR_EOF size {rsize} < 20")
            saw_eof = True
            break
    if not saw_eof:
        raise ValueError("EMF missing EMR_EOF (type 14) record")
    if pos != n_bytes:
        raise ValueError(f"EMF trailing unwalked bytes after EMR_EOF: pos={pos} != nBytes={n_bytes}")
    if walked != num_records:
        raise ValueError(f"EMF record count mismatch: walked={walked} != header nRecords={num_records}")

    return {
        "record_type": rec_type,
        "header_size": rec_size,
        "signature": sig.decode("ascii"),
        "records": num_records,
        "n_bytes": n_bytes,
    }


def dissect_wmf(data: bytes) -> dict[str, Any]:
    if len(data) < 18:
        raise ValueError("Truncated WMF (< 18 bytes)")
    key = struct.unpack("<I", data[:4])[0]
    is_placeable = (key == 0x9AC6CDD7)
    off = 22 if is_placeable else 0
    if len(data) < off + 18:
        raise ValueError("Truncated WMF standard header")
    if is_placeable:
        words = struct.unpack("<10H", data[0:20])
        xor_sum = 0
        for w in words:
            xor_sum ^= w
        expected_sum = struct.unpack("<H", data[20:22])[0]
        if xor_sum != expected_sum:
            raise ValueError(
                f"Invalid WMF placeable checksum: computed={xor_sum:#06x} != stored={expected_sum:#06x}"
            )
    ftype, hdr_size, version, file_size_words, _max_obj, _max_rec, _num_params = struct.unpack(
        "<HHHIHIH", data[off:off + 18]
    )
    if ftype not in (1, 2):
        raise ValueError(f"Invalid WMF file_type {ftype} (expected 1 or 2)")
    if hdr_size != 9:
        raise ValueError(f"Invalid WMF header_words {hdr_size} != 9")
    end_bytes = off + file_size_words * 2
    if file_size_words < 9 or end_bytes > len(data):
        raise ValueError(
            f"Truncated WMF file_size_words={file_size_words} ({end_bytes} > {len(data)})"
        )
    if end_bytes != len(data):
        raise ValueError(
            f"WMF trailing bytes after declared end={end_bytes}: len={len(data)}"
        )

    pos = off + 18
    records = 0
    has_eof = False
    while pos < end_bytes:
        if pos + 6 > end_bytes:
            raise ValueError(f"Truncated WMF record header at offset {pos}")
        rd_size_words, rd_func = struct.unpack("<IH", data[pos:pos + 6])
        if rd_size_words < 3:
            raise ValueError(f"Invalid WMF record rd_size_words={rd_size_words} (< 3) at offset {pos}")
        rec_bytes = rd_size_words * 2
        if pos + rec_bytes > end_bytes:
            raise ValueError(
                f"Truncated WMF record (func={rd_func:#06x}, words={rd_size_words}) at offset {pos}: "
                f"{pos + rec_bytes} > {end_bytes}"
            )
        records += 1
        pos += rec_bytes
        if rd_func == 0x0000:  # META_EOF
            if rd_size_words != 3:
                raise ValueError(f"Invalid WMF META_EOF size words {rd_size_words} != 3")
            has_eof = True
            break

    if not has_eof:
        raise ValueError("WMF missing META_EOF (0x0000) record")
    if pos != end_bytes:
        raise ValueError(
            f"WMF trailing unwalked bytes before declared end: pos={pos} != end={end_bytes}"
        )

    return {
        "placeable": is_placeable,
        "file_type": ftype,
        "header_words": hdr_size,
        "version": hex(version),
        "file_size_words": file_size_words,
        "records": records,
        "has_eof": has_eof,
    }


def dissect_acb(data: bytes) -> dict[str, Any]:
    if len(data) < 12 or data[:4] != b"8BCB":
        raise ValueError("Invalid ACB magic (expected 8BCB)")
    version, book_id = struct.unpack(">HH", data[4:8])
    if version != 1:
        raise ValueError(f"Unsupported ACB version {version} (expected 1)")

    pos = 8

    def read_u16_str() -> str:
        nonlocal pos
        if pos + 4 > len(data):
            raise ValueError(f"Truncated ACB UTF-16 string length at offset {pos}")
        char_count = struct.unpack(">I", data[pos:pos + 4])[0]
        pos += 4
        byte_count = char_count * 2
        if pos + byte_count > len(data):
            raise ValueError(f"Truncated ACB UTF-16 string payload at offset {pos}")
        raw_s = data[pos:pos + byte_count]
        pos += byte_count
        try:
            return raw_s.decode("utf-16-be")
        except UnicodeDecodeError as exc:
            raise ValueError(f"Invalid ACB UTF-16BE string: {exc}") from exc

    title = read_u16_str()
    _prefix = read_u16_str()
    _suffix = read_u16_str()
    _description = read_u16_str()

    if pos + 8 > len(data):
        raise ValueError(f"Truncated ACB color book metadata at offset {pos}")
    color_count, page_size, page_offset, color_space = struct.unpack(">HHHH", data[pos:pos + 8])
    pos += 8
    if color_count == 0:
        raise ValueError("Invalid ACB color_count=0 (must be > 0)")
    channels_by_space = {0: 3, 2: 4, 7: 3, 8: 1}
    if color_space not in channels_by_space:
        raise ValueError(f"Unsupported ACB color_space {color_space}")
    n_channels = channels_by_space[color_space]

    for idx in range(color_count):
        _swatch = read_u16_str()
        if pos + 6 + n_channels > len(data):
            raise ValueError(f"Truncated ACB color record {idx} at offset {pos}")
        pos += 6 + n_channels
    if pos != len(data):
        raise ValueError(
            f"Trailing unconsumed bytes after ACB color records: pos={pos} != len={len(data)}"
        )

    return {
        "magic": "8BCB",
        "version": version,
        "book_id": book_id,
        "title": title,
        "color_count": color_count,
        "page_size": page_size,
        "page_offset": page_offset,
        "color_space": color_space,
        "byte_len": len(data),
    }


_VALID_DWG_MAGICS = (
    b"AC1012",
    b"AC1013",
    b"AC1014",
    b"AC1015",
    b"AC1018",
    b"AC1021",
    b"AC1024",
    b"AC1027",
    b"AC1032",
)
_R2000_DWG_SENTINEL = bytes.fromhex("95A04E2899821AE55E41E05F9D3A4D00")
_DWG_SEC0_START_SENTINEL = bytes.fromhex("CF7B1F23FDDE38A95F7C68B84E6D335F")
_DWG_SEC0_END_SENTINEL = bytes.fromhex("3084E0DC0221C756A0839747B192CCA0")
_DWG_SEC1_START_SENTINEL = bytes.fromhex("8DA1C4B8C4A9F8C5C0DCF45FE7CFB68A")
_DWG_SEC1_END_SENTINEL = bytes.fromhex("725E3B473B56073A3F230BA018304975")


def dissect_dwg(data: bytes) -> dict[str, Any]:
    """Independent Python binary dissector for AutoCAD DWG (R13/R14/R2000+ AC1012..AC1032)."""
    if len(data) < 128:
        raise ValueError(f"Truncated DWG ({len(data)} < 128 bytes)")
    magic = data[:6]
    if magic not in _VALID_DWG_MAGICS:
        raise ValueError(f"Invalid DWG magic {magic!r}")
    if not any(data[6:]):
        raise ValueError("DWG body is all zeros (stub)")

    magic_str = magic.decode("ascii")
    if magic in (b"AC1012", b"AC1013", b"AC1014", b"AC1015"):
        num_records = struct.unpack("<I", data[21:25])[0]
        if num_records < 3 or num_records > 256:
            raise ValueError(f"Invalid DWG num_records={num_records} (expected >= 3)")
        crc_off = 25 + num_records * 9
        if crc_off + 18 > len(data):
            raise ValueError(
                f"Truncated DWG header locators/sentinel: need {crc_off + 18}, got {len(data)}"
            )
        hdr_end = crc_off + 18
        locators: list[tuple[int, int, int]] = []
        for idx in range(num_records):
            off = 25 + idx * 9
            rec_id, seeker, size = struct.unpack("<BII", data[off:off + 9])
            if size > 0 and (seeker < hdr_end or seeker + size > len(data)):
                raise ValueError(
                    f"DWG section locator[{idx}] id={rec_id} out of bounds: "
                    f"seeker={seeker}, size={size}, len={len(data)}"
                )
            locators.append((rec_id, seeker, size))
        non_empty = [
            (seeker, seeker + size, idx, rec_id)
            for idx, (rec_id, seeker, size) in enumerate(locators)
            if size > 0
        ]
        if not non_empty:
            raise ValueError("DWG has no non-empty section locators (all size == 0)")
        non_empty_ids = {rec_id for _, _, _, rec_id in non_empty}
        if 0 not in non_empty_ids or 1 not in non_empty_ids:
            raise ValueError("DWG missing non-empty Header Variables (id=0) or Classes (id=1) section")
        non_empty_sorted = sorted(non_empty)
        for i in range(len(non_empty_sorted) - 1):
            s_a, e_a, idx_a, id_a = non_empty_sorted[i]
            s_b, e_b, idx_b, id_b = non_empty_sorted[i + 1]
            if e_a > s_b:
                raise ValueError(
                    f"DWG overlapping section locators: [{idx_a}] id={id_a} ({s_a}..{e_a}) "
                    f"overlaps [{idx_b}] id={id_b} ({s_b}..{e_b})"
                )
        stored_crc = struct.unpack("<H", data[crc_off:crc_off + 2])[0]
        calc_crc = _dwg_crc16(data[:crc_off], seed=0xC0C1)
        if stored_crc != calc_crc:
            raise ValueError(
                f"DWG header CRC16 mismatch: computed={calc_crc:#06x} != stored={stored_crc:#06x}"
            )
        sentinel = data[crc_off + 2:crc_off + 18]
        if sentinel != _R2000_DWG_SENTINEL:
            raise ValueError(f"DWG R2000 header sentinel mismatch: {sentinel.hex()}")
        for idx, (rec_id, seeker, size) in enumerate(locators):
            if size <= 0:
                continue
            sec = data[seeker:seeker + size]
            if not any(sec):
                raise ValueError(
                    f"DWG section locator[{idx}] id={rec_id} payload is all zeros (stub)"
                )
            if rec_id in (0, 1):
                if size < 38:
                    raise ValueError(
                        f"DWG section id={rec_id} too short ({size} < 38 bytes)"
                    )
                exp_start, exp_end = (
                    (_DWG_SEC0_START_SENTINEL, _DWG_SEC0_END_SENTINEL)
                    if rec_id == 0
                    else (_DWG_SEC1_START_SENTINEL, _DWG_SEC1_END_SENTINEL)
                )
                if sec[:16] != exp_start:
                    raise ValueError(
                        f"DWG section id={rec_id} start sentinel mismatch: {sec[:16].hex()}"
                    )
                if sec[-16:] != exp_end:
                    raise ValueError(
                        f"DWG section id={rec_id} end sentinel mismatch: {sec[-16:].hex()}"
                    )
                stored_sec_crc = struct.unpack("<H", sec[-18:-16])[0]
                calc_sec_crc = _dwg_crc16(sec[16:-18], seed=0xC0C1)
                if stored_sec_crc != calc_sec_crc:
                    raise ValueError(
                        f"DWG section id={rec_id} CRC16 mismatch: "
                        f"computed={calc_sec_crc:#06x} != stored={stored_sec_crc:#06x}"
                    )
                declared_sec_len = struct.unpack("<I", sec[16:20])[0]
                if declared_sec_len != size - 38:
                    raise ValueError(
                        f"DWG section id={rec_id} inner size mismatch: {declared_sec_len} != {size - 38}"
                    )
            elif rec_id == 2:
                if size < 4:
                    raise ValueError(
                        f"DWG section id=2 (Object Map) too short ({size} < 4 bytes)"
                    )
        return {
            "magic": magic_str,
            "byte_len": len(data),
            "num_records": num_records,
            "locators": locators,
            "header_crc": hex(stored_crc),
            "sentinel_ok": True,
        }

    if magic in (b"AC1018", b"AC1024", b"AC1027", b"AC1032"):
        if len(data) <= 0x100:
            raise ValueError(
                f"Truncated DWG R2004+ file ({len(data)} <= 256 bytes; missing section/page map data past header)"
            )
        if data[6:11] != b"\x00" * 5:
            raise ValueError(f"Invalid DWG R2004+ reserved header bytes [6:11]: {data[6:11].hex()}")
        seed = 1
        decrypted = bytearray(0x6C)
        for i in range(0x6C):
            seed = (seed * 0x343FD + 0x269EC3) & 0xFFFFFFFF
            decrypted[i] = data[0x80 + i] ^ ((seed >> 16) & 0xFF)
        if not decrypted.startswith(b"AcFssFcAJ2K\x00"):
            raise ValueError(
                f"Invalid DWG R2004+ decrypted header ID: {bytes(decrypted[:12])!r}"
            )
        if not any(decrypted[12:0x68]):
            raise ValueError("DWG R2004+ decrypted header metadata [0x0C:0x68] is all zeros (stub)")
        sect_page_map_id = struct.unpack("<I", decrypted[0x20:0x24])[0]
        sect_page_map_addr = struct.unpack("<Q", decrypted[0x2C:0x34])[0]
        sect_map_id = struct.unpack("<I", decrypted[0x34:0x38])[0]
        stored_crc32 = struct.unpack("<I", decrypted[0x68:0x6C])[0]
        if stored_crc32 == 0:
            raise ValueError("Invalid DWG R2004+ decrypted header CRC32 == 0")
        if sect_page_map_id == 0:
            raise ValueError("Invalid DWG R2004+ sect_page_map_id == 0")
        if sect_map_id == 0:
            raise ValueError("Invalid DWG R2004+ sect_map_id == 0")
        abs_page_map_off = 0x100 + sect_page_map_addr
        if abs_page_map_off < 0x100 or abs_page_map_off > len(data) - 20:
            raise ValueError(
                f"DWG R2004+ sect_page_map_addr out of bounds: 0x100+{sect_page_map_addr}={abs_page_map_off} "
                f"not in [256, {len(data) - 20}]"
            )
        if not any(data[0x100:]):
            raise ValueError("DWG R2004+ body past 0x100 is all zeros (stub)")
        if not any(data[abs_page_map_off:abs_page_map_off + 20]):
            raise ValueError("DWG R2004+ section page map header is all zeros (stub)")
        pm_type, pm_decomp, pm_comp, pm_comp_type, _pm_csum = struct.unpack(
            "<IIIII", data[abs_page_map_off:abs_page_map_off + 20]
        )
        if pm_type != 0x41630E3B:
            raise ValueError(
                f"DWG R2004+ invalid System Section Page type at {abs_page_map_off}: "
                f"{pm_type:#010x} != 0x41630E3B"
            )
        if pm_comp_type not in (0, 1, 2):
            raise ValueError(
                f"DWG R2004+ invalid section page map comp_type={pm_comp_type} (expected 0, 1, or 2)"
            )
        if pm_comp == 0 or abs_page_map_off + 20 + pm_comp > len(data):
            raise ValueError(
                f"DWG R2004+ section page map compressed size out of bounds: "
                f"pm_comp={pm_comp} at {abs_page_map_off + 20} > {len(data)}"
            )
        if pm_decomp == 0 or pm_decomp > 64 * 1024 * 1024:
            raise ValueError(
                f"DWG R2004+ invalid section page map decompressed size: pm_decomp={pm_decomp}"
            )
        if pm_comp_type == 0 and pm_comp != pm_decomp:
            raise ValueError(
                f"DWG R2004+ uncompressed section page map size mismatch: "
                f"pm_comp={pm_comp} != pm_decomp={pm_decomp}"
            )
        crc_input = bytearray(decrypted)
        crc_input[0x68:0x6C] = b"\x00\x00\x00\x00"
        calc_crc32 = zlib.crc32(bytes(crc_input)) & 0xFFFFFFFF
        if stored_crc32 != calc_crc32:
            raise ValueError(
                f"DWG R2004+ decrypted header CRC32 mismatch: computed={calc_crc32:#010x} != stored={stored_crc32:#010x}"
            )
        return {
            "magic": magic_str,
            "byte_len": len(data),
            "encrypted_header_ok": True,
            "sect_page_map_id": sect_page_map_id,
            "sect_page_map_addr": sect_page_map_addr,
            "sect_map_id": sect_map_id,
            "header_crc32": hex(stored_crc32),
        }

    if magic == b"AC1021":
        if len(data) <= 0x400:
            raise ValueError(
                f"Truncated DWG R2007 file ({len(data)} <= 1024 bytes; missing payload past header)"
            )
        if data[6:11] != b"\x00" * 5:
            raise ValueError(f"Invalid DWG R2007 reserved header bytes [6:11]: {data[6:11].hex()}")
        rs_region = data[0x80:0x380]
        if not any(rs_region) or len(set(rs_region)) == 1:
            raise ValueError("DWG R2007 Reed-Solomon header region [0x80:0x380] is zero/constant (stub)")
        if not all(any(rs_region[off:off + 256]) for off in (0, 256, 512)):
            raise ValueError(
                "DWG R2007 Reed-Solomon header region [0x80:0x380] has an all-zero 256-byte slice (stub)"
            )
        nonzero_rs = sum(1 for b in rs_region if b != 0)
        if nonzero_rs < 128:
            raise ValueError(
                f"DWG R2007 Reed-Solomon header region [0x80:0x380] too sparse ({nonzero_rs} non-zero bytes < 128)"
            )
        distinct_nonzero = {b for b in rs_region if b != 0}
        if len(distinct_nonzero) < 32:
            raise ValueError(
                f"DWG R2007 Reed-Solomon header region [0x80:0x380] lacks entropy "
                f"({len(distinct_nonzero)} distinct non-zero bytes < 32)"
            )
        nonzero_payload = sum(1 for b in data[0x400:] if b != 0)
        if nonzero_payload < 16:
            raise ValueError(
                f"DWG R2007 payload past 0x400 is too sparse/stub ({nonzero_payload} non-zero bytes < 16)"
            )
        return {
            "magic": magic_str,
            "byte_len": len(data),
            "rs_distinct_nonzero": len(distinct_nonzero),
        }

    return {
        "magic": magic_str,
        "byte_len": len(data),
    }


def _decode_png_rgb_bytes_checked(
    path: Path, *, max_pixels: Optional[int] = None
) -> tuple[int, int, bytes]:
    raw_bytes = path.read_bytes()
    ihdr = _validate_png_bytes_full(raw_bytes)
    if max_pixels is not None and ihdr["width"] * ihdr["height"] > max_pixels:
        raise ValueError(
            f"PNG pixel count {ihdr['width'] * ihdr['height']} exceeds limit {max_pixels}"
        )
    if ihdr["height"] <= _PNG_TALL_HEIGHT_PIL_BYPASS:
        try:
            from PIL import Image  # type: ignore
            with Image.open(io_bytes(raw_bytes)) as im:
                w, h = im.size
                rgb_bytes = _pil_image_to_rgb_bytes(im, raw_png_bytes=raw_bytes)
            del raw_bytes
            return w, h, rgb_bytes
        except ImportError:
            pass
    if ihdr["interlace"] == 0:
        ihdr, plte, trns, _phys, raw = _validate_png_chunks(raw_bytes, retain_raw=True)
        del raw_bytes
        w, h = ihdr["width"], ihdr["height"]
        ctype_id = ihdr["color_type"]
        depth = ihdr["bit_depth"]
        channels_count = {0: 1, 2: 3, 3: 1, 4: 2, 6: 4}[ctype_id]
        bits_per_pixel = depth * channels_count
        rgb_out = bytearray(w * h * 3)
        end_off: list[int] = []
        row_rgb_len = w * 3
        for y, scan in enumerate(_iter_unfiltered_scanlines(raw, 0, w, h, bits_per_pixel, end_off)):
            row_rgba = _png_scan_row_to_rgba(scan, w, ctype_id, depth, plte, trns)
            dst_base = y * row_rgb_len
            for x in range(w):
                si = x * 4
                di = dst_base + x * 3
                a = row_rgba[si + 3]
                if a == 255:
                    rgb_out[di] = row_rgba[si]
                    rgb_out[di + 1] = row_rgba[si + 1]
                    rgb_out[di + 2] = row_rgba[si + 2]
                elif a == 0:
                    rgb_out[di] = 255
                    rgb_out[di + 1] = 255
                    rgb_out[di + 2] = 255
                else:
                    inv_a = 255 - a
                    rgb_out[di] = (row_rgba[si] * a + 255 * inv_a + 127) // 255
                    rgb_out[di + 1] = (row_rgba[si + 1] * a + 255 * inv_a + 127) // 255
                    rgb_out[di + 2] = (row_rgba[si + 2] * a + 255 * inv_a + 127) // 255
        raw_off = end_off[0] if end_off else 0
        if raw_off != len(raw):
            raise ValueError(f"PNG IDAT trailing bytes: {len(raw) - raw_off}")
        del raw
        return w, h, bytes(rgb_out)

    w, h, rgba = _decode_png_rgba_bytes_stdlib(raw_bytes)
    del raw_bytes
    rgb_out = bytearray(w * h * 3)
    for i in range(w * h):
        si = i * 4
        di = i * 3
        a = rgba[si + 3]
        if a == 255:
            rgb_out[di] = rgba[si]
            rgb_out[di + 1] = rgba[si + 1]
            rgb_out[di + 2] = rgba[si + 2]
        elif a == 0:
            rgb_out[di] = 255
            rgb_out[di + 1] = 255
            rgb_out[di + 2] = 255
        else:
            inv_a = 255 - a
            rgb_out[di] = (rgba[si] * a + 255 * inv_a + 127) // 255
            rgb_out[di + 1] = (rgba[si + 1] * a + 255 * inv_a + 127) // 255
            rgb_out[di + 2] = (rgba[si + 2] * a + 255 * inv_a + 127) // 255
    del rgba
    return w, h, bytes(rgb_out)


def _decode_png_rgb_bytes(path: Path) -> tuple[int, int, bytes]:
    return _decode_png_rgb_bytes_checked(path)


def _resample_rgb_nearest(buf: bytes, src_w: int, src_h: int, dst_w: int, dst_h: int) -> bytes:
    if (src_w, src_h) == (dst_w, dst_h):
        return buf
    out = bytearray(dst_w * dst_h * 3)
    for y in range(dst_h):
        sy = min(src_h - 1, (y * src_h) // dst_h)
        srow = sy * src_w * 3
        drow = y * dst_w * 3
        for x in range(dst_w):
            sx = min(src_w - 1, (x * src_w) // dst_w)
            so = srow + sx * 3
            do = drow + x * 3
            out[do:do + 3] = buf[so:so + 3]
    return bytes(out)


def _non_white_iou_rgb(raw_a: bytes, raw_b: bytes) -> float:
    inter = 0
    union = 0
    for off in range(0, min(len(raw_a), len(raw_b)), 3):
        a_nw = raw_a[off] < 250 or raw_a[off + 1] < 250 or raw_a[off + 2] < 250
        b_nw = raw_b[off] < 250 or raw_b[off + 1] < 250 or raw_b[off + 2] < 250
        if a_nw and b_nw:
            inter += 1
            union += 1
        elif a_nw or b_nw:
            union += 1
    return (inter / union) if union > 0 else 1.0


def crop_png_center(src_png: Path, target_w: int, target_h: int, dst_png: Path) -> None:
    """Center-crop a PNG to (target_w, target_h) using PIL or pure-stdlib PNG."""
    raw_bytes = src_png.read_bytes()
    ihdr = _validate_png_bytes_full(raw_bytes)
    if ihdr["height"] <= _PNG_TALL_HEIGHT_PIL_BYPASS:
        try:
            from PIL import Image  # type: ignore
            with Image.open(io_bytes(raw_bytes)) as im:
                w, h = im.size
                if target_w > w or target_h > h:
                    raise ValueError(f"Cannot center-crop {w}x{h} to larger {target_w}x{target_h}")
                im.load()
                del raw_bytes
                dx = (w - target_w) // 2
                dy = (h - target_h) // 2
                im.crop((dx, dy, dx + target_w, dy + target_h)).save(dst_png)
                return
        except ImportError:
            pass
    w, h, rgba = _decode_png_rgba_bytes_stdlib(raw_bytes)
    del raw_bytes
    if target_w > w or target_h > h:
        raise ValueError(f"Cannot center-crop {w}x{h} to larger {target_w}x{target_h}")
    dx = (w - target_w) // 2
    dy = (h - target_h) // 2
    out = bytearray(target_w * target_h * 4)
    for row in range(target_h):
        src_off = ((dy + row) * w + dx) * 4
        dst_off = row * target_w * 4
        out[dst_off:dst_off + target_w * 4] = rgba[src_off:src_off + target_w * 4]
    del rgba
    dst_png.write_bytes(_encode_png_rgba_stdlib(target_w, target_h, out))


def build_blank_white_png(w: int, h: int, dst_png: Path) -> None:
    rgba = bytes([255, 255, 255, 255]) * (w * h)
    dst_png.write_bytes(_encode_png_rgba_stdlib(w, h, rgba))


def compare_png_pixels(png_a: Path, png_b: Path) -> dict[str, Any]:
    """Compare two PNGs pixel-by-pixel using PIL.ImageChops or pure-stdlib PNG fallback."""
    tall_png = max(_peek_png_height(png_a), _peek_png_height(png_b)) > _PNG_TALL_HEIGHT_PIL_BYPASS
    if not tall_png:
        try:
            from PIL import Image, ImageChops  # type: ignore

            raw_bytes_a = png_a.read_bytes()
            ihdr_a = _validate_png_bytes_full(raw_bytes_a)
            if ihdr_a["width"] * ihdr_a["height"] > _MAX_PNG_COMPARE_PIXELS:
                raise ValueError(
                    f"PNG comparison pixel count {ihdr_a['width'] * ihdr_a['height']} exceeds limit {_MAX_PNG_COMPARE_PIXELS}"
                )
            with Image.open(io_bytes(raw_bytes_a)) as ia:
                ra = _pil_image_to_rgb_image(ia, raw_png_bytes=raw_bytes_a)
            del raw_bytes_a

            raw_bytes_b = png_b.read_bytes()
            ihdr_b = _validate_png_bytes_full(raw_bytes_b)
            if ihdr_b["width"] * ihdr_b["height"] > _MAX_PNG_COMPARE_PIXELS:
                raise ValueError(
                    f"PNG comparison pixel count {ihdr_b['width'] * ihdr_b['height']} exceeds limit {_MAX_PNG_COMPARE_PIXELS}"
                )
            with Image.open(io_bytes(raw_bytes_b)) as ib:
                rb = _pil_image_to_rgb_image(ib, raw_png_bytes=raw_bytes_b)
            del raw_bytes_b

            size_a = ra.size
            size_b = rb.size
            if size_a != size_b:
                rb_resized = rb.resize(size_a)
                del rb
                size_match = False
            else:
                rb_resized = rb
                size_match = True
            diff = ImageChops.difference(ra, rb_resized)
            raw = diff.tobytes()
            del diff
            total = max(1, len(raw))
            nonzero_channels = sum(1 for b in raw if b > 4)
            mean_abs = sum(raw) / total
            max_diff = max(raw) if raw else 0
            del raw
            iou = _non_white_iou_rgb(ra.tobytes(), rb_resized.tobytes())
            return {
                "backend": "PIL",
                "size_a": size_a,
                "size_b": size_b,
                "size_match": size_match,
                "mean_abs_diff": mean_abs,
                "max_channel_diff": max_diff,
                "nonzero_diff_ratio": nonzero_channels / total,
                "non_white_iou": iou,
            }
        except ImportError:
            pass

    wa, ha, raw_a = _decode_png_rgb_bytes_checked(png_a, max_pixels=_MAX_PNG_COMPARE_PIXELS)
    wb, hb, raw_b = _decode_png_rgb_bytes_checked(png_b, max_pixels=_MAX_PNG_COMPARE_PIXELS)
    size_match = (wa, ha) == (wb, hb)
    if size_match:
        rb_bytes = raw_b
    else:
        rb_bytes = _resample_rgb_nearest(raw_b, wb, hb, wa, ha)
        del raw_b
    total = max(1, len(raw_a))
    abs_sum = 0
    nonzero_channels = 0
    max_diff = 0
    for a_val, b_val in zip(raw_a, rb_bytes):
        d = abs(a_val - b_val)
        abs_sum += d
        if d > 4:
            nonzero_channels += 1
        if d > max_diff:
            max_diff = d
    iou = _non_white_iou_rgb(raw_a, rb_bytes)
    return {
        "backend": "stdlib_fallback",
        "size_a": (wa, ha),
        "size_b": (wb, hb),
        "size_match": size_match,
        "mean_abs_diff": abs_sum / total,
        "max_channel_diff": max_diff,
        "nonzero_diff_ratio": nonzero_channels / total,
        "non_white_iou": iou,
    }


_RUST_HEX_DIGITS = frozenset("0123456789abcdefABCDEF")


def _match_rust_char_literal(src: str, i: int) -> Optional[int]:
    """If `src[i:]` starts a valid Rust char (`'...'`) or byte (`b'...'`) literal, return end index."""
    n = len(src)
    ch = src[i]
    if ch == "b":
        if i > 0 and (src[i - 1].isalnum() or src[i - 1] == "_"):
            return None
        if i + 1 >= n or src[i + 1] != "'":
            return None
        k = i + 2
    elif ch == "'":
        k = i + 1
    else:
        return None

    if k >= n:
        return None
    if src[k] == "\\":
        m = k + 1
        if m >= n:
            return None
        esc_ch = src[m]
        if esc_ch == "x":
            if m + 2 < n and src[m + 1] in _RUST_HEX_DIGITS and src[m + 2] in _RUST_HEX_DIGITS:
                end_esc = m + 3
            else:
                return None
        elif esc_ch == "u" and m + 1 < n and src[m + 1] == "{":
            p = m + 2
            hex_digits = 0
            while p < n and (src[p] in _RUST_HEX_DIGITS or src[p] == "_"):
                if src[p] != "_":
                    hex_digits += 1
                p += 1
            if hex_digits >= 1 and p < n and src[p] == "}":
                end_esc = p + 1
            else:
                return None
        elif esc_ch not in ("\n", "\r"):
            end_esc = m + 1
        else:
            return None
        if end_esc < n and src[end_esc] == "'":
            return end_esc + 1
        return None

    if src[k] not in ("'", "\n", "\r") and k + 1 < n and src[k + 1] == "'":
        return k + 2
    return None


def strip_rust_comments_and_strings(src: str) -> str:
    """Strip Rust //, nested /* ... */, char/byte literals, and string literals so grep checks never match comments."""
    out: list[str] = []
    i = 0
    n = len(src)
    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        if ch == "/" and nxt == "/":
            i += 2
            while i < n and src[i] != "\n":
                i += 1
            continue
        if ch == "/" and nxt == "*":
            depth = 1
            i += 2
            nl_count = 0
            while i < n and depth > 0:
                if i + 1 < n and src[i] == "/" and src[i + 1] == "*":
                    depth += 1
                    i += 2
                elif i + 1 < n and src[i] == "*" and src[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    if src[i] == "\n":
                        nl_count += 1
                    i += 1
            out.append("\n" * nl_count if nl_count else " ")
            continue
        if ch in ("r", "b") and (
            (ch == "r" and (nxt == '"' or nxt == "#"))
            or (ch == "b" and i + 1 < n and src[i + 1] == "r" and i + 2 < n and src[i + 2] in ('"', "#"))
        ):
            j = i + (2 if ch == "b" else 1)
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                j += 1
                end_pat = '"' + ("#" * hashes)
                end_idx = src.find(end_pat, j)
                end_pos = n if end_idx == -1 else end_idx + len(end_pat)
                nl_count = src[i:end_pos].count("\n")
                out.append("\n" * nl_count if nl_count else " ")
                i = end_pos
                continue
        char_end = _match_rust_char_literal(src, i)
        if char_end is not None:
            out.append(" ")
            i = char_end
            continue
        if ch == '"' or (ch == "b" and nxt == '"'):
            start = i
            i += 2 if ch == "b" else 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                elif src[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            nl_count = src[start:i].count("\n")
            out.append("\n" * nl_count if nl_count else " ")
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def strip_rust_comments_only(src: str) -> str:
    """Strip Rust // and nested /* ... */ comments while preserving string and char/byte literals."""
    out: list[str] = []
    i = 0
    n = len(src)
    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        if ch == "/" and nxt == "/":
            i += 2
            while i < n and src[i] != "\n":
                i += 1
            continue
        if ch == "/" and nxt == "*":
            depth = 1
            i += 2
            nl_count = 0
            while i < n and depth > 0:
                if i + 1 < n and src[i] == "/" and src[i + 1] == "*":
                    depth += 1
                    i += 2
                elif i + 1 < n and src[i] == "*" and src[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    if src[i] == "\n":
                        nl_count += 1
                    i += 1
            out.append("\n" * nl_count if nl_count else " ")
            continue
        if ch in ("r", "b") and (
            (ch == "r" and (nxt == '"' or nxt == "#"))
            or (ch == "b" and i + 1 < n and src[i + 1] == "r" and i + 2 < n and src[i + 2] in ('"', "#"))
        ):
            j = i + (2 if ch == "b" else 1)
            hashes = 0
            while j < n and src[j] == "#":
                hashes += 1
                j += 1
            if j < n and src[j] == '"':
                j += 1
                end_pat = '"' + ("#" * hashes)
                end_idx = src.find(end_pat, j)
                end_pos = n if end_idx == -1 else end_idx + len(end_pat)
                out.append(src[i:end_pos])
                i = end_pos
                continue
        char_end = _match_rust_char_literal(src, i)
        if char_end is not None:
            out.append(src[i:char_end])
            i = char_end
            continue
        if ch == '"' or (ch == "b" and nxt == '"'):
            start = i
            i += 2 if ch == "b" else 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                elif src[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            out.append(src[start:i])
            continue
        out.append(ch)
        i += 1
    return "".join(out)


# ---------------------------------------------------------------------------
# Deterministic Offline Fixture Builders (Pure Python Stdlib)
# ---------------------------------------------------------------------------

_FIXED_DOC_UUID = "11111111-1111-4111-8111-111111111111"
_FIXED_PAL_UUID = "22222222-2222-4222-8222-222222222222"
_FIXED_TS = "2026-11-22T00:00:00Z"


def _default_outline(color: dict[str, Any], width_mm: float = 0.25, hairline: bool = False) -> dict[str, Any]:
    return {
        "color": color,
        "width_mm": width_mm,
        "is_hairline": hairline,
        "cap": "Butt",
        "join": "Miter",
        "miter_limit": 4.0,
        "dash_pattern": [],
        "start_arrow": "None",
        "end_arrow": "None",
        "behind_fill": False,
        "scale_with_object": False,
        "overprint": False,
        "calligraphy_angle_deg": 0.0,
        "calligraphy_stretch": 1.0,
    }


def _default_text_style(font_family: str = "Noto Sans", size_pt: float = 18.0) -> dict[str, Any]:
    return {
        "font_family": font_family,
        "font_size_pt": size_pt,
        "bold": False,
        "font_weight": 400,
        "bidi_font_family": None,
        "opentype_features": {"liga": True, "calt": True, "kern": True, "dlig": False, "smcp": False},
        "italic": False,
        "underline": False,
        "strikethrough": False,
        "letter_spacing_mm": 0.0,
        "line_height_ratio": 1.2,
        "fill": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}},
        "direction": "Auto",
        "align": "Left",
        "kashida_stretch": 0.0,
    }


def _wrap_coree_zip(title: str, document: dict[str, Any]) -> bytes:
    manifest = {
        "format_version": 1,
        "app": "coree",
        "app_version": "0.3.0",
        "document_id": document["id"],
        "title": title,
        "created_at": _FIXED_TS,
        "modified_at": _FIXED_TS,
        "page_count": len(document["pages"]),
        "asset_count": len(document["assets"]),
        "has_preview": False,
    }
    buf = io_bytes(b"")
    with zipfile.ZipFile(buf, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        for name, payload in (
            ("manifest.json", json.dumps(manifest, indent=2, ensure_ascii=False).encode("utf-8")),
            ("document.json", json.dumps(document, indent=2, ensure_ascii=False).encode("utf-8")),
        ):
            zi = zipfile.ZipInfo(name, date_time=(2026, 11, 22, 0, 0, 0))
            zi.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(zi, payload)
    return buf.getvalue()


def build_coree_bytes(title: str = "Tier1 Fixture", color_mode: str = "Cmyk") -> bytes:
    cmyk_cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_magenta = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_black = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}
    cut_spot = {"Spot": {"name": "CutContour", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.0, 0.0], "alpha": 1.0}}

    objects = {
        "3": {
            "id": 3,
            "name": "Primary Rect",
            "payload": {"Shape": {"Rectangle": {"width": 90.0, "height": 55.0, "corner_radii": [3.0, 3.0, 3.0, 3.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": {"Solid": cmyk_cyan},
            "outline": _default_outline(cmyk_black, 0.4, False),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
        "4": {
            "id": 4,
            "name": "Badge Ellipse",
            "payload": {"Shape": {"Ellipse": {"rx": 22.0, "ry": 16.0, "mode": "Full", "start_deg": 0.0, "end_deg": 360.0}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 130.0, 60.0]},
            "fill": {"Solid": cmyk_magenta},
            "outline": _default_outline(cmyk_black, 0.3, False),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
        "5": {
            "id": 5,
            "name": "Bilingual Title",
            "payload": {
                "ArtisticText": {
                    "runs": [
                        {"text": "coree Tier1 ", "style": _default_text_style("Noto Sans", 18.0)},
                        {"text": "كوري", "style": _default_text_style("Noto Naskh Arabic", 18.0)},
                    ],
                    "direction": "Auto",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 100.0]},
            "fill": "None",
            "outline": _default_outline(cmyk_black, 0.0, False),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
        "7": {
            "id": 7,
            "name": "Die Cut Box",
            "payload": {"Shape": {"Rectangle": {"width": 94.0, "height": 59.0, "corner_radii": [3.0, 3.0, 3.0, 3.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 18.0, 18.0]},
            "fill": "None",
            "outline": _default_outline(cut_spot, 0.0762, True),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
    }

    document = {
        "id": _FIXED_DOC_UUID,
        "title": title,
        "color_mode": color_mode,
        "default_unit": "Millimeter",
        "dpi": 300.0,
        "pages": [
            {
                "id": 1,
                "name": "Page 1",
                "width_mm": 210.0,
                "height_mm": 297.0,
                "bleed_mm": 3.0,
                "facing_pages": False,
                "layers": [
                    {
                        "id": 2,
                        "name": "Artwork",
                        "visible": True,
                        "locked": False,
                        "printable": True,
                        "master_scope": "None",
                        "is_cut_layer": False,
                        "is_desktop_layer": False,
                        "objects": [3, 4, 5],
                    },
                    {
                        "id": 6,
                        "name": "CutContour",
                        "visible": True,
                        "locked": False,
                        "printable": True,
                        "master_scope": "None",
                        "is_cut_layer": True,
                        "is_desktop_layer": False,
                        "objects": [7],
                    },
                ],
            }
        ],
        "active_page_index": 0,
        "multipage_view": False,
        "objects": objects,
        "assets": {},
        "palettes": [],
        "document_palette": {
            "id": _FIXED_PAL_UUID,
            "name": "Document Palette",
            "color_mode": color_mode,
            "is_read_only": False,
            "swatches": [
                {"name": "Cyan", "color": cmyk_cyan},
                {"name": "Magenta", "color": cmyk_magenta},
            ],
        },
        "color_styles": [],
        "object_styles": [],
        "symbols": {},
        "guides": [],
        "grid": {"visible": False, "snap": False, "spacing_x_mm": 10.0, "spacing_y_mm": 10.0, "subdivisions": 2, "show_dots": False},
        "perspective_grid": {"enabled": False, "mode": "TwoPoint", "horizon_y_mm": 100.0, "vanishing_points": [{"x": -40.0, "y": 100.0}, {"x": 250.0, "y": 100.0}], "density": 12, "opacity": 0.35},
        "metadata": {"author": "coree-e2e", "subject": "Tier1 Fixture", "keywords": ["vector", "cmyk"], "notes": "Deterministic offline fixture", "created_at": _FIXED_TS, "modified_at": _FIXED_TS, "source_format": None, "app_version": "0.3.0"},
        "unknown_doc_chunks": [],
        "custom_icc_profile": None,
        "custom_icc_name": None,
        "next_id": 8,
    }
    return _wrap_coree_zip(title, document)


def build_print_merge_template_coree_bytes() -> bytes:
    cmyk_cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_black = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}
    objects = {
        "3": {
            "id": 3,
            "name": "Greeting Field",
            "payload": {
                "ArtisticText": {
                    "runs": [{"text": "Name: <<Name>> | City: <<City>>", "style": _default_text_style("Noto Sans", 16.0)}],
                    "direction": "Auto",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 30.0]},
            "fill": "None",
            "outline": _default_outline(cmyk_black, 0.0, False),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
        "4": {
            "id": 4,
            "name": "Barcode:QR:<<Code>>",
            "payload": {"Shape": {"Rectangle": {"width": 30.0, "height": 30.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 60.0]},
            "fill": {"Solid": cmyk_cyan},
            "outline": _default_outline(cmyk_cyan, 0.0, False),
            "lens": None,
            "effects": [],
            "blend_mode": "Normal",
            "opacity": 1.0,
            "visible": True,
            "locked": False,
            "printable": True,
            "style_id": None,
            "unknown_chunks": [],
        },
    }
    document = {
        "id": "33333333-3333-4333-8333-333333333333",
        "title": "Print Merge Template",
        "color_mode": "Cmyk",
        "default_unit": "Millimeter",
        "dpi": 300.0,
        "pages": [{"id": 1, "name": "Page 1", "width_mm": 150.0, "height_mm": 120.0, "bleed_mm": 3.0, "facing_pages": False, "layers": [{"id": 2, "name": "Layer 1", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [3, 4]}]}],
        "active_page_index": 0,
        "multipage_view": False,
        "objects": objects,
        "assets": {},
        "palettes": [],
        "document_palette": {"id": _FIXED_PAL_UUID, "name": "Document Palette", "color_mode": "Cmyk", "is_read_only": False, "swatches": []},
        "color_styles": [],
        "object_styles": [],
        "symbols": {},
        "guides": [],
        "grid": {"visible": False, "snap": False, "spacing_x_mm": 10.0, "spacing_y_mm": 10.0, "subdivisions": 2, "show_dots": False},
        "perspective_grid": {"enabled": False, "mode": "TwoPoint", "horizon_y_mm": 100.0, "vanishing_points": [{"x": -40.0, "y": 100.0}, {"x": 250.0, "y": 100.0}], "density": 12, "opacity": 0.35},
        "metadata": {"author": "coree-e2e", "subject": "Print Merge Template", "keywords": [], "notes": "", "created_at": _FIXED_TS, "modified_at": _FIXED_TS, "source_format": None, "app_version": "0.3.0"},
        "unknown_doc_chunks": [],
        "custom_icc_profile": None,
        "custom_icc_name": None,
        "next_id": 5,
    }
    return _wrap_coree_zip("Print Merge Template", document)


def build_svg_bytes() -> bytes:
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<svg xmlns="http://www.w3.org/2000/svg" width="210mm" height="297mm" viewBox="0 0 793.7008 1122.5197">\n'
        '  <g id="layer-artwork" data-layer="Artwork">\n'
        '    <rect x="40" y="40" width="220" height="140" fill="#00A8E8" stroke="#111111" stroke-width="2"/>\n'
        '    <circle cx="380" cy="180" r="70" fill="#E63946" stroke="#000000" stroke-width="1.5"/>\n'
        '    <text x="40" y="240" font-family="Noto Sans" font-size="24" fill="#111111">coree SVG</text>\n'
        '  </g>\n'
        '  <g id="layer-cut" data-layer="CutContour">\n'
        '    <path d="M 30 30 L 470 30 L 470 280 L 30 280 Z" fill="none" stroke="#FF00FF" stroke-width="0.288"/>\n'
        '  </g>\n'
        '</svg>\n'
    ).encode("utf-8")


def build_svgz_bytes() -> bytes:
    return gzip.compress(build_svg_bytes(), mtime=0)


def build_eps_bytes() -> bytes:
    return (
        "%!PS-Adobe-3.0 EPSF-3.0\n"
        "%%BoundingBox: 0 0 595 842\n"
        "%%HiResBoundingBox: 0.0000 0.0000 595.2756 841.8898\n"
        "%%LanguageLevel: 3\n"
        "%%Pages: 1\n"
        "%%EndComments\n"
        "gsave\n"
        "0.0 0.65 0.91 setrgbcolor\n"
        "72 680 moveto 280 680 lineto 280 780 lineto 72 780 lineto closepath fill\n"
        "0 0 0 setrgbcolor\n"
        "1.5 setlinewidth\n"
        "72 680 moveto 280 680 lineto 280 780 lineto 72 780 lineto closepath stroke\n"
        "grestore\n"
        "%%EOF\n"
    ).encode("ascii")


def build_pdf_bytes() -> bytes:
    objects = [
        b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n",
        (
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595.2756 841.8898] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n"
        ),
    ]
    stream_body = b"0 0.55 0.88 rg\n72 680 200 100 re f\nBT /F1 18 Tf 72 640 Td (coree PDF) Tj ET\n"
    objects.append(
        f"4 0 obj\n<< /Length {len(stream_body)} >>\nstream\n".encode("ascii")
        + stream_body
        + b"endstream\nendobj\n"
    )
    objects.append(
        b"5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>\nendobj\n"
    )

    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets: list[int] = []
    for obj in objects:
        offsets.append(len(out))
        out.extend(obj)

    xref_offset = len(out)
    out.extend(f"xref\n0 {len(objects) + 1}\n".encode("ascii"))
    out.extend(b"0000000000 65535 f \n")
    for off in offsets:
        out.extend(f"{off:010d} 00000 n \n".encode("ascii"))
    out.extend(f"trailer\n<< /Size {len(objects) + 1} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n".encode("ascii"))
    return bytes(out)


def build_dxf_bytes() -> bytes:
    return (
        "0\nSECTION\n2\nHEADER\n9\n$ACADVER\n1\nAC1015\n9\n$INSUNITS\n70\n4\n0\nENDSEC\n"
        "0\nSECTION\n2\nTABLES\n0\nTABLE\n2\nLAYER\n70\n2\n"
        "0\nLAYER\n2\n0\n70\n0\n62\n7\n6\nCONTINUOUS\n"
        "0\nLAYER\n2\nCutContour\n70\n0\n62\n6\n6\nCONTINUOUS\n"
        "0\nENDTAB\n0\nENDSEC\n"
        "0\nSECTION\n2\nENTITIES\n"
        "0\nLWPOLYLINE\n8\nCutContour\n90\n4\n70\n1\n"
        "10\n10.0\n20\n10.0\n10\n90.0\n20\n10.0\n10\n90.0\n20\n60.0\n10\n10.0\n20\n60.0\n"
        "0\nCIRCLE\n8\n0\n10\n140.0\n20\n80.0\n30\n0.0\n40\n22.0\n"
        "0\nENDSEC\n0\nEOF\n"
    ).encode("ascii")


def build_plt_bytes() -> bytes:
    return b"IN;SP1;PU400,400;PD3600,400,3600,2400,400,2400,400,400;PU;SP2;PA2000,1400;CI450;PU;SP0;\n"


def build_psd_bytes() -> bytes:
    w, h = 4, 4
    header = struct.pack(">4sH6sHIIHH", b"8BPS", 1, b"\x00" * 6, 3, h, w, 8, 3)
    sections = struct.pack(">III", 0, 0, 0)
    r_plane = bytes([20] * (w * h))
    g_plane = bytes([165] * (w * h))
    b_plane = bytes([230] * (w * h))
    comp = struct.pack(">H", 0) + r_plane + g_plane + b_plane
    return header + sections + comp


def build_gpl_bytes() -> bytes:
    return (
        "GIMP Palette\nName: Tier1 Palette\nColumns: 0\n#\n"
        "  0 168 232\tCyan Swatch\n"
        "220  40  60\tRed Swatch\n"
    ).encode("utf-8")


def build_ase_bytes() -> bytes:
    name_u16 = "Tier1 Cyan\x00".encode("utf-16-be")
    block_body = (
        struct.pack(">H", len(name_u16) // 2)
        + name_u16
        + b"CMYK"
        + struct.pack(">ffff", 1.0, 0.0, 0.0, 0.0)
        + struct.pack(">H", 2)
    )
    return b"ASEF" + struct.pack(">HHI", 1, 0, 1) + struct.pack(">HI", 0x0001, len(block_body)) + block_body


def build_pal_bytes() -> bytes:
    return b"JASC-PAL\n0100\n2\n0 168 232\n220 40 60\n"


def _riff_chunk(fourcc: bytes, payload: bytes) -> bytes:
    pad = b"\x00" if (len(payload) & 1) else b""
    return fourcc + struct.pack("<I", len(payload)) + payload + pad


def _riff_list(list_type: bytes, children: list[bytes]) -> bytes:
    body = list_type + b"".join(children)
    pad = b"\x00" if (len(body) & 1) else b""
    return b"LIST" + struct.pack("<I", len(body)) + body + pad


def _build_minimal_cdr_riff_payload(form_type: bytes, version: int) -> bytes:
    doc_mcfg = struct.pack("<iiBfH", 2_100_000, 2_970_000, 0, 300.0, 5) + b"Tier1"
    page_mcfg = struct.pack("<iiiBH", 2_100_000, 2_970_000, 30_000, 0, 6) + b"Page 1"
    layr_hdr = struct.pack("<BBH", 0x05, 0, 7) + b"Layer 1"

    arg0 = struct.pack("<BBfH", 0x05, 0, 1.0, 4) + b"Box1"
    arg1 = struct.pack("<iiiiiiBB", 800_000, 500_000, 0, 0, 0, 0, 0, 1)
    loda_args = arg0 + arg1
    args_offset = 12 + len(loda_args)
    loda_payload = (
        struct.pack("<III", 2, args_offset, args_offset + 8)
        + loda_args
        + struct.pack("<II", 12, 12 + len(arg0))
        + struct.pack("<II", 0x00D0, 0x0001)
    )

    obj_list = _riff_list(b"obj ", [_riff_chunk(b"loda", loda_payload)])
    layr_list = _riff_list(b"layr", [_riff_chunk(b"layr", layr_hdr), obj_list])
    page_list = _riff_list(b"page", [_riff_chunk(b"mcfg", page_mcfg), layr_list])
    doc_list = _riff_list(b"doc ", [_riff_chunk(b"mcfg", doc_mcfg), page_list])

    inner = form_type + _riff_chunk(b"vrsn", struct.pack("<H", version)) + doc_list
    return b"RIFF" + struct.pack("<I", len(inner)) + inner


def build_cdr_riff_bytes() -> bytes:
    return _build_minimal_cdr_riff_payload(b"CDRC", 1200)


def build_cdr_x4_bytes() -> bytes:
    buf = io_bytes(b"")
    with zipfile.ZipFile(buf, "w") as zf:
        zi = zipfile.ZipInfo("mimetype", date_time=(2026, 11, 22, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_STORED
        zf.writestr(zi, b"application/x-vnd.corel.draw.document+zip")
        zi2 = zipfile.ZipInfo("content/riffData.cdr", date_time=(2026, 11, 22, 0, 0, 0))
        zi2.compress_type = zipfile.ZIP_DEFLATED
        zf.writestr(zi2, _build_minimal_cdr_riff_payload(b"CDRE", 1400))
    return buf.getvalue()


def build_cdr_x6_bytes() -> bytes:
    doc_mcfg = struct.pack("<iiBfH", 2_100_000, 2_970_000, 0, 300.0, 5) + b"Tier1"
    page_mcfg = struct.pack("<iiiBH", 2_100_000, 2_970_000, 30_000, 0, 6) + b"Page 1"
    layr_hdr = struct.pack("<BBH", 0x05, 0, 7) + b"Layer 1"

    arg0 = struct.pack("<BBfH", 0x05, 0, 1.0, 4) + b"Box1"
    arg1 = struct.pack("<iiiiiiBB", 800_000, 500_000, 0, 0, 0, 0, 0, 1)
    loda_args = arg0 + arg1
    args_offset = 12 + len(loda_args)
    loda_payload = (
        struct.pack("<III", 2, args_offset, args_offset + 8)
        + loda_args
        + struct.pack("<II", 12, 12 + len(arg0))
        + struct.pack("<II", 0x00D0, 0x0001)
    )

    data_blob = bytearray()

    def add_ext(payload: bytes) -> bytes:
        off = len(data_blob)
        data_blob.extend(payload)
        return struct.pack("<IIII", 0, len(payload), off, 0)

    vrsn_inline_ptr = struct.pack("<II", 0xFFFFFFFF, 2) + struct.pack("<H", 1600) + b"\x00" * 6
    doc_mcfg_ptr = add_ext(doc_mcfg)
    page_mcfg_ptr = add_ext(page_mcfg)
    layr_ptr = add_ext(layr_hdr)
    loda_ptr = add_ext(loda_payload)

    obj_list = _riff_list(b"obj ", [_riff_chunk(b"loda", loda_ptr)])
    layr_list = _riff_list(b"layr", [_riff_chunk(b"layr", layr_ptr), obj_list])
    page_list = _riff_list(b"page", [_riff_chunk(b"mcfg", page_mcfg_ptr), layr_list])
    doc_list = _riff_list(b"doc ", [_riff_chunk(b"mcfg", doc_mcfg_ptr), page_list])
    inner = b"CDRO" + _riff_chunk(b"vrsn", vrsn_inline_ptr) + doc_list
    root_dat = b"RIFF" + struct.pack("<I", len(inner)) + inner

    buf = io_bytes(b"")
    with zipfile.ZipFile(buf, "w") as zf:
        zi = zipfile.ZipInfo("mimetype", date_time=(2026, 11, 22, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_STORED
        zf.writestr(zi, b"application/x-vnd.corel.zcf.draw.document+zip")
        for name, data in (
            ("content/root.dat", root_dat),
            ("content/data/0000.dat", bytes(data_blob)),
            ("content/dataFileList.dat", b"data/0000.dat\n"),
            ("styles/document.cdss", json.dumps({
                "version": 16,
                "style": {
                    "~graphic~": {
                        "fill": {"type": 1, "primaryColor": "CMYK,100,0,0,0"},
                        "outline": {"width": 0.25, "color": "CMYK,0,0,0,100"},
                    }
                },
            }).encode("utf-8")),
            (
                "color/docPalette.xml",
                b'<?xml version="1.0"?><palette name="Doc"><colors><color cs="CMYK" name="Cyan" tints="100,0,0,0"/><color cs="CMYK" name="Magenta" tints="0,100,0,0"/></colors></palette>',
            ),
            ("metadata/metadata.xml", b"<metadata><title>Tier1 X6</title></metadata>"),
        ):
            zi_e = zipfile.ZipInfo(name, date_time=(2026, 11, 22, 0, 0, 0))
            zi_e.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(zi_e, data)
    return buf.getvalue()


def build_cdt_bytes() -> bytes:
    buf = io_bytes(b"")
    with zipfile.ZipFile(buf, "w") as zf:
        zi = zipfile.ZipInfo("mimetype", date_time=(2026, 11, 22, 0, 0, 0))
        zi.compress_type = zipfile.ZIP_STORED
        zf.writestr(zi, b"application/x-vnd.corel.draw.template+zip")
        zi2 = zipfile.ZipInfo("content/riffData.cdr", date_time=(2026, 11, 22, 0, 0, 0))
        zi2.compress_type = zipfile.ZIP_DEFLATED
        zf.writestr(zi2, _build_minimal_cdr_riff_payload(b"CDTE", 1400))
    return buf.getvalue()


def build_cmx_bytes() -> bytes:
    cont = b"CMX2" + struct.pack("<HHHiiiiH", 4, 0x0064, 1, 0, 0, 2_100_000, 2_970_000, 5) + b"Tier1"
    ccmm = struct.pack("<Bf", 0, 300.0)
    pack = struct.pack("<IH", 0, 9) + b"coree-cmx"
    empty_tbl = struct.pack("<H", 0)

    def cmx_cmd(cid: int, body: bytes = b"") -> bytes:
        return struct.pack("<HI", cid, len(body)) + body

    # CMD_BEGIN_PAGE=1 (w, h, bleed, name_idx), CMD_BEGIN_LAYER=3 (flags, ms, name_idx), CMD_END_LAYER=4, CMD_END_PAGE=2
    page_stream = (
        cmx_cmd(1, struct.pack("<iiiH", 2_100_000, 2_970_000, 30_000, 0))
        + cmx_cmd(3, struct.pack("<BBH", 0x05, 0, 0))
        + cmx_cmd(4)
        + cmx_cmd(2)
    )
    inner = (
        b"CMX2"
        + _riff_chunk(b"cont", cont)
        + _riff_chunk(b"ccmm", ccmm)
        + _riff_chunk(b"pack", pack)
        + _riff_chunk(b"rclr", empty_tbl)
        + _riff_chunk(b"rpen", empty_tbl)
        + _riff_chunk(b"rlin", empty_tbl)
        + _riff_chunk(b"rstr", empty_tbl)
        + _riff_chunk(b"page", page_stream)
    )
    return b"RIFF" + struct.pack("<I", len(inner)) + inner


def build_cpt_bytes() -> bytes:
    w, h = 48, 48
    title = b"Tier1"
    hdr = b"CPT9FILE" + struct.pack("<IIIHHffIH", 28 + len(title), w, h, 1, 24, 96.0, 96.0, 1, len(title)) + title
    lname = b"L1"
    pixels = bytes([20, 160, 220] * (w * h))
    # Exact cpt.rs binary layout: name_len(H), name, off_x(i), off_y(i), w(I), h(I), opacity(B), blend(B), flags(B), model(H), comp(B), strip_cnt(I), rows(I), enc_len(I), data, has_alpha(B)
    layer = (
        b"LAYR"
        + struct.pack("<H", len(lname)) + lname
        + struct.pack("<iiIIBBBHBIII", 0, 0, w, h, 255, 0, 1, 1, 0, 1, h, len(pixels))
        + pixels
        + struct.pack("<B", 0)
    )
    return hdr + layer


def build_cdr_riff_with_unknown_xdoc(
    xdoc_payload: bytes = b"COREE_XDOC_PASSTHROUGH_MAGIC_v1",
    xobj_payload: bytes = b"COREE_XOBJ_PASSTHROUGH_MAGIC_v1",
    xlay_payload: bytes = b"COREE_XLAY_PASSTHROUGH_MAGIC_v1",
    xpag_payload: bytes = b"COREE_XPAG_PASSTHROUGH_MAGIC_v1",
) -> bytes:
    """Build a valid RIFF CDR v12 file with synthetic unknown XDOC/XPAG/XLAY/XOBJ chunks."""
    doc_mcfg = struct.pack("<iiBfH", 2_100_000, 2_970_000, 0, 300.0, 5) + b"Tier1"
    page_mcfg = struct.pack("<iiiBH", 2_100_000, 2_970_000, 30_000, 0, 6) + b"Page 1"
    layr_hdr = struct.pack("<BBH", 0x05, 0, 7) + b"Layer 1"

    arg0 = struct.pack("<BBfH", 0x05, 0, 1.0, 4) + b"Box1"
    arg1 = struct.pack("<iiiiiiBB", 800_000, 500_000, 0, 0, 0, 0, 0, 1)
    loda_args = arg0 + arg1
    args_offset = 12 + len(loda_args)
    loda_payload = (
        struct.pack("<III", 2, args_offset, args_offset + 8)
        + loda_args
        + struct.pack("<II", 12, 12 + len(arg0))
        + struct.pack("<II", 0x00D0, 0x0001)
    )

    obj_list = _riff_list(
        b"obj ",
        [_riff_chunk(b"loda", loda_payload), _riff_chunk(b"XOBJ", xobj_payload)],
    )
    layr_list = _riff_list(
        b"layr",
        [_riff_chunk(b"layr", layr_hdr), _riff_chunk(b"XLAY", xlay_payload), obj_list],
    )
    page_list = _riff_list(
        b"page",
        [_riff_chunk(b"mcfg", page_mcfg), _riff_chunk(b"XPAG", xpag_payload), layr_list],
    )
    doc_list = _riff_list(
        b"doc ",
        [_riff_chunk(b"mcfg", doc_mcfg), _riff_chunk(b"XDOC", xdoc_payload), page_list],
    )
    inner = b"CDRC" + _riff_chunk(b"vrsn", struct.pack("<H", 1200)) + doc_list
    return b"RIFF" + struct.pack("<I", len(inner)) + inner


def build_csv_bytes() -> bytes:
    return (
        "Name,Code,City\n"
        "\"Alice, A.\",SKU-001,Cairo\n"
        "\"أحمد علي\",SKU-002,الرياض\n"
    ).encode("utf-8")


def build_rhai_bytes() -> bytes:
    return (
        'let ids = all_object_ids();\n'
        'set_fill_hex(ids, "#00A8E8");\n'
        'recolor_all_to_cmyk();\n'
        'log("tier1-macro-ok objects=" + ids.len());\n'
    ).encode("utf-8")


def build_png_bytes(width: int = 32, height: int = 32) -> bytes:
    """Deterministic 32x32 RGBA PNG with a clear geometric colored box in center."""
    def chunk(ctype: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(ctype + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + ctype + payload + struct.pack(">I", crc)

    raw_rows = bytearray()
    for y in range(height):
        raw_rows.append(0)  # filter None
        for x in range(width):
            if 6 <= x < width - 6 and 6 <= y < height - 6:
                raw_rows.extend((0, 140, 220, 255))
            else:
                raw_rows.extend((255, 255, 255, 255))

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 6, 0, 0, 0)
    phys = struct.pack(">IIB", 11811, 11811, 1)  # 300 DPI
    idat = zlib.compress(bytes(raw_rows), level=6)
    return (
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", ihdr)
        + chunk(b"pHYs", phys)
        + chunk(b"IDAT", idat)
        + chunk(b"IEND", b"")
    )


def build_bmp_bytes(width: int = 16, height: int = 16) -> bytes:
    row_bytes = (width * 3 + 3) & ~3
    img_size = row_bytes * height
    file_hdr = struct.pack("<2sIHHI", b"BM", 54 + img_size, 0, 0, 54)
    dib_hdr = struct.pack("<IIIHHIIIIII", 40, width, height, 1, 24, 0, img_size, 11811, 11811, 0, 0)
    rows = bytearray()
    for y in range(height):
        row = bytearray()
        for x in range(width):
            if 3 <= x < width - 3 and 3 <= y < height - 3:
                row.extend((220, 120, 20))  # BGR
            else:
                row.extend((255, 255, 255))
        while len(row) < row_bytes:
            row.append(0)
        rows.extend(row)
    return file_hdr + dib_hdr + bytes(rows)


def _dwg_crc16(data: bytes, seed: int = 0xC0C1) -> int:
    crc = seed
    for b in data:
        crc ^= b
        for _ in range(8):
            if crc & 1:
                crc = (crc >> 1) ^ 0xA001
            else:
                crc >>= 1
    return crc & 0xFFFF


def build_dwg_probe_bytes() -> bytes:
    """Structurally valid minimal AutoCAD R2000 (AC1015) binary header + section locators + CRC16."""
    # R2000 header: 6B version + 7B reserved/maint + 4B image_seeker + 2B dwg/maint ver + 2B codepage(30=ANSI_1252) + 4B num_records(6)
    num_records = 6
    hdr_prefix_len = 25 + num_records * 9 + 2 + 16  # 25 + 54 + 2 + 16 = 97 bytes

    # Section 0: Header Variables (start sentinel + u32 size + payload + u16 CRC16 + end sentinel)
    hdr_vars_start = bytes.fromhex("CF7B1F23FDDE38A95F7C68B84E6D335F")
    hdr_vars_end = bytes.fromhex("3084E0DC0221C756A0839747B192CCA0")
    hdr_vars_body = struct.pack("<I", 4) + b"\x00\x00\x00\x00"
    hdr_vars_crc = struct.pack("<H", _dwg_crc16(hdr_vars_body))
    sec0 = hdr_vars_start + hdr_vars_body + hdr_vars_crc + hdr_vars_end

    # Section 1: Classes
    cls_start = bytes.fromhex("8DA1C4B8C4A9F8C5C0DCF45FE7CFB68A")
    cls_end = bytes.fromhex("725E3B473B56073A3F230BA018304975")
    cls_body = struct.pack("<I", 0)
    cls_crc = struct.pack("<H", _dwg_crc16(cls_body))
    sec1 = cls_start + cls_body + cls_crc + cls_end

    # Section 2: Object Map (terminator section: big-endian u16 size=2 + u16 CRC16)
    omap_size = struct.pack(">H", 2)
    omap_crc = struct.pack("<H", _dwg_crc16(omap_size))
    sec2 = omap_size + omap_crc

    sec0_off = hdr_prefix_len
    sec1_off = sec0_off + len(sec0)
    sec2_off = sec1_off + len(sec1)

    hdr = bytearray()
    hdr.extend(b"AC1015")
    hdr.extend(b"\x00\x00\x00\x00\x00\x00\x01")
    hdr.extend(struct.pack("<I", 0))         # preview image seeker
    hdr.extend(b"\x18\x00")                  # R2000 dwg_ver / maint_ver
    hdr.extend(struct.pack("<H", 30))        # ANSI_1252 codepage
    hdr.extend(struct.pack("<I", num_records))
    locators = [
        (0, sec0_off, len(sec0)),
        (1, sec1_off, len(sec1)),
        (2, sec2_off, len(sec2)),
        (3, 0, 0),
        (4, 0, 0),
        (5, 0, 0),
    ]
    for rec_id, seeker, size in locators:
        hdr.extend(struct.pack("<BII", rec_id, seeker, size))
    hdr_crc = _dwg_crc16(bytes(hdr))
    hdr.extend(struct.pack("<H", hdr_crc))
    hdr.extend(bytes.fromhex("95A04E2899821AE55E41E05F9D3A4D00"))

    return bytes(hdr) + sec0 + sec1 + sec2


def build_emf_probe_bytes() -> bytes:
    # Minimal EMR_HEADER (0x01) + EMR_EOF (0x0E) probe for W4 ADD-06
    # [MS-EMF] 2.3.4.2: Type(4) Size(4) Bounds(16=4i) Frame(16=4i) Signature(4=' EMF' @ offset 40) Version(4) Bytes(4) Records(4)
    hdr = struct.pack(
        "<IIiiiiiiii4sIII",
        1, 88, 0, 0, 1000, 1000, 0, 0, 10000, 10000, b" EMF", 0x00010000, 108, 2,
    )
    hdr = hdr.ljust(88, b"\x00")
    eof = struct.pack("<IIIII", 14, 20, 0, 16, 20)
    return hdr + eof


def build_acb_probe_bytes() -> bytes:
    # Minimal Adobe Color Book 8BCB v1 CMYK probe for W4 ADD-06
    title = "Tier1 Book".encode("utf-16-be")
    prefix = "T1 ".encode("utf-16-be")
    suffix = "".encode("utf-16-be")
    desc = "".encode("utf-16-be")
    def ustr(b: bytes) -> bytes:
        return struct.pack(">I", len(b) // 2) + b

    swatch_name = "Tier1 Cyan".encode("utf-16-be")
    body = (
        b"8BCB"
        + struct.pack(">HH", 1, 9001)
        + ustr(title) + ustr(prefix) + ustr(suffix) + ustr(desc)
        + struct.pack(">HHHH", 1, 1, 1, 2)  # 1 color, page size 1, page offset 1, CMYK=2
        + ustr(swatch_name)
        + b"T1CY01"
        + bytes([0, 255, 255, 255])  # CMYK inverted bytes per ACB spec (100% Cyan)
    )
    return body


def _write_if_changed(path: Path, data: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    if path.is_file() and path.read_bytes() == data:
        return
    path.write_bytes(data)


def ensure_tier1_fixtures(fixtures_dir: Path) -> dict[str, Path]:
    """Deterministically populate e2e_suite/fixtures/tier1/ offline."""
    builders: dict[str, Callable[[], bytes]] = {
        "fixture.coree": build_coree_bytes,
        "print_merge_template.coree": build_print_merge_template_coree_bytes,
        "fixture.svg": build_svg_bytes,
        "fixture.svgz": build_svgz_bytes,
        "fixture.eps": build_eps_bytes,
        "fixture.pdf": build_pdf_bytes,
        "fixture.dxf": build_dxf_bytes,
        "fixture.plt": build_plt_bytes,
        "fixture.psd": build_psd_bytes,
        "fixture.gpl": build_gpl_bytes,
        "fixture.ase": build_ase_bytes,
        "fixture.pal": build_pal_bytes,
        "fixture_riff.cdr": build_cdr_riff_bytes,
        "fixture_x4.cdr": build_cdr_x4_bytes,
        "fixture_x6.cdr": build_cdr_x6_bytes,
        "fixture.cdt": build_cdt_bytes,
        "fixture.cmx": build_cmx_bytes,
        "fixture.cpt": build_cpt_bytes,
        "merge_data.csv": build_csv_bytes,
        "macro_recolor.rhai": build_rhai_bytes,
        "trace_shape.png": build_png_bytes,
        "trace_shape.bmp": build_bmp_bytes,
        "probe_w4.dwg": build_dwg_probe_bytes,
        "probe_w4.emf": build_emf_probe_bytes,
        "probe_w4.acb": build_acb_probe_bytes,
    }
    out: dict[str, Path] = {}
    for fname, fn in builders.items():
        p = fixtures_dir / fname
        _write_if_changed(p, fn())
        out[fname] = p
    return out
