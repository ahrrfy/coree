"""Tier 2: Boundary Value Analysis, Corrupt/Adversarial Inputs & GUI/ToolCore."""

from __future__ import annotations

import base64
import json
import os
import re
import shutil
import struct
import tomllib
import xml.etree.ElementTree as ET
import zipfile
import zlib
from pathlib import Path
from typing import Any, Optional

from e2e_suite.harness import (
    RunContext,
    SubprocessResult,
    TestCase,
    TestOutcome,
    _FIXED_DOC_UUID,
    _FIXED_PAL_UUID,
    _FIXED_TS,
    _decode_png_rgba_bytes_stdlib,
    _default_outline,
    _default_text_style,
    _encode_png_rgba_stdlib,
    _riff_chunk,
    _riff_list,
    _wrap_coree_zip,
    build_acb_probe_bytes,
    build_cdr_riff_with_unknown_xdoc,
    build_cdt_bytes,
    build_cmx_bytes,
    build_coree_bytes,
    build_cpt_bytes,
    build_dwg_probe_bytes,
    build_emf_probe_bytes,
    build_png_bytes,
    build_print_merge_template_coree_bytes,
    compare_png_pixels,
    dissect_cdr_riff,
    dissect_cdr_zcf,
    dissect_psd,
    inspect_coree_zip,
    parse_pdf_structure,
    parse_png_info,
    parse_svg_structure,
    require_cli_subcommand,
    run_cmd,
    strip_rust_comments_and_strings,
    strip_rust_comments_only,
)

TIER = 2
TIER_NAME = "Boundary, Negative & GUI/ToolCore"

_CLI_PARSE_EXCEPTIONS = (
    ValueError,
    KeyError,
    TypeError,
    IndexError,
    zipfile.BadZipFile,
    json.JSONDecodeError,
    ET.ParseError,
    UnicodeDecodeError,
    UnicodeEncodeError,
    struct.error,
    OSError,
)


def _f(ctx: RunContext, name: str) -> Path:
    return ctx.fixtures_dir / "tier2" / name


def _require_dict(val: Any, context: str) -> dict[str, Any]:
    if not isinstance(val, dict):
        raise ValueError(f"{context}: expected JSON object (dict), got {type(val).__name__}")
    return val


def _assert_no_panic_or_crash(res: SubprocessResult, label: str) -> Optional[TestOutcome]:
    if res.timed_out:
        return TestOutcome("FAIL", f"{label}: subprocess timed out ({res.stderr[-200:]})")
    if res.returncode < 0:
        return TestOutcome("FAIL", f"{label}: terminated by signal {-res.returncode}: {res.stderr[-200:]}")
    combined = f"{res.stdout}\n{res.stderr}"
    if "panicked at" in combined or "RUST_BACKTRACE" in combined:
        return TestOutcome("FAIL", f"{label}: Rust panic detected: {combined[-300:]}")
    return None


def _read_crates_text(ctx: RunContext, rel: str) -> str:
    p = ctx.repo_root / rel
    return p.read_text(encoding="utf-8", errors="replace") if p.is_file() else ""


def _read_rust_stripped(ctx: RunContext, rel: str) -> str:
    p = ctx.repo_root / rel
    files: list[Path] = []
    if p.is_file():
        files.append(p)
    if rel.endswith(".rs"):
        mod_dir = p.with_suffix("")
        if mod_dir.is_dir():
            files.extend(sorted(mod_dir.rglob("*.rs")))
    if not files:
        return ""
    return "\n".join(
        strip_rust_comments_and_strings(f.read_text(encoding="utf-8", errors="replace"))
        for f in files
    )


def _strip_not_clauses(expr: str) -> str:
    out: list[str] = []
    i = 0
    n = len(expr)
    not_rx = re.compile(r"\bnot\s*\(")
    while i < n:
        m = not_rx.search(expr, i)
        if not m:
            out.append(expr[i:])
            break
        out.append(expr[i:m.start()])
        j = m.end()
        depth = 1
        while j < n and depth > 0:
            if expr[j] == "(":
                depth += 1
            elif expr[j] == ")":
                depth -= 1
            j += 1
        out.append(" ")
        i = j
    return "".join(out)


def _strip_cfg_test_modules(stripped_code: str) -> str:
    n = len(stripped_code)
    spans: list[tuple[int, int]] = []
    pos = 0
    cfg_rx = re.compile(r"#\[\s*cfg\s*\(")
    item_kw_rx = re.compile(
        r"\s*(?:(?:pub(?:\([^)]*\))?\s+)?(?:(?:async|unsafe|const)\s+)*(?:(?:extern(?:\s+\"[^\"]*\")?\s+)?fn|extern(?:\s+\"[^\"]*\")?(?=\s*\{)|mod|impl|struct|enum|trait|type|const|static|use)\b|(?:#\[macro_export\]\s*)?macro_rules!\s*[A-Za-z_][A-Za-z0-9_]*)"
    )
    while pos < n:
        m = cfg_rx.search(stripped_code, pos)
        if not m:
            break
        p_depth = 1
        k = m.end()
        while k < n and p_depth > 0:
            if stripped_code[k] == "(":
                p_depth += 1
            elif stripped_code[k] == ")":
                p_depth -= 1
            k += 1
        if p_depth != 0:
            pos = m.end()
            continue
        inner_cfg = stripped_code[m.end():k - 1]
        b_depth = 1
        while k < n and b_depth > 0:
            if stripped_code[k] == "[":
                b_depth += 1
            elif stripped_code[k] == "]":
                b_depth -= 1
            k += 1
        if b_depth != 0:
            pos = m.end()
            continue
        if not re.search(r"\btest\b", _strip_not_clauses(inner_cfg)):
            pos = k
            continue
        cur = k
        while cur < n:
            while cur < n and stripped_code[cur].isspace():
                cur += 1
            if stripped_code[cur:cur + 2] == "#[":
                cur += 2
                ab = 1
                while cur < n and ab > 0:
                    if stripped_code[cur] == "[":
                        ab += 1
                    elif stripped_code[cur] == "]":
                        ab -= 1
                    cur += 1
            else:
                break
        m_item = item_kw_rx.match(stripped_code, cur)
        if not m_item:
            pos = k
            continue
        i = m_item.end()
        paren = 0
        bracket = 0
        end_span = -1
        while i < n:
            ch = stripped_code[i]
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren = max(0, paren - 1)
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket = max(0, bracket - 1)
            elif ch == ";" and paren == 0 and bracket == 0:
                end_span = i + 1
                break
            elif ch == "{" and paren == 0 and bracket == 0:
                depth = 1
                j = i + 1
                while j < n and depth > 0:
                    if stripped_code[j] == "{":
                        depth += 1
                    elif stripped_code[j] == "}":
                        depth -= 1
                    j += 1
                if depth == 0:
                    end_span = j
                break
            i += 1
        if end_span > m.start():
            spans.append((m.start(), end_span))
            pos = end_span
        else:
            pos = k
    if not spans:
        return stripped_code
    out: list[str] = []
    last = 0
    for s, e in spans:
        out.append(stripped_code[last:s])
        last = e
    out.append(stripped_code[last:])
    return "".join(out)


def _extract_brace_block(stripped_code: str, header_rx: str) -> Optional[str]:
    n = len(stripped_code)
    for m in re.finditer(header_rx, stripped_code):
        i = m.end() - 1 if m.group(0).endswith("{") else m.end()
        paren = 0
        bracket = 0
        while i < n:
            ch = stripped_code[i]
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren = max(0, paren - 1)
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket = max(0, bracket - 1)
            elif ch == "{" and paren == 0 and bracket == 0:
                depth = 1
                j = i + 1
                while j < n and depth > 0:
                    if stripped_code[j] == "{":
                        depth += 1
                    elif stripped_code[j] == "}":
                        depth -= 1
                    j += 1
                if depth == 0:
                    return stripped_code[i:j]
                break
            elif ch == ";" and paren == 0 and bracket == 0:
                break
            i += 1
    return None


def _run_cargo(ctx: RunContext, args: list[str], timeout: float = 90.0) -> SubprocessResult:
    cargo_bin = str(Path.home() / ".cargo" / "bin")
    cur_path = os.environ.get("PATH", "")
    new_path = f"{cargo_bin}:{cur_path}" if cargo_bin not in cur_path else cur_path
    if not shutil.which("cargo", path=new_path):
        return SubprocessResult(
            cmd=["cargo", *args],
            returncode=127,
            stdout="",
            stderr="cargo binary not found in PATH or ~/.cargo/bin",
            duration_ms=0.0,
        )
    return run_cmd(
        ["cargo", *args],
        cwd=ctx.repo_root,
        env={"PATH": new_path},
        timeout=timeout,
        timeout_scale=ctx.timeout_scale,
        measure_rss=False,
    )


def _run_cargo_tests(
    ctx: RunContext,
    args: list[str],
    label: str,
    *,
    min_passed: int = 1,
    timeout: float = 90.0,
    required_test_rxs: tuple[str, ...] = (),
    require_any_rx: Optional[str] = None,
    exclude_prefix: Optional[str] = None,
) -> Optional[TestOutcome]:
    cmd_args = list(args) if args and args[0] == "test" else ["test", *args]
    if "--offline" not in cmd_args:
        cmd_args.insert(1, "--offline")
    ct = _run_cargo(ctx, cmd_args, timeout=timeout)
    crash = _assert_no_panic_or_crash(ct, label)
    if crash is not None and ct.returncode < 0:
        return crash
    matches = re.findall(r"(\d+)\s+passed;\s+(\d+)\s+failed", ct.stdout)
    total_passed = sum(int(p) for p, _ in matches)
    total_failed = sum(int(f) for _, f in matches)
    if ct.returncode != 0 or total_failed > 0 or total_passed < min_passed:
        tail_out = ct.stdout[-400:].strip()
        tail_err = ct.stderr[-400:].strip()
        return TestOutcome(
            "FAIL",
            f"{label}: cargo {' '.join(cmd_args)} failed or matched <{min_passed} test(s) "
            f"(rc={ct.returncode}, passed={total_passed}, failed={total_failed}):\n"
            f"{tail_out}\n{tail_err}".strip(),
            metrics={"cargo_passed": total_passed, "cargo_failed": total_failed},
        )
    passed_names = re.findall(r"(?m)^test\s+(\S+)\s+\.\.\.\s+ok\s*$", ct.stdout)
    if exclude_prefix:
        passed_names = [t for t in passed_names if not t.startswith(exclude_prefix)]
    if require_any_rx and not any(re.search(require_any_rx, t, re.I) for t in passed_names):
        return TestOutcome(
            "FAIL",
            f"{label}: cargo {' '.join(cmd_args)} matched 0 qualifying tests matching /{require_any_rx}/ "
            f"(excluded={exclude_prefix!r}, passed={passed_names[:10]})",
        )
    for rx in required_test_rxs:
        if not any(re.search(rx, t, re.I) for t in passed_names):
            return TestOutcome(
                "FAIL",
                f"{label}: no passing test in cargo {' '.join(cmd_args)} matched /{rx}/ (passed={passed_names[:12]})",
            )
    return None


def _build_streaming_solid_png_bytes(width: int, height: int) -> bytes:
    """Build a valid 8-bit grayscale solid PNG in bounded memory (<1 MiB even for >16MP)."""
    def _chunk(tag: bytes, payload: bytes) -> bytes:
        crc = zlib.crc32(tag + payload) & 0xFFFFFFFF
        return struct.pack(">I", len(payload)) + tag + payload + struct.pack(">I", crc)

    ihdr = struct.pack(">IIBBBBB", width, height, 8, 0, 0, 0, 0)
    row = b"\x00" + (b"\xff" * width)
    comp = zlib.compressobj(level=6)
    idat_parts: list[bytes] = []
    for _ in range(height):
        chunk = comp.compress(row)
        if chunk:
            idat_parts.append(chunk)
    tail = comp.flush()
    if tail:
        idat_parts.append(tail)
    return (
        b"\x89PNG\r\n\x1a\n"
        + _chunk(b"IHDR", ihdr)
        + _chunk(b"IDAT", b"".join(idat_parts))
        + _chunk(b"IEND", b"")
    )


def _build_packbits_psd_bytes(width: int, height: int, fill_byte: int = 0xC8) -> bytes:
    """Build a valid 1-channel 8-bit grayscale PackBits PSD for oversized (>2048px) BVA."""
    header = (
        b"8BPS"
        + struct.pack(">H", 1)
        + b"\x00" * 6
        + struct.pack(">HIIHH", 1, height, width, 8, 1)
    )
    encoded_rows: list[bytes] = []
    row_counts: list[int] = []
    for _ in range(height):
        rem = width
        parts: list[bytes] = []
        while rem > 0:
            run = min(128, rem)
            if run == 1:
                parts.append(bytes([0, fill_byte]))
            else:
                parts.append(bytes([(257 - run) & 0xFF, fill_byte]))
            rem -= run
        enc = b"".join(parts)
        encoded_rows.append(enc)
        row_counts.append(len(enc))
    counts_bytes = b"".join(struct.pack(">H", c) for c in row_counts)
    return (
        header
        + struct.pack(">I", 0)
        + struct.pack(">I", 0)
        + struct.pack(">I", 0)
        + struct.pack(">H", 1)
        + counts_bytes
        + b"".join(encoded_rows)
    )


def _write_coree_from_doc(dst: Path, title: str, doc: dict[str, Any]) -> None:
    dst.write_bytes(_wrap_coree_zip(title, doc))


def _minimal_doc_dict(title: str = "Tier2 BVA", width_mm: float = 210.0, height_mm: float = 297.0) -> dict[str, Any]:
    cmyk_cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_black = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}
    return {
        "id": _FIXED_DOC_UUID,
        "title": title,
        "color_mode": "Cmyk",
        "default_unit": "Millimeter",
        "dpi": 300.0,
        "pages": [{
            "id": 1,
            "name": "Page 1",
            "width_mm": width_mm,
            "height_mm": height_mm,
            "bleed_mm": 3.0,
            "facing_pages": False,
            "layers": [{
                "id": 2,
                "name": "Layer 1",
                "visible": True,
                "locked": False,
                "printable": True,
                "master_scope": "None",
                "is_cut_layer": False,
                "is_desktop_layer": False,
                "objects": [3],
            }],
        }],
        "active_page_index": 0,
        "multipage_view": False,
        "objects": {
            "3": {
                "id": 3,
                "name": "Anchor Rect",
                "payload": {
                    "Shape": {
                        "Rectangle": {
                            "width": 60.0,
                            "height": 40.0,
                            "corner_radii": [0.0, 0.0, 0.0, 0.0],
                            "corner_style": "Rounded",
                            "lock_corners": True,
                        }
                    }
                },
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
            }
        },
        "assets": {},
        "palettes": [],
        "document_palette": {
            "id": _FIXED_PAL_UUID,
            "name": "Document Palette",
            "color_mode": "Cmyk",
            "is_read_only": False,
            "swatches": [],
        },
        "color_styles": [],
        "object_styles": [],
        "symbols": {},
        "guides": [],
        "grid": {"visible": False, "snap": False, "spacing_x_mm": 10.0, "spacing_y_mm": 10.0, "subdivisions": 2, "show_dots": False},
        "perspective_grid": {
            "enabled": False,
            "mode": "TwoPoint",
            "horizon_y_mm": 100.0,
            "vanishing_points": [{"x": -40.0, "y": 100.0}, {"x": 250.0, "y": 100.0}],
            "density": 12,
            "opacity": 0.35,
        },
        "metadata": {
            "author": "coree-e2e",
            "subject": "Tier2",
            "keywords": [],
            "notes": "",
            "created_at": _FIXED_TS,
            "modified_at": _FIXED_TS,
            "source_format": None,
            "app_version": "0.3.1",
        },
        "unknown_doc_chunks": [],
        "custom_icc_profile": None,
        "custom_icc_name": None,
        "next_id": 4,
    }


# ---------------------------------------------------------------------------
# BASELINE (smoke=True) TESTS — Pure CLI + Offline Fixtures, Zero Cargo Touch
# ---------------------------------------------------------------------------

def _test_base_cli_bva_numeric(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-BASE-CLI-BVA-NUM")
    doc_path = work / "valid.coree"
    doc_path.write_bytes(build_coree_bytes("T2 BVA"))
    checks = 0

    # 1. Unknown CLI flag -> clap exit 2
    r_flag = run_cmd([str(ctx.cli_bin), "--nonexistent-flag"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_flag, "unknown flag")
    if crash:
        return crash
    if r_flag.returncode == 0:
        return TestOutcome("FAIL", "unknown --nonexistent-flag unexpectedly succeeded (rc=0)")
    checks += 1

    # 2. render --dpi BVA (0, negative, NaN, inf, -inf reject; 1.0 & 72.0 accept)
    for bad_dpi in ("0", "-72", "NaN", "inf", "-inf"):
        out_p = work / f"bad_dpi_{bad_dpi}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "render", str(doc_path), str(out_p), "--dpi", bad_dpi],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"render --dpi {bad_dpi}")
        if crash:
            return crash
        if r.returncode == 0 or out_p.exists():
            return TestOutcome("FAIL", f"render --dpi {bad_dpi} should have failed without writing PNG (rc={r.returncode})")
        checks += 1

    for good_dpi in ("1", "72"):
        out_p = work / f"good_dpi_{good_dpi}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "render", str(doc_path), str(out_p), "--dpi", good_dpi],
            cwd=work,
            timeout=20.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"render --dpi {good_dpi}")
        if crash:
            return crash
        if r.returncode != 0 or not out_p.is_file():
            return TestOutcome("FAIL", f"render --dpi {good_dpi} failed (rc={r.returncode}): {r.stderr}")
        try:
            info = parse_png_info(out_p.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"render --dpi {good_dpi} produced invalid PNG: {exc}")
        if info["width"] < 1 or info["height"] < 1:
            return TestOutcome("FAIL", f"render --dpi {good_dpi} invalid PNG dims: {info}")
        checks += 1

    # 3. render --multipage huge page auto-clamping (<= 16_384 px) + >64M px surface cap rejection
    huge_doc = _minimal_doc_dict("Huge Multipage", width_mm=1000.0, height_mm=100.0)
    huge_doc["pages"].append(dict(huge_doc["pages"][0], id=10, name="Page 2"))
    huge_in = work / "huge_multipage.coree"
    huge_out = work / "huge_multipage.png"
    _write_coree_from_doc(huge_in, "Huge Multipage", huge_doc)
    r_mp = run_cmd(
        [str(ctx.cli_bin), "render", str(huge_in), str(huge_out), "--multipage", "--dpi", "300"],
        cwd=work,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_mp, "render --multipage huge")
    if crash:
        return crash
    if r_mp.returncode != 0 or not huge_out.is_file():
        return TestOutcome("FAIL", f"render --multipage huge doc failed (rc={r_mp.returncode}): {r_mp.stderr}")
    try:
        mp_info = parse_png_info(huge_out.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"render --multipage huge PNG invalid: {exc}")
    if mp_info["width"] > 16_384 or mp_info["height"] > 16_384:
        return TestOutcome("FAIL", f"render --multipage exceeded 16384px cap: {mp_info['width']}x{mp_info['height']}")
    checks += 1

    over64m_doc = _minimal_doc_dict("Over 64MP Multipage", width_mm=5000.0, height_mm=5000.0)
    over64m_doc["pages"].append(dict(over64m_doc["pages"][0], id=10, name="Page 2"))
    over64m_in = work / "over64m_multipage.coree"
    over64m_out = work / "over64m_multipage.png"
    _write_coree_from_doc(over64m_in, "Over 64MP Multipage", over64m_doc)
    r_o64 = run_cmd(
        [str(ctx.cli_bin), "render", str(over64m_in), str(over64m_out), "--multipage", "--dpi", "300"],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_o64, "render --multipage >64MP cap")
    if crash:
        return crash
    if r_o64.returncode == 0 or over64m_out.exists():
        return TestOutcome("FAIL", "render --multipage >64MP surface should fail cleanly without writing PNG")
    checks += 1

    # 4. render --page BVA (0 valid; 1 max+1, 999, u64 max invalid)
    for bad_page in ("1", "999", "18446744073709551615"):
        out_p = work / f"bad_page_{bad_page}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "render", str(doc_path), str(out_p), "--page", bad_page, "--dpi", "72"],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"render --page {bad_page}")
        if crash:
            return crash
        if r.returncode == 0 or out_p.exists():
            return TestOutcome("FAIL", f"render --page {bad_page} on 1-page doc should have failed (rc={r.returncode})")
        checks += 1

    # 5. convert --dpi BVA
    for bad_dpi in ("0", "-1", "NaN", "inf"):
        out_p = work / f"conv_{bad_dpi}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "convert", str(doc_path), str(out_p), "--dpi", bad_dpi],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"convert --dpi {bad_dpi}")
        if crash:
            return crash
        if r.returncode == 0 or out_p.exists():
            return TestOutcome("FAIL", f"convert --dpi {bad_dpi} should have failed")
        checks += 1

    # 6. thumbnail --size BVA (0, 4097 reject; 1 min, 4096 max accept)
    for bad_sz in ("0", "4097"):
        out_p = work / f"thumb_{bad_sz}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "thumbnail", str(doc_path), str(out_p), "--size", bad_sz],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"thumbnail --size {bad_sz}")
        if crash:
            return crash
        if r.returncode == 0 or out_p.exists():
            return TestOutcome("FAIL", f"thumbnail --size {bad_sz} should have failed")
        checks += 1

    for good_sz in (1, 4096):
        out_p = work / f"thumb_{good_sz}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "thumbnail", str(doc_path), str(out_p), "--size", str(good_sz)],
            cwd=work,
            timeout=30.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"thumbnail --size {good_sz}")
        if crash:
            return crash
        if r.returncode != 0 or not out_p.is_file():
            return TestOutcome("FAIL", f"thumbnail --size {good_sz} failed (rc={r.returncode}): {r.stderr}")
        try:
            t_info = parse_png_info(out_p.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"thumbnail --size {good_sz} invalid PNG: {exc}")
        if max(t_info["width"], t_info["height"]) > good_sz or min(t_info["width"], t_info["height"]) < 1:
            return TestOutcome("FAIL", f"thumbnail --size {good_sz} bounds mismatch: {t_info['width']}x{t_info['height']}")
        checks += 1

    # 7. preflight --min-dpi BVA
    for bad_md in ("0", "-150", "NaN", "inf"):
        r = run_cmd(
            [str(ctx.cli_bin), "preflight", str(doc_path), "--min-dpi", bad_md, "--json"],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"preflight --min-dpi {bad_md}")
        if crash:
            return crash
        if r.returncode == 0:
            return TestOutcome("FAIL", f"preflight --min-dpi {bad_md} should have failed")
        checks += 1

    r_pf = run_cmd(
        [str(ctx.cli_bin), "preflight", str(doc_path), "--min-dpi", "1", "--json"],
        cwd=work,
        timeout=15.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_pf, "preflight --min-dpi 1")
    if crash:
        return crash
    if r_pf.returncode != 0:
        return TestOutcome("FAIL", f"preflight --min-dpi 1 failed: {r_pf.stderr}")
    try:
        pf_json = _require_dict(json.loads(r_pf.stdout), "preflight --json")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"preflight --min-dpi 1 invalid JSON: {exc}")
    if not isinstance(pf_json.get("issues"), list):
        return TestOutcome("FAIL", f"preflight JSON missing list 'issues': {sorted(pf_json)}")
    checks += 1

    # 8. laser BVA (dedupe, join, kerf -50..=50, hatch-spacing, hatch-angle, --launch/--plugin)
    bad_laser_cases = [
        (["--dedupe", "-0.01"], "dedupe_neg"),
        (["--dedupe", "NaN"], "dedupe_nan"),
        (["--join", "-0.01"], "join_neg"),
        (["--join", "inf"], "join_inf"),
        (["--kerf", "-50.001"], "kerf_under"),
        (["--kerf", "50.001"], "kerf_over"),
        (["--kerf", "NaN"], "kerf_nan"),
        (["--hatch-spacing", "-0.1"], "hatch_neg"),
        (["--hatch-angle", "inf"], "angle_inf"),
        (["--launch"], "launch_no_plugin"),
        (["--plugin", "nonexistent-plugin-id"], "unknown_plugin"),
    ]
    for extra_args, tag in bad_laser_cases:
        out_p = work / f"laser_{tag}.gcode"
        r = run_cmd(
            [str(ctx.cli_bin), "laser", str(doc_path), str(out_p), *extra_args],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"laser {tag}")
        if crash:
            return crash
        if r.returncode == 0:
            return TestOutcome("FAIL", f"laser {tag} ({extra_args}) should have failed")
        checks += 1

    for ok_kerf in ("-50.0", "0.0", "50.0"):
        out_p = work / f"laser_kerf_{ok_kerf}.gcode"
        r = run_cmd(
            [str(ctx.cli_bin), "laser", str(doc_path), str(out_p), "--kerf", ok_kerf],
            cwd=work,
            timeout=15.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"laser --kerf {ok_kerf}")
        if crash:
            return crash
        if r.returncode != 0 or not out_p.is_file() or out_p.stat().st_size == 0:
            return TestOutcome("FAIL", f"laser --kerf {ok_kerf} failed (rc={r.returncode}): {r.stderr}")
        checks += 1

    # 9. trace --colors BVA & 1x1 minimum raster across modes
    mini_png = work / "mini_1x1.png"
    mini_png.write_bytes(build_png_bytes(1, 1))
    for mode in ("quick", "outline", "centerline"):
        out_svg = work / f"traced_1x1_{mode}.svg"
        r = run_cmd(
            [str(ctx.cli_bin), "trace", str(mini_png), str(out_svg), "--mode", mode, "--colors", "0"],
            cwd=work,
            timeout=20.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"trace 1x1 {mode} colors=0")
        if crash:
            return crash
        if r.returncode != 0 or not out_svg.is_file():
            return TestOutcome("FAIL", f"trace 1x1 {mode} failed (rc={r.returncode}): {r.stderr}")
        try:
            svg_info = parse_svg_structure(out_svg.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"trace 1x1 {mode} invalid SVG: {exc}")
        if svg_info.get("counts", {}).get("svg", 0) < 1:
            return TestOutcome("FAIL", f"trace 1x1 {mode} output is not SVG: {svg_info}")
        checks += 1

    for col_val in ("1", "65", "999999"):
        out_svg = work / f"traced_col_{col_val}.svg"
        r = run_cmd(
            [str(ctx.cli_bin), "trace", str(mini_png), str(out_svg), "--colors", col_val],
            cwd=work,
            timeout=20.0,
            timeout_scale=ctx.timeout_scale,
        )
        crash = _assert_no_panic_or_crash(r, f"trace --colors {col_val}")
        if crash:
            return crash
        if r.returncode != 0 or not out_svg.is_file():
            return TestOutcome("FAIL", f"trace --colors {col_val} clamp failed: {r.stderr}")
        try:
            col_svg_info = parse_svg_structure(out_svg.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"trace --colors {col_val} invalid SVG: {exc}")
        if col_svg_info.get("counts", {}).get("svg", 0) < 1:
            return TestOutcome("FAIL", f"trace --colors {col_val} output is not SVG: {col_svg_info}")
        checks += 1

    # 10. collect BVA & ProductionMetrics manifest.json (EXT-F07)
    collect_valid_dir = work / "collect_valid"
    r_col_ok = run_cmd(
        [str(ctx.cli_bin), "collect", str(doc_path), str(collect_valid_dir)],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_col_ok, "collect valid.coree")
    if crash:
        return crash
    manifest_p = collect_valid_dir / "manifest.json"
    if r_col_ok.returncode != 0 or not manifest_p.is_file():
        return TestOutcome("FAIL", f"collect valid.coree failed (rc={r_col_ok.returncode}): {r_col_ok.stderr}")
    try:
        manifest_json = _require_dict(json.loads(manifest_p.read_text(encoding="utf-8")), "collect manifest.json")
        pm = _require_dict(manifest_json.get("production_metrics"), "collect manifest production_metrics")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"collect manifest.json invalid: {exc}")
    if (
        not isinstance(pm.get("object_count"), int)
        or isinstance(pm.get("object_count"), bool)
        or pm["object_count"] < 1
        or not isinstance(pm.get("total_vector_length_mm"), (int, float))
        or isinstance(pm.get("total_vector_length_mm"), bool)
        or pm["total_vector_length_mm"] <= 0.0
        or not isinstance(pm.get("cut_length_mm"), (int, float))
        or isinstance(pm.get("cut_length_mm"), bool)
        or not isinstance(pm.get("material_utilization_pct"), (int, float))
        or isinstance(pm.get("material_utilization_pct"), bool)
        or not (0.0 <= pm["material_utilization_pct"] <= 100.0)
    ):
        return TestOutcome("FAIL", f"collect ProductionMetrics invalid for valid.coree: {pm!r}")
    checks += 1

    empty_col_doc = _minimal_doc_dict("Empty Collect")
    empty_col_doc["objects"] = {}
    empty_col_doc["pages"][0]["layers"][0]["objects"] = []
    empty_col_in = work / "empty_collect.coree"
    empty_col_dir = work / "collect_empty"
    _write_coree_from_doc(empty_col_in, "Empty Collect", empty_col_doc)
    r_col_empty = run_cmd(
        [str(ctx.cli_bin), "collect", str(empty_col_in), str(empty_col_dir)],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_col_empty, "collect empty.coree")
    if crash:
        return crash
    empty_manifest_p = empty_col_dir / "manifest.json"
    if r_col_empty.returncode != 0 or not empty_manifest_p.is_file():
        return TestOutcome("FAIL", f"collect empty.coree failed (rc={r_col_empty.returncode}): {r_col_empty.stderr}")
    try:
        empty_manifest = _require_dict(json.loads(empty_manifest_p.read_text(encoding="utf-8")), "empty collect manifest.json")
        empty_pm = _require_dict(empty_manifest.get("production_metrics"), "empty collect production_metrics")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"collect empty manifest.json invalid: {exc}")
    if (
        not isinstance(empty_pm.get("object_count"), int)
        or isinstance(empty_pm.get("object_count"), bool)
        or empty_pm.get("object_count") != 0
        or not isinstance(empty_pm.get("total_vector_length_mm"), (int, float))
        or isinstance(empty_pm.get("total_vector_length_mm"), bool)
        or empty_pm.get("total_vector_length_mm") != 0.0
        or not isinstance(empty_pm.get("cut_length_mm"), (int, float))
        or isinstance(empty_pm.get("cut_length_mm"), bool)
        or empty_pm.get("cut_length_mm") != 0.0
    ):
        return TestOutcome("FAIL", f"collect empty.coree expected zero ProductionMetrics, got {empty_pm!r}")
    checks += 1

    missing_col_dir = work / "collect_missing"
    r_col_missing = run_cmd(
        [str(ctx.cli_bin), "collect", str(work / "nonexistent_collect.coree"), str(missing_col_dir)],
        cwd=work,
        timeout=15.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_col_missing, "collect missing.coree")
    if crash:
        return crash
    if r_col_missing.returncode == 0 or missing_col_dir.exists():
        return TestOutcome("FAIL", "collect on nonexistent input should have failed without creating output dir")
    checks += 1

    return TestOutcome(
        "PASS",
        f"CLI numeric/option BVA verified across render/convert/thumbnail/preflight/laser/trace/collect ({checks} checks, 0 panics)",
        metrics={"checks": checks},
    )


def _test_base_corrupt_headers(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-BASE-CORRUPT-HEADERS")
    empty_src = _f(ctx, "empty_0b.cdr")
    if not empty_src.is_file():
        return TestOutcome("FAIL", f"Missing Tier 2 fixture: {empty_src}")
    empty_bytes = empty_src.read_bytes()
    checks = 0

    # 1. 0-byte files across all 17 extensions
    exts = (
        "coree", "cdr", "cdt", "cmx", "cpt", "pdf", "ai", "eps",
        "dxf", "plt", "psd", "svg", "svgz", "png", "bmp", "jpg", "tiff",
    )
    for ext in exts:
        p = work / f"zero.{ext}"
        p.write_bytes(empty_bytes)
        r_ins = run_cmd([str(ctx.cli_bin), "inspect", str(p), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r_ins, f"inspect zero.{ext}")
        if crash:
            return crash
        if r_ins.returncode == 0:
            return TestOutcome("FAIL", f"inspect zero.{ext} unexpectedly exited 0")
        out_png = work / f"out_zero_{ext}.png"
        r_conv = run_cmd([str(ctx.cli_bin), "convert", str(p), str(out_png)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r_conv, f"convert zero.{ext}")
        if crash:
            return crash
        if r_conv.returncode == 0 or out_png.exists():
            return TestOutcome("FAIL", f"convert zero.{ext} unexpectedly succeeded or created output")
        checks += 2

    # 2. Static corrupt header fixtures + dynamic truncated CMX2 / negative-dim BMP / missing-doc .coree / missing path
    bad_cases: list[tuple[str, Path]] = [
        ("truncated_riff", _f(ctx, "truncated_riff.cdr")),
        ("huge_cpt_100k", _f(ctx, "huge_cpt_100k.cpt")),
        ("pdf_encrypted", _f(ctx, "pdf_encrypted.pdf")),
        ("psd_bad_dims", _f(ctx, "psd_bad_dims.psd")),
        ("png_bad_crc", _f(ctx, "png_bad_crc.png")),
    ]

    trunc_cmx = work / "trunc_cmx2.cmx"
    trunc_cmx.write_bytes(b"RIFF\x20\x00\x00\x00CMX2cont\x99\x00\x00\x00")
    bad_cases.append(("trunc_cmx2", trunc_cmx))

    neg_bmp = work / "neg_dim.bmp"
    neg_bmp.write_bytes(
        struct.pack("<2sIHHI", b"BM", 58, 0, 0, 54)
        + struct.pack("<IiiHHIIIIII", 40, -1, 1, 1, 24, 0, 4, 0, 0, 0, 0)
        + b"\x00\x00\x00\x00"
    )
    bad_cases.append(("neg_dim_bmp", neg_bmp))

    missing_doc_coree = work / "no_doc_json.coree"
    with zipfile.ZipFile(missing_doc_coree, "w") as zf:
        zf.writestr("manifest.json", json.dumps({"format_version": 1, "app": "coree", "app_version": "0.3.1"}))
    bad_cases.append(("missing_doc_json_coree", missing_doc_coree))

    bad_cases.append(("nonexistent", work / "does_not_exist.cdr"))

    for label, path in bad_cases:
        if label != "nonexistent" and not path.is_file():
            return TestOutcome("FAIL", f"Missing corrupt fixture for {label}: {path}")
        r_ins = run_cmd([str(ctx.cli_bin), "inspect", str(path), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r_ins, f"inspect {label}")
        if crash:
            return crash
        if r_ins.returncode == 0:
            return TestOutcome("FAIL", f"inspect {label} unexpectedly returned 0")
        out_png = work / f"out_{label}.png"
        r_conv = run_cmd([str(ctx.cli_bin), "convert", str(path), str(out_png)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r_conv, f"convert {label}")
        if crash:
            return crash
        if r_conv.returncode == 0 or out_png.exists():
            return TestOutcome("FAIL", f"convert {label} unexpectedly succeeded or created output")
        checks += 2

    return TestOutcome(
        "PASS",
        f"All 0-byte, truncated, encrypted & corrupt headers cleanly rejected ({checks} invocations, 0 panics)",
        metrics={"checks": checks},
    )


def _test_base_adversarial_archive_xml(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-BASE-ADVERSARIAL-ARCHIVE-XML")
    sentinel_a = work.parent / "evil_escape.txt"
    sentinel_b = work / "evil_data.dat"
    for sp in (sentinel_a, sentinel_b):
        if sp.exists():
            sp.unlink()

    # Scratch ZCF X6+ with traversal in content/dataFileList.dat
    zcf_trav = work / "zcf_datalist_traversal.cdr"
    with zipfile.ZipFile(zcf_trav, "w") as zf:
        zf.writestr("mimetype", b"application/x-vnd.corel.zcf.draw.document+zip")
        zf.writestr("content/root.dat", b"RIFF\x04\x00\x00\x00CDRO")
        zf.writestr("content/dataFileList.dat", b"../evil_data.dat\n")

    adversarial_paths = [
        ("zip_slip_cdr", _f(ctx, "zip_slip.cdr")),
        ("zip_slip_coree", _f(ctx, "zip_slip_coree.coree")),
        ("zcf_datalist_traversal", zcf_trav),
        ("zip_bomb_ratio", _f(ctx, "zip_bomb_ratio.cdr")),
        ("zip_bomb_size_528m", _f(ctx, "zip_bomb_size_528m.coree")),
        ("cmpr_zlib_bomb", _f(ctx, "cmpr_zlib_bomb.cdr")),
        ("deep_nested_200", _f(ctx, "deep_nested_200.cdr")),
        ("huge_dib_65536", _f(ctx, "huge_dib_65536.cdr")),
        ("svg_billion_laughs", _f(ctx, "svg_billion_laughs.svg")),
        ("svg_deep_300", _f(ctx, "svg_deep_300.svg")),
    ]

    checks = 0
    for label, path in adversarial_paths:
        if not path.is_file():
            return TestOutcome("FAIL", f"Missing adversarial fixture {label}: {path}")
        for subcmd in ("inspect", "convert"):
            cmd = [str(ctx.cli_bin), subcmd, str(path)]
            out_png = work / f"adv_{label}.png"
            if subcmd == "convert":
                cmd.append(str(out_png))
            r = run_cmd(cmd, cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
            crash = _assert_no_panic_or_crash(r, f"{subcmd} {label}")
            if crash:
                return crash
            if r.returncode == 0:
                return TestOutcome("FAIL", f"{subcmd} {label} unexpectedly succeeded (security guard missed)")
            if subcmd == "convert" and out_png.exists():
                return TestOutcome("FAIL", f"convert {label} created output file despite adversarial input")
            checks += 1

    for sp in (sentinel_a, sentinel_b):
        if sp.exists():
            return TestOutcome("FAIL", f"Zip-Slip traversal materialized file at {sp}")

    return TestOutcome(
        "PASS",
        f"Zip-Slip, Zip-Bomb (ratio & 528 MiB), Zlib-Bomb, depth-200, 65k DIB & XML DTD/depth bombs rejected cleanly ({checks} checks)",
        metrics={"checks": checks},
    )


def _test_base_arabic_cp1256_edge(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-BASE-ARABIC-CP1256-EDGE")

    # 1. CP1256 explicit + heuristic + edge bytes (0x81, 0x8D, 0xFF)
    cp_cdr = _f(ctx, "cp1256_arabic_edge.cdr")
    if not cp_cdr.is_file():
        return TestOutcome("FAIL", f"Missing fixture: {cp_cdr}")

    r_ins = run_cmd([str(ctx.cli_bin), "inspect", str(cp_cdr), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ins, "inspect cp1256_arabic_edge.cdr")
    if crash:
        return crash
    if r_ins.returncode != 0:
        return TestOutcome("FAIL", f"inspect cp1256_arabic_edge.cdr failed (rc={r_ins.returncode}): {r_ins.stderr}")
    try:
        ins_data = _require_dict(json.loads(r_ins.stdout), "cp1256 inspect --json")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"cp1256 inspect invalid JSON: {exc}")
    obj_count = ins_data.get("object_count")
    if not isinstance(obj_count, int) or isinstance(obj_count, bool) or obj_count < 3:
        return TestOutcome("FAIL", f"cp1256_arabic_edge.cdr expected >= 3 decoded objects, got {obj_count!r}")

    cp_coree = work / "cp1256_decoded.coree"
    r_conv = run_cmd([str(ctx.cli_bin), "convert", str(cp_cdr), str(cp_coree)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_conv, "convert cp1256 -> .coree")
    if crash:
        return crash
    if r_conv.returncode != 0 or not cp_coree.is_file():
        return TestOutcome("FAIL", f"convert cp1256_arabic_edge.cdr -> .coree failed: {r_conv.stderr}")
    try:
        with zipfile.ZipFile(cp_coree, "r") as zf:
            doc_json = _require_dict(json.loads(zf.read("document.json").decode("utf-8")), "cp1256 document.json")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"cp1256 converted .coree invalid: {exc}")

    decoded_texts: list[str] = []
    objs = doc_json.get("objects")
    if isinstance(objs, dict):
        for obj in objs.values():
            if not isinstance(obj, dict):
                continue
            payload = obj.get("payload")
            if isinstance(payload, dict) and isinstance(payload.get("ArtisticText"), dict):
                runs = payload["ArtisticText"].get("runs")
                if isinstance(runs, list):
                    for run in runs:
                        if isinstance(run, dict) and isinstance(run.get("text"), str):
                            decoded_texts.append(run["text"])
    joined = "\n".join(decoded_texts)
    for expected_sub in ("تصميم عربي كلاسيكي", "مرحبا بالعالم العربي", "پ", "چ", "ے"):
        if expected_sub not in joined:
            return TestOutcome("FAIL", f"CP1256 decoded text missing {expected_sub!r}; got {joined!r}")

    cp_png = work / "cp1256.png"
    r_rnd = run_cmd([str(ctx.cli_bin), "render", str(cp_cdr), str(cp_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_rnd, "render cp1256")
    if crash:
        return crash
    if r_rnd.returncode != 0 or not cp_png.is_file():
        return TestOutcome("FAIL", f"render cp1256_arabic_edge.cdr failed: {r_rnd.stderr}")
    try:
        cp_info = parse_png_info(cp_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"cp1256 rendered PNG invalid: {exc}")
    if cp_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"cp1256_arabic_edge.cdr rendered blank PNG: {cp_info}")

    # 2. RTL Kashida + zero-width characters + empty run
    rtl_coree = _f(ctx, "rtl_kashida_zw.coree")
    if not rtl_coree.is_file():
        return TestOutcome("FAIL", f"Missing fixture: {rtl_coree}")
    rtl_png = work / "rtl.png"
    rtl_pdf = work / "rtl.pdf"
    rtl_svg = work / "rtl.svg"
    r_rp = run_cmd([str(ctx.cli_bin), "render", str(rtl_coree), str(rtl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_rp, "render rtl_kashida_zw")
    if crash:
        return crash
    if r_rp.returncode != 0 or not rtl_png.is_file():
        return TestOutcome("FAIL", f"render rtl_kashida_zw.coree failed: {r_rp.stderr}")
    try:
        rtl_info = parse_png_info(rtl_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"rtl_kashida_zw PNG invalid: {exc}")
    if rtl_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"rtl_kashida_zw.coree rendered blank PNG: {rtl_info}")

    r_rpdf = run_cmd([str(ctx.cli_bin), "convert", str(rtl_coree), str(rtl_pdf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_rpdf, "convert rtl -> pdf")
    if crash:
        return crash
    if r_rpdf.returncode != 0 or not rtl_pdf.is_file():
        return TestOutcome("FAIL", f"convert rtl_kashida_zw -> pdf failed: {r_rpdf.stderr}")
    try:
        pdf_s = parse_pdf_structure(rtl_pdf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"rtl_kashida_zw PDF invalid: {exc}")
    if not (pdf_s.get("has_catalog") and pdf_s.get("has_eof")):
        return TestOutcome("FAIL", f"rtl_kashida_zw PDF structure invalid: {pdf_s}")

    r_rsvg = run_cmd([str(ctx.cli_bin), "convert", str(rtl_coree), str(rtl_svg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_rsvg, "convert rtl -> svg")
    if crash:
        return crash
    if r_rsvg.returncode != 0 or not rtl_svg.is_file():
        return TestOutcome("FAIL", f"convert rtl_kashida_zw -> svg failed: {r_rsvg.stderr}")
    try:
        svg_s = parse_svg_structure(rtl_svg.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"rtl_kashida_zw SVG invalid: {exc}")
    if svg_s.get("counts", {}).get("svg", 0) < 1 or svg_s.get("counts", {}).get("text", 0) < 2:
        return TestOutcome("FAIL", f"rtl_kashida_zw SVG invalid: {svg_s}")

    # 3. Print Merge boundary CSVs (malformed unclosed quote, header-only 0-row, Arabic + XML meta-chars)
    tpl_path = work / "pm_template.coree"
    tpl_path.write_bytes(build_print_merge_template_coree_bytes())

    bad_csv = _f(ctx, "bad_csv_print_merge.csv")
    bad_pm_out = work / "bad_pm.coree"
    r_bad_pm = run_cmd(
        [str(ctx.cli_bin), "print-merge", str(tpl_path), str(bad_csv), str(bad_pm_out)],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_bad_pm, "print-merge bad_csv")
    if crash:
        return crash
    if r_bad_pm.returncode == 0 or bad_pm_out.exists():
        return TestOutcome("FAIL", "print-merge on unclosed-quote CSV should have failed")

    hdr_only_csv = work / "header_only.csv"
    hdr_only_csv.write_text("Name,Code,City\n", encoding="utf-8")
    hdr_only_out = work / "hdr_only_pm.coree"
    r_hdr_pm = run_cmd(
        [str(ctx.cli_bin), "print-merge", str(tpl_path), str(hdr_only_csv), str(hdr_only_out)],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_hdr_pm, "print-merge header-only CSV")
    if crash:
        return crash
    if r_hdr_pm.returncode == 0 or hdr_only_out.exists():
        return TestOutcome(
            "FAIL",
            f"print-merge on 0-row header-only CSV should have failed without creating output (rc={r_hdr_pm.returncode})",
        )

    special_csv = work / "special_arabic.csv"
    special_csv.write_text(
        "\ufeffName,Code,City\n\"شركة <كوري> & \"\"الطباعة\"\"\",SKU-AR-1,الرياض\n",
        encoding="utf-8",
    )
    special_svg_out = work / "special_pm.svg"
    r_sp_pm = run_cmd(
        [str(ctx.cli_bin), "print-merge", str(tpl_path), str(special_csv), str(special_svg_out)],
        cwd=work,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_sp_pm, "print-merge special Arabic XML-escaped CSV")
    if crash:
        return crash
    if r_sp_pm.returncode != 0 or not special_svg_out.is_file():
        return TestOutcome("FAIL", f"print-merge special Arabic CSV failed: {r_sp_pm.stderr}")
    try:
        pm_svg_bytes = special_svg_out.read_bytes()
        pm_svg_s = parse_svg_structure(pm_svg_bytes)
        pm_svg_root = ET.fromstring(pm_svg_bytes.decode("utf-8"))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"print-merge SVG output malformed XML (XML escaping failure?): {exc}")
    if pm_svg_s.get("counts", {}).get("svg", 0) < 1 or pm_svg_s.get("counts", {}).get("text", 0) < 1:
        return TestOutcome("FAIL", f"print-merge SVG invalid: {pm_svg_s}")
    merged_text_content = "\n".join(
        "".join(el.itertext())
        for el in pm_svg_root.iter()
        if el.tag.split("}", 1)[-1] == "text"
    )
    for expected_tok in ("شركة", "الرياض", "<كوري>", '"الطباعة"'):
        if expected_tok not in merged_text_content:
            return TestOutcome(
                "FAIL",
                f"print-merge SVG <text> missing merged Arabic/escaped token {expected_tok!r}; got {merged_text_content!r}",
            )

    return TestOutcome(
        "PASS",
        f"CP1256 Arabic (explicit+heuristic+edge bytes), RTL Kashida/ZW, & CSV Print Merge BVA verified (cp1256 non_white={cp_info['non_white_pixels']}, rtl non_white={rtl_info['non_white_pixels']})",
        metrics={"cp1256_non_white": cp_info["non_white_pixels"], "rtl_non_white": rtl_info["non_white_pixels"]},
    )


def _test_base_json_coree_nan_cycle(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-BASE-JSON-COREE-NAN-CYCLE")

    # 1. Degenerate geometry + circular PowerClip/Group IDs + dangling ObjectId
    nan_coree = _f(ctx, "nan_cycle_bounds.coree")
    if not nan_coree.is_file():
        return TestOutcome("FAIL", f"Missing fixture: {nan_coree}")
    nan_png = work / "nan_cycle.png"
    r_nan = run_cmd([str(ctx.cli_bin), "render", str(nan_coree), str(nan_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_nan, "render nan_cycle_bounds.coree")
    if crash:
        return crash
    if r_nan.returncode != 0 or not nan_png.is_file():
        return TestOutcome("FAIL", f"render nan_cycle_bounds.coree failed (rc={r_nan.returncode}): {r_nan.stderr}")
    try:
        nan_info = parse_png_info(nan_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"nan_cycle_bounds rendered PNG invalid: {exc}")
    if nan_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"nan_cycle_bounds failed to render normal anchor object: {nan_info}")

    r_pf = run_cmd([str(ctx.cli_bin), "preflight", str(nan_coree), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_pf, "preflight nan_cycle_bounds.coree")
    if crash:
        return crash
    if r_pf.returncode != 0:
        return TestOutcome("FAIL", f"preflight nan_cycle_bounds.coree failed: {r_pf.stderr}")
    try:
        pf_nan_json = _require_dict(json.loads(r_pf.stdout), "nan_cycle preflight JSON")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"nan_cycle preflight invalid JSON: {exc}")
    if not isinstance(pf_nan_json.get("issues"), list):
        return TestOutcome("FAIL", f"nan_cycle preflight JSON missing list 'issues': {sorted(pf_nan_json)}")

    # 2. 0 pages in .coree -> render fails cleanly without panic
    empty_pages_doc = _minimal_doc_dict("Zero Pages")
    empty_pages_doc["pages"] = []
    empty_pages_p = work / "zero_pages.coree"
    _write_coree_from_doc(empty_pages_p, "Zero Pages", empty_pages_doc)
    r_zp = run_cmd([str(ctx.cli_bin), "render", str(empty_pages_p), str(work / "zp.png")], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_zp, "render zero_pages.coree")
    if crash:
        return crash
    if r_zp.returncode == 0:
        return TestOutcome("FAIL", "render on 0-page .coree unexpectedly succeeded")

    # 3. Non-JSON NaN/1e309 token in document.json -> rejected cleanly
    bad_json_coree = work / "nan_token.coree"
    doc_raw = json.dumps(_minimal_doc_dict("NaN Token")).replace("210.0", "NaN", 1)
    manifest_raw = json.dumps({
        "format_version": 1, "app": "coree", "app_version": "0.3.1",
        "document_id": _FIXED_DOC_UUID, "title": "NaN Token",
        "created_at": _FIXED_TS, "modified_at": _FIXED_TS,
        "page_count": 1, "asset_count": 0, "has_preview": False,
    })
    with zipfile.ZipFile(bad_json_coree, "w") as zf:
        zf.writestr("manifest.json", manifest_raw)
        zf.writestr("document.json", doc_raw)
    r_nj = run_cmd([str(ctx.cli_bin), "inspect", str(bad_json_coree)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_nj, "inspect NaN JSON token")
    if crash:
        return crash
    if r_nj.returncode == 0:
        return TestOutcome("FAIL", "inspect on invalid NaN JSON token unexpectedly succeeded")

    # 4. Negative page dimensions & negative bleed handling (either clean rejection without file, or clamped valid PNG)
    neg_doc = _minimal_doc_dict("Neg Page", width_mm=-10.0, height_mm=-20.0)
    neg_doc["pages"][0]["bleed_mm"] = -5.0
    neg_p = work / "neg_page.coree"
    neg_png = work / "neg.png"
    _write_coree_from_doc(neg_p, "Neg Page", neg_doc)
    r_neg = run_cmd([str(ctx.cli_bin), "render", str(neg_p), str(neg_png), "--dpi", "72"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_neg, "render negative page dims")
    if crash:
        return crash
    if r_neg.returncode != 0:
        if neg_png.exists():
            return TestOutcome("FAIL", f"render negative page dims failed (rc={r_neg.returncode}) yet left partial file {neg_png.name}")
    else:
        if not neg_png.is_file():
            return TestOutcome("FAIL", "render negative page dims exited 0 without writing PNG")
        try:
            neg_info = parse_png_info(neg_png.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"render negative page dims produced invalid PNG: {exc}")
        if neg_info["width"] < 1 or neg_info["height"] < 1:
            return TestOutcome("FAIL", f"render negative page dims produced invalid dimensions: {neg_info}")

    # 5. Arabic UTF-8 filename with spaces
    ar_name_in = work / "ملف اختبار عربي.coree"
    ar_name_out = work / "صورة ناتجة.png"
    ar_name_in.write_bytes(build_coree_bytes("Arabic Filename"))
    r_ar = run_cmd([str(ctx.cli_bin), "render", str(ar_name_in), str(ar_name_out), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ar, "render Arabic UTF-8 path")
    if crash:
        return crash
    if r_ar.returncode != 0 or not ar_name_out.is_file():
        return TestOutcome("FAIL", f"render with Arabic UTF-8 filename failed: {r_ar.stderr}")
    try:
        ar_info = parse_png_info(ar_name_out.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Arabic UTF-8 filename PNG invalid: {exc}")
    if ar_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"Arabic UTF-8 filename rendered blank PNG: {ar_info}")

    return TestOutcome(
        "PASS",
        f"Degenerate bounds, PowerClip/Group cycle guard, 0-page/NaN JSON rejection & Arabic UTF-8 filename paths verified (nan non_white={nan_info['non_white_pixels']})",
        metrics={"nan_non_white": nan_info["non_white_pixels"]},
    )


# ---------------------------------------------------------------------------
# WAVE 1..5 (smoke=False) TESTS — Structural/CLI Prerequisites First, Then Cargo
# ---------------------------------------------------------------------------

def _test_w1_toolcore_interaction(ctx: RunContext) -> TestOutcome:
    tools_toml = ctx.repo_root / "crates" / "coree-tools" / "Cargo.toml"
    if not tools_toml.is_file():
        return TestOutcome("FAIL", "W1 MND-02: crates/coree-tools/Cargo.toml not yet created (ToolCore extraction pending)")
    try:
        parsed = tomllib.loads(tools_toml.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as exc:
        return TestOutcome("FAIL", f"W1 MND-02: invalid crates/coree-tools/Cargo.toml: {exc}")
    deps = set(parsed.get("dependencies", {}).keys()) | set(parsed.get("dev-dependencies", {}).keys()) | set(parsed.get("build-dependencies", {}).keys())
    if any(d in deps for d in ("egui", "eframe", "egui_dock")):
        return TestOutcome("FAIL", f"W1 MND-02: crates/coree-tools must not depend on egui/eframe: {sorted(deps)}")

    app_toml = ctx.repo_root / "crates" / "coree-app" / "Cargo.toml"
    try:
        app_parsed = tomllib.loads(app_toml.read_text(encoding="utf-8")) if app_toml.is_file() else {}
    except (OSError, tomllib.TOMLDecodeError) as exc:
        return TestOutcome("FAIL", f"W1 MND-02: invalid crates/coree-app/Cargo.toml: {exc}")
    app_deps = set(app_parsed.get("dependencies", {}).keys())
    if "coree-tools" not in app_deps:
        return TestOutcome("FAIL", f"W1 MND-02: crates/coree-app/Cargo.toml does not depend on coree-tools: {sorted(app_deps)}")

    tools_src = ""
    tools_src_dir = ctx.repo_root / "crates" / "coree-tools" / "src"
    if tools_src_dir.is_dir():
        tools_src = "\n".join(
            _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
            for p in sorted(tools_src_dir.rglob("*.rs"))
        )
    for sym in (
        "ToolCore",
        "ToolReadContext",
        "TransientOverlay",
        "ToolOutput",
        "preview_paths",
        "preview_handles",
        "active_guides",
        "dirty_world_bbox_mm",
        "cursor_hint",
    ):
        if sym not in tools_src:
            return TestOutcome("FAIL", f"W1 MND-02: crates/coree-tools/src production code missing {sym}")

    canvas_prod = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs"))
    ctools_prod = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/canvas_tools.rs"))
    doc_prod = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/document.rs"))
    sel_prod = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/selection.rs"))
    pal_prod = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/palette_bar.rs"))
    if not re.search(r"\b(?:coree_tools|ToolCore|TransientOverlay)\b", canvas_prod + "\n" + ctools_prod):
        return TestOutcome("FAIL", "W1 MND-02: ui/canvas.rs / state/canvas_tools.rs does not wire coree_tools::ToolCore")
    if "w1c_pending_objects_mut" in (canvas_prod + "\n" + ctools_prod + "\n" + doc_prod):
        return TestOutcome("FAIL", "W1 MND-02: temporary w1c_pending_objects_mut bridge still present")
    if re.search(r"\b(?:objects\s*\.\s*get_mut|object_mut|[A-Za-z0-9_]*objects_mut)\s*\(", canvas_prod + "\n" + ctools_prod):
        return TestOutcome("FAIL", "W1 MND-02: ui/canvas.rs or state/canvas_tools.rs still directly mutates Document objects")
    if "stamp_duplicate_during_active_drag" not in canvas_prod or "DraggingPivot" not in canvas_prod:
        return TestOutcome("FAIL", "W1 MND-02: CorelDRAW Spacebar drag-stamp or DraggingPivot muscle memory missing in canvas.rs")
    if "RotateAndSkew" not in sel_prod or "ScaleAndMove" not in sel_prod:
        return TestOutcome("FAIL", "W1 MND-02: SelectionState::click_object ScaleAndMove<->RotateAndSkew toggle missing")
    if "apply_fill_color" not in pal_prod or "apply_outline_color" not in pal_prod:
        return TestOutcome("FAIL", "W1 MND-02: Swatch bar L-click Fill / R-click Outline wiring missing in palette_bar.rs")

    fail_tools = _run_cargo_tests(ctx, ["-p", "coree-tools"], "W1 MND-02 coree-tools", min_passed=5, timeout=120.0)
    if fail_tools:
        return fail_tools
    fail_app = _run_cargo_tests(
        ctx,
        ["-p", "coree-app", "--", "tool"],
        "W1 MND-02 coree-app tool",
        min_passed=7,
        timeout=120.0,
        require_any_rx=r"tool_?core|transient_overlay|pointer",
    )
    if fail_app:
        return fail_app
    fail_audit = _run_cargo_tests(
        ctx,
        ["-p", "coree-app", "--test", "tools_62_audit"],
        "W1 MND-02 tools_62_audit",
        min_passed=17,
        timeout=120.0,
        require_any_rx=r"tool_?core|transient_overlay|pointer",
    )
    if fail_audit:
        return fail_audit
    return TestOutcome("PASS", "ToolCore headless pointer/snap/pivot/Spacebar-stamp/Ctrl+D/Swatch/InteractiveFill/Eyedropper/Connector tests verified")


def _test_w1_command_delta_bva(ctx: RunContext) -> TestOutcome:
    prod_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/production.rs"))
    bfq = _extract_brace_block(prod_code, r"\bfn\s+build_find_query\b")
    if not bfq or (
        re.search(r"\bfill_color\s*:\s*(?:Option::)?None\b", bfq)
        or re.search(r"\boutline_color\s*:\s*(?:Option::)?None\b", bfq)
        or re.search(r"\boutline_width_min_mm\s*:\s*(?:Option::)?None\b", bfq)
        or re.search(r"\blayer_name\s*:\s*(?:Option::)?None\b", bfq)
    ):
        return TestOutcome("FAIL", "W1 REQ-01: build_find_query in production.rs still hardcodes None filter fields")

    cmd_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/command.rs"))
    op_body = _extract_brace_block(cmd_code, r"\bpub\s+enum\s+CommandOp\s*\{")
    if not op_body:
        return TestOutcome("FAIL", "W1 REQ-01: pub enum CommandOp missing in command.rs")
    for op_name in ("ReorderPages", "FitPowerClip", "FindAndReplace", "DuplicateObjects"):
        if not re.search(rf"\b{op_name}\b", op_body):
            return TestOutcome("FAIL", f"W1 REQ-01: CommandOp::{op_name} missing in command.rs")
    delta_block = _extract_brace_block(cmd_code, r"\b(?:pub\s+)?struct\s+DocumentDelta\b")
    if not delta_block:
        return TestOutcome("FAIL", "W1 REQ-01: struct DocumentDelta missing in command.rs")
    for fld in ("color_styles", "object_styles", "symbols", "custom_icc_profile", "perspective_grid", "assets"):
        if not re.search(rf"\b{fld}\s*:", delta_block):
            return TestOutcome("FAIL", f"W1 REQ-01: DocumentDelta missing field {fld}")

    doc_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/document.rs"))
    if "w1c_pending_objects_mut" in doc_code:
        return TestOutcome("FAIL", "W1 REQ-01: Document::w1c_pending_objects_mut encapsulation escape still present")

    fail_cmd = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "command"], "W1 REQ-01 coree-core command", min_passed=9, timeout=120.0)
    if fail_cmd:
        return fail_cmd
    fail_fp = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "fit_powerclip"], "W1 REQ-01 FitPowerClip", min_passed=3, timeout=120.0)
    if fail_fp:
        return fail_fp
    fail_app = _run_cargo_tests(ctx, ["-p", "coree-app", "--", "command"], "W1 REQ-01 coree-app command", min_passed=3, timeout=120.0)
    if fail_app:
        return fail_app
    return TestOutcome("PASS", "CommandOp/DocumentDelta BVA, ReorderPages OOB, FitPowerClip cycle & FindAndReplace verified")


def _test_w1_ipc_uds_clipboard(ctx: RunContext) -> TestOutcome:
    ipc_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ipc.rs"))
    if "std::net" in ipc_code or "TcpListener" in ipc_code or "TcpStream" in ipc_code:
        return TestOutcome("FAIL", "W1 REQ-10: ipc.rs still uses std::net TcpListener/TcpStream instead of UDS/NamedPipe")
    if "UnixListener" not in ipc_code or "UnixStream" not in ipc_code:
        return TestOutcome("FAIL", "W1 REQ-10: ipc.rs missing UnixListener/UnixStream UDS IPC")
    if not re.search(r"\b0o?600\b", ipc_code):
        return TestOutcome("FAIL", "W1 REQ-10: ipc.rs missing 0o600/0600 UDS permission hardening")

    clip_candidates = [
        ctx.repo_root / "crates" / "coree-io" / "src" / "clipboard.rs",
        ctx.repo_root / "crates" / "coree-io" / "src" / "clipboard_codec.rs",
    ]
    if not any(p.is_file() for p in clip_candidates):
        io_lib = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-io/src/lib.rs"))
        if "clipboard" not in io_lib:
            return TestOutcome("FAIL", "W1 REQ-09: Live Vector Clipboard module missing in crates/coree-io/src/")

    fail_ipc = _run_cargo_tests(
        ctx,
        ["-p", "coree-app", "--", "ipc"],
        "W1 REQ-10 ipc",
        min_passed=3,
        timeout=120.0,
        require_any_rx=r"uds|named_pipe|json_rpc|handshake|socket",
    )
    if fail_ipc:
        return fail_ipc
    fail_clip = _run_cargo_tests(ctx, ["-p", "coree-io", "--", "clipboard"], "W1 REQ-09 clipboard", min_passed=1, timeout=120.0)
    if fail_clip:
        return fail_clip
    return TestOutcome("PASS", "UDS 0600 JSON-RPC IPC boundary & Live Vector Clipboard (SVG/PDF/EMF) verified")


def _test_w1_sqlite_quarantine_wal(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W1-SQLITE-QUARANTINE-WAL")
    corrupt_src = _f(ctx, "corrupt_sqlite_header.db")
    if not corrupt_src.is_file():
        return TestOutcome("FAIL", f"Missing Tier 2 fixture: {corrupt_src}")
    staged = work / "staged_corrupt.db"
    shutil.copy2(corrupt_src, staged)
    if staged.read_bytes()[:16] == b"SQLite format 3\x00":
        return TestOutcome("FAIL", "Tier 2 fixture corrupt_sqlite_header.db unexpectedly has valid SQLite3 header")
    r_db = run_cmd([str(ctx.cli_bin), "inspect", str(staged)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_db, "inspect staged_corrupt.db")
    if crash:
        return crash
    if r_db.returncode == 0:
        return TestOutcome("FAIL", "inspect staged_corrupt.db unexpectedly succeeded (rc=0)")

    db_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-storage/src/db.rs"))
    auto_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-storage/src/autosave.rs"))
    if not re.search(r"\bquarantine\b", db_code + "\n" + auto_code, re.I):
        return TestOutcome("FAIL", "W1 IMP-07: coree-storage db.rs/autosave.rs production code missing corrupt SQLite quarantine")
    if not re.search(r"\bdifferential\b", auto_code, re.I):
        return TestOutcome("FAIL", "W1 IMP-07: autosave.rs production code missing async differential autosave")

    fail_q = _run_cargo_tests(
        ctx,
        ["-p", "coree-storage", "--", "quarantine"],
        "W1 IMP-07 SQLite quarantine",
        min_passed=1,
        timeout=120.0,
        exclude_prefix="fonts::",
        require_any_rx=r"(?:db|sqlite|wal|autosave|corrupt).*quarantine|quarantine.*(?:db|sqlite|wal|autosave|corrupt)",
    )
    if fail_q:
        return fail_q
    fail_d = _run_cargo_tests(ctx, ["-p", "coree-storage", "--", "differential"], "W1 IMP-07 differential", min_passed=1, timeout=120.0)
    if fail_d:
        return fail_d
    return TestOutcome("PASS", "Corrupt SQLite DB quarantine, SHA-256 snapshot & WAL recovery boundary verified")


def _test_w2_cdrcrv_geometry_bva(ctx: RunContext) -> TestOutcome:
    geom_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/geometry.rs"))
    erase_blk = _extract_brace_block(geom_code, r"\bfn\s+erase_circle\b")
    if not erase_blk or re.search(r"\b96(?:usize)?\b", erase_blk):
        return TestOutcome("FAIL", "W2 REM-01: erase_circle still uses 96-segment polygon approximation")
    if not re.search(r"\bfn\s+enforce_continuity\b", geom_code):
        return TestOutcome("FAIL", "W2 REM-10: PathNode::enforce_continuity missing in geometry.rs")

    sf_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/smart_fill.rs"))
    if not re.search(r"\bdcel\b|\bhalf_edge\b|\bHalfEdge\b", sf_code, re.I):
        return TestOutcome("FAIL", "W2 IMP-05: smart_fill.rs missing planar DCEL topology")

    for pkg, filt, lbl, min_p in (
        ("coree-core", "geometry", "W2 REM-01 geometry", 8),
        ("coree-core", "boolean", "W2 REM-03 boolean", 8),
        ("coree-core", "continuity", "W2 REM-10 continuity", 1),
        ("coree-render", "dcel", "W2 IMP-05 dcel", 1),
    ):
        fail = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail:
            return fail
    return TestOutcome("PASS", "Analytical CDRCRV geometry/boolean/continuity & DCEL SmartFill BVA verified")


def _test_w2_drawing_modes_master(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W2-DRAWING-MODES-MASTER")
    doc_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/document.rs"))
    cmd_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/command.rs"))
    geom_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/geometry.rs"))
    eff_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/effects.rs"))
    dim_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/dimension.rs"))
    table_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/table.rs"))
    tools_dir = ctx.repo_root / "crates" / "coree-tools" / "src"
    tools_src = "\n".join(
        _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
        for p in sorted(tools_dir.rglob("*.rs"))
    ) if tools_dir.is_dir() else ""

    node_payload_blk = _extract_brace_block(doc_rs, r"\bpub\s+enum\s+NodePayload\s*\{") or ""
    for variant in ("BSpline", "ArtisticMedia", "Clone"):
        if not re.search(rf"\b{variant}\b", node_payload_blk):
            return TestOutcome("FAIL", f"W2 ADD-02: NodePayload::{variant} missing inside enum NodePayload in document.rs")
    for pc_sym in ("FitContents", "CenterContents", "StretchToFill"):
        if not re.search(rf"\b{pc_sym}\b", doc_rs + "\n" + cmd_rs):
            return TestOutcome("FAIL", f"W2 ADD-03: PowerClip mode {pc_sym} missing in document.rs/command.rs")
    for m_sym in ("AllPages", "OddPages", "EvenPages"):
        if not re.search(rf"\b{m_sym}\b", doc_rs):
            return TestOutcome("FAIL", f"W2 FEAT-09: MasterLayerScope::{m_sym} missing in document.rs")
    if "VectorTable" not in table_rs or "MAX_DIM" not in table_rs:
        return TestOutcome("FAIL", "W2 FEAT-30: VectorTable / MAX_DIM missing in coree-core/src/table.rs")
    if not re.search(r"\b(?:smart_drawing|live_sketch|SmartDrawing|LiveSketch)\b", geom_rs):
        return TestOutcome("FAIL", "W2 FEAT-19: SmartDrawing / LiveSketch missing in geometry.rs")
    if not re.search(r"\b(?:SmartDrawing|LiveSketch|smart_drawing|live_sketch)\b", tools_src):
        return TestOutcome("FAIL", "W2 FEAT-19: SmartDrawing / LiveSketch interactive mode missing in crates/coree-tools/src/")
    if not re.search(r"\bSymmetry\b", doc_rs + "\n" + eff_rs) or not re.search(r"\bSymmetry\b", tools_src):
        return TestOutcome("FAIL", "W2 FEAT-26: Symmetry drawing mode missing in coree-core or coree-tools")
    if not re.search(r"\bDimensionObject\b", dim_rs):
        return TestOutcome("FAIL", "W2 FEAT-28: DimensionObject missing in dimension.rs")

    # Degenerate BSpline / ArtisticMedia / Clone / Dimension / PowerClip / Table / Symmetry .coree boundary tests via CLI
    bva_doc = _minimal_doc_dict("W2 BVA")
    black_cmyk = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}
    mag_cmyk = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    bva_doc["objects"]["10"] = {
        "id": 10,
        "name": "Bad BSpline Deg0",
        "payload": {
            "BSpline": {
                "control_points": [{"x": 10.0, "y": 10.0}],
                "degree": 0,
                "knots": [0.0],
                "closed": False,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.5, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["11"] = {
        "id": 11,
        "name": "Dangling Clone",
        "payload": {"Clone": {"master_id": 888888, "link_fill": True, "link_outline": True, "link_shape": True}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 10.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.5, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["12"] = {
        "id": 12,
        "name": "Zero Width ArtisticMedia",
        "payload": {
            "ArtisticMedia": {
                "spine": {"subpaths": [{"nodes": [{"pos": {"x": 10.0, "y": 80.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"}], "closed": False}]},
                "style": {"Calligraphic": {"width_mm": 0.0, "angle_deg": 45.0}},
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": {"Solid": black_cmyk},
        "outline": _default_outline(black_cmyk, 0.0, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["13"] = {
        "id": 13,
        "name": "Zero Length Dimension",
        "payload": {
            "Dimension": {
                "kind": "Horizontal",
                "p1": {"x": 50.0, "y": 50.0},
                "p2": {"x": 50.0, "y": 50.0},
                "p3": None,
                "offset_mm": 0.0,
                "precision": 2,
                "prefix": "",
                "suffix": " mm",
                "label_override": None,
                "text_style": _default_text_style(),
                "unit_override": None,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.3, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["14"] = {
        "id": 14,
        "name": "Empty PowerClip",
        "payload": {
            "PowerClip": {
                "container": {"subpaths": [{"nodes": [
                    {"pos": {"x": 120.0, "y": 20.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                    {"pos": {"x": 160.0, "y": 20.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                    {"pos": {"x": 160.0, "y": 60.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                ], "closed": True}]},
                "container_parametric": None,
                "lock_contents": True,
                "contents": [],
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.3, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["15"] = {
        "id": 15,
        "name": "Zero Dim Table",
        "payload": {"Table": {"rows": 0, "cols": 0, "col_widths_mm": [], "row_heights_mm": [], "cells": [], "border_style": _default_outline(black_cmyk, 0.3, False)}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 120.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.3, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["16"] = {
        "id": 16,
        "name": "OverMaxDim Table 201x201",
        "payload": {"Table": {"rows": 201, "cols": 201, "col_widths_mm": [], "row_heights_mm": [], "cells": [], "border_style": _default_outline(black_cmyk, 0.3, False)}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 150.0]},
        "fill": "None",
        "outline": _default_outline(black_cmyk, 0.3, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["objects"]["17"] = {
        "id": 17,
        "name": "Zero Axis Symmetry",
        "payload": dict(bva_doc["objects"]["3"]["payload"]),
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 200.0]},
        "fill": {"Solid": mag_cmyk},
        "outline": _default_outline(black_cmyk, 0.2, False),
        "lens": None,
        "effects": [{"Symmetry": {"axes": 0, "mirror": True, "origin": {"x": 50.0, "y": 220.0}}}],
        "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["pages"][0]["layers"][0]["objects"].extend([10, 11, 12, 13, 14, 15, 16, 17])
    bva_doc["next_id"] = 18
    bva_in = work / "w2_bva.coree"
    bva_png = work / "w2_bva.png"
    _write_coree_from_doc(bva_in, "W2 BVA", bva_doc)
    r = run_cmd([str(ctx.cli_bin), "render", str(bva_in), str(bva_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r, "render W2 BVA BSpline/ArtisticMedia/Clone")
    if crash:
        return crash
    if r.returncode != 0 or not bva_png.is_file():
        return TestOutcome("FAIL", f"W2 BVA render failed (rc={r.returncode}): {r.stderr}")
    try:
        bva_info = parse_png_info(bva_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 BVA rendered PNG invalid: {exc}")
    if bva_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"W2 BVA rendered blank PNG despite Anchor Rect: {bva_info}")

    # FEAT-09: MasterLayerScope OddPages / EvenPages multipage render oracle
    master_doc = _minimal_doc_dict("W2 Master Odd/Even")
    master_doc["objects"]["21"] = dict(master_doc["objects"]["3"], id=21, name="OddMasterRect", transform={"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]})
    master_doc["objects"]["23"] = dict(master_doc["objects"]["3"], id=23, name="EvenMasterRect", fill={"Solid": mag_cmyk}, transform={"m": [1.0, 0.0, 0.0, 1.0, 100.0, 150.0]})
    master_doc["pages"][0]["layers"] = [
        {"id": 20, "name": "OddMaster", "visible": True, "locked": False, "printable": True, "master_scope": "OddPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [21]},
        {"id": 22, "name": "EvenMaster", "visible": True, "locked": False, "printable": True, "master_scope": "EvenPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [23]},
        {"id": 24, "name": "AllMasterEmpty", "visible": True, "locked": False, "printable": True, "master_scope": "AllPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": []},
    ]
    for p_idx, pid in ((2, 101), (3, 103)):
        master_doc["pages"].append({
            "id": pid, "name": f"Page {p_idx}", "width_mm": 210.0, "height_mm": 297.0, "bleed_mm": 3.0, "facing_pages": False,
            "layers": [{"id": pid + 1, "name": f"Layer {p_idx}", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": []}],
        })
    master_doc["next_id"] = 25
    master_in = work / "w2_master.coree"
    _write_coree_from_doc(master_in, "W2 Master", master_doc)
    master_pngs: list[Path] = []
    for pg in (0, 1, 2):
        mp = work / f"w2_master_p{pg}.png"
        rm = run_cmd([str(ctx.cli_bin), "render", str(master_in), str(mp), "--page", str(pg), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(rm, f"W2 master page {pg}")
        if crash:
            return crash
        if rm.returncode != 0 or not mp.is_file():
            return TestOutcome("FAIL", f"W2 FEAT-09: MasterLayer render page {pg} failed: {rm.stderr}")
        try:
            mp_info = parse_png_info(mp.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W2 FEAT-09: MasterLayer page {pg} PNG invalid: {exc}")
        if mp_info["non_white_pixels"] <= 20:
            return TestOutcome("FAIL", f"W2 FEAT-09: MasterLayer page {pg} rendered blank")
        master_pngs.append(mp)
    try:
        m_diff_01 = compare_png_pixels(master_pngs[0], master_pngs[1])
        m_diff_02 = compare_png_pixels(master_pngs[0], master_pngs[2])
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 FEAT-09: MasterLayer PNG comparison failed: {exc}")
    if m_diff_01["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", "W2 FEAT-09: OddPages vs EvenPages master layers produced identical Page 1 & Page 2 renders")
    if m_diff_02["mean_abs_diff"] >= 0.2:
        return TestOutcome("FAIL", "W2 FEAT-09: OddPages master layer differed between Page 1 and Page 3")

    # FEAT-30: Valid 2x2 merged VectorTable render
    tbl_doc = _minimal_doc_dict("W2 Table")
    t_style = _default_text_style("Noto Sans", 11.0)
    tbl_doc["objects"] = {
        "3": {
            "id": 3,
            "name": "Merged Table",
            "payload": {
                "Table": {
                    "rows": 2,
                    "cols": 2,
                    "col_widths_mm": [30.0, 30.0],
                    "row_heights_mm": [15.0, 15.0],
                    "cells": [
                        {"row": 0, "col": 0, "row_span": 1, "col_span": 2, "text": "Header", "text_style": t_style, "fill": {"Solid": mag_cmyk}, "padding_mm": 2.0, "merged_into": None},
                        {"row": 0, "col": 1, "row_span": 1, "col_span": 1, "text": "", "text_style": t_style, "fill": "None", "padding_mm": 2.0, "merged_into": [0, 0]},
                        {"row": 1, "col": 0, "row_span": 1, "col_span": 1, "text": "A", "text_style": t_style, "fill": "None", "padding_mm": 2.0, "merged_into": None},
                        {"row": 1, "col": 1, "row_span": 1, "col_span": 1, "text": "B", "text_style": t_style, "fill": "None", "padding_mm": 2.0, "merged_into": None},
                    ],
                    "border_style": _default_outline(black_cmyk, 0.4, False),
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 30.0]},
            "fill": "None",
            "outline": _default_outline(black_cmyk, 0.4, False),
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
    }
    tbl_in = work / "w2_table.coree"
    tbl_png = work / "w2_table.png"
    _write_coree_from_doc(tbl_in, "W2 Table", tbl_doc)
    r_tbl = run_cmd([str(ctx.cli_bin), "render", str(tbl_in), str(tbl_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_tbl, "W2 VectorTable render")
    if crash:
        return crash
    if r_tbl.returncode != 0 or not tbl_png.is_file():
        return TestOutcome("FAIL", f"W2 FEAT-30: merged VectorTable render failed: {r_tbl.stderr}")
    try:
        tbl_info = parse_png_info(tbl_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 FEAT-30: merged VectorTable PNG invalid: {exc}")
    if tbl_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", f"W2 FEAT-30: merged VectorTable rendered blank: {tbl_info}")

    # FEAT-26: 4-axis Symmetry visual delta vs non-symmetry control
    sym_ctrl = _minimal_doc_dict("W2 Symmetry Ctrl")
    sym_ctrl["objects"]["3"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 120.0, 80.0]}
    sym_4x = json.loads(json.dumps(sym_ctrl))
    sym_4x["objects"]["3"]["effects"] = [{"Symmetry": {"axes": 4, "mirror": True, "origin": {"x": 100.0, "y": 100.0}}}]
    sc_in, sc_png = work / "sym_ctrl.coree", work / "sym_ctrl.png"
    s4_in, s4_png = work / "sym_4x.coree", work / "sym_4x.png"
    _write_coree_from_doc(sc_in, "Sym Ctrl", sym_ctrl)
    _write_coree_from_doc(s4_in, "Sym 4x", sym_4x)
    rsc = run_cmd([str(ctx.cli_bin), "render", str(sc_in), str(sc_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(rsc, "W2 render Symmetry control")
    if crash:
        return crash
    rs4 = run_cmd([str(ctx.cli_bin), "render", str(s4_in), str(s4_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(rs4, "W2 render 4-axis Symmetry")
    if crash:
        return crash
    if rsc.returncode != 0 or rs4.returncode != 0 or not sc_png.is_file() or not s4_png.is_file():
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry render failed: {rs4.stderr or rsc.stderr}")
    try:
        sym_delta = compare_png_pixels(sc_png, s4_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry PNG invalid: {exc}")
    if sym_delta["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", "W2 FEAT-26: 4-axis Symmetry produced no visual delta over single-object control")

    for pkg, filt, lbl, min_p in (
        ("coree-core", "artistic_media", "W2 ADD-02 ArtisticMedia", 3),
        ("coree-core", "live_clone", "W2 ADD-02 live Clone", 1),
        ("coree-core", "fit_powerclip", "W2 ADD-03 FitPowerClip", 3),
        ("coree-core", "homography", "W2 ADD-04 Perspective homography", 1),
        ("coree-core", "smart_drawing", "W2 FEAT-19 SmartDrawing", 3),
        ("coree-core", "master", "W2 FEAT-09 MasterLayers", 2),
        ("coree-tools", "symmetry", "W2 FEAT-26 ToolCore Symmetry", 1),
        ("coree-tools", "table", "W2 FEAT-30 ToolCore Table", 1),
        ("coree-app", "edit_symbol", "W2 ADD-03 Symbol isolation", 1),
    ):
        fail = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail:
            return fail
    return TestOutcome("PASS", "W2 BSpline/ArtisticMedia/Clone/MasterLayer/Symmetry/Perspective/Table/Dimension/PowerClip BVA verified")


def _test_w2_cdr_codec_boundary(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W2-CDR-CODEC-BOUNDARY")
    zcf_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-cdr/src/zcf.rs"))
    if "quick_xml" not in zcf_code and "roxmltree" not in zcf_code:
        return TestOutcome("FAIL", "W2 REM-06: zcf.rs missing quick_xml/roxmltree XML parser")
    if "[0.0, 0.0, 0.0, 1.0]" in zcf_code:
        return TestOutcome("FAIL", "W2 REM-06: zcf.rs still contains hardcoded [0,0,0,1] Spot fallback")

    # 1. Stage adversarial ZCF X6+ with OOB 16B pointer + XML <!ENTITY> in docPalette.xml
    adv_zcf = work / "adv_oob_entity.cdr"
    oob_loda_ptr = struct.pack("<IIII", 99, 1024, 999999, 0)  # file_index=99 OOB, offset=999999 OOB
    root_inner = b"CDRO" + _riff_chunk(b"vrsn", struct.pack("<II", 0xFFFFFFFF, 2) + struct.pack("<H", 1600) + b"\x00" * 6) + _riff_list(
        b"doc ",
        [_riff_list(b"page", [_riff_list(b"layr", [_riff_list(b"obj ", [_riff_chunk(b"loda", oob_loda_ptr)])])])],
    )
    with zipfile.ZipFile(adv_zcf, "w") as zf:
        zf.writestr("mimetype", b"application/x-vnd.corel.zcf.draw.document+zip")
        zf.writestr("content/root.dat", b"RIFF" + struct.pack("<I", len(root_inner)) + root_inner)
        zf.writestr("content/data/0000.dat", b"\x00" * 16)
        zf.writestr("content/dataFileList.dat", b"data/0000.dat\n")
        zf.writestr(
            "color/docPalette.xml",
            b'<?xml version="1.0"?><!DOCTYPE p [<!ENTITY a "AAAA">]><palette><colors><color cs="CMYK" name="&a;" tints="0,0,0,100"/></colors></palette>',
        )
    r_zcf = run_cmd([str(ctx.cli_bin), "inspect", str(adv_zcf), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_zcf, "inspect adversarial ZCF")
    if crash:
        return crash
    if "AAAA" in r_zcf.stdout or "AAAA" in r_zcf.stderr:
        return TestOutcome("FAIL", "W2 REM-06: ZCF XML parser expanded <!ENTITY a 'AAAA'>")
    if r_zcf.returncode == 0:
        return TestOutcome("FAIL", f"W2 MND-03: inspect adv_oob_entity.cdr (OOB 16B pointer + XML <!DOCTYPE>) unexpectedly returned rc=0: {r_zcf.stdout[:200]}")
    adv_out = work / "adv_oob_out.coree"
    r_zcf_conv = run_cmd([str(ctx.cli_bin), "convert", str(adv_zcf), str(adv_out)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_zcf_conv, "convert adversarial ZCF")
    if crash:
        return crash
    if r_zcf_conv.returncode == 0 or adv_out.exists():
        return TestOutcome("FAIL", "W2 MND-03: convert adv_oob_entity.cdr unexpectedly succeeded")

    # 2. Cyclic styles/document.cdss inheritance guard (cycA -> cycB -> cycA)
    cyc_zcf = work / "cyclic_cdss.cdr"
    with zipfile.ZipFile(cyc_zcf, "w") as zf:
        zf.writestr("mimetype", b"application/x-vnd.corel.zcf.draw.document+zip")
        zf.writestr("content/root.dat", b"RIFF\x04\x00\x00\x00CDRO")
        zf.writestr(
            "styles/document.cdss",
            json.dumps({
                "version": 2300,
                "style": {
                    "cycA": {"parent": "cycB", "outline": {"width": "5000"}},
                    "cycB": {"parent": "cycA"},
                },
            }),
        )
    r_cyc = run_cmd([str(ctx.cli_bin), "inspect", str(cyc_zcf), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cyc, "inspect cyclic cdss ZCF")
    if crash:
        return crash

    # 3. EXT-F03: Odd-padded XDOC/XOBJ/XLAY/XPAG UnknownChunkMap byte-for-byte passthrough
    odd_xdoc = b"ODD_XDOC_7B"
    odd_xobj = b"ODD_XOBJ_9B"
    odd_xlay = b"ODD_XLAY_9B"
    odd_xpag = b"ODD_XPAG_9B"
    odd_cdr = work / "odd_unknown.cdr"
    odd_rt = work / "odd_unknown_rt.cdr"
    odd_cdr.write_bytes(build_cdr_riff_with_unknown_xdoc(odd_xdoc, odd_xobj, odd_xlay, odd_xpag))
    r_odd = run_cmd([str(ctx.cli_bin), "convert", str(odd_cdr), str(odd_rt), "--cdr-version", "riff"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_odd, "convert odd-padded XDOC/XOBJ")
    if crash:
        return crash
    if r_odd.returncode != 0 or not odd_rt.is_file():
        return TestOutcome("FAIL", f"W2 EXT-F03: odd-padded UnknownChunkMap round-trip failed: {r_odd.stderr}")
    try:
        odd_info = dissect_cdr_riff(odd_rt.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 EXT-F03: round-tripped odd RIFF malformed: {exc}")
    pbf = odd_info.get("payloads_by_fourcc", {})
    if (
        odd_xdoc not in pbf.get("XDOC", [])
        or odd_xobj not in pbf.get("XOBJ", [])
        or odd_xlay not in pbf.get("XLAY", [])
        or odd_xpag not in pbf.get("XPAG", [])
    ):
        return TestOutcome("FAIL", f"W2 EXT-F03: odd-padded XDOC/XOBJ/XLAY/XPAG dropped/corrupted across round-trip: {pbf}")

    # 4. FEAT-82 (CMX RIFX/rlin & CPT PackBits/i32::MIN) & FEAT-11 (CDT valid/corrupt)
    valid_cmx = work / "valid_probe.cmx"
    valid_cmx.write_bytes(build_cmx_bytes())
    r_cmx_ok = run_cmd([str(ctx.cli_bin), "inspect", str(valid_cmx), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cmx_ok, "inspect valid CMX")
    if crash:
        return crash
    if r_cmx_ok.returncode != 0:
        return TestOutcome("FAIL", f"W2 FEAT-82: inspect valid CMX failed: {r_cmx_ok.stderr}")

    bad_rifx_cmx = work / "bad_rifx_rlin.cmx"
    bad_rifx_cmx.write_bytes(b"RIFX\x00\x00\x00\x20CMX1rlin\x00\x00\x00\xff\xff\xff\xff")
    r_cmx_bad = run_cmd([str(ctx.cli_bin), "inspect", str(bad_rifx_cmx)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cmx_bad, "inspect malformed RIFX CMX")
    if crash:
        return crash
    if r_cmx_bad.returncode == 0:
        return TestOutcome("FAIL", "W2 FEAT-82: malformed RIFX CMX unexpectedly returned rc=0")

    valid_cpt = work / "valid_probe.cpt"
    valid_cpt.write_bytes(build_cpt_bytes())
    r_cpt_ok = run_cmd([str(ctx.cli_bin), "inspect", str(valid_cpt), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cpt_ok, "inspect valid CPT")
    if crash:
        return crash
    if r_cpt_ok.returncode != 0:
        return TestOutcome("FAIL", f"W2 FEAT-82: inspect valid CPT failed: {r_cpt_ok.stderr}")

    bad_cpt = work / "bad_i32min_bomb.cpt"
    bad_cpt.write_bytes(b"CPT9FILE" + struct.pack("<IiII", 16, -2147483648, 8, 1) + b"\x00" * 16)
    r_cpt_bad = run_cmd([str(ctx.cli_bin), "inspect", str(bad_cpt)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cpt_bad, "inspect i32::MIN CPT")
    if crash:
        return crash
    if r_cpt_bad.returncode == 0:
        return TestOutcome("FAIL", "W2 FEAT-82: CPT i32::MIN height bomb unexpectedly returned rc=0")

    valid_cdt = work / "valid_probe.cdt"
    valid_cdt.write_bytes(build_cdt_bytes())
    r_cdt_ok = run_cmd([str(ctx.cli_bin), "inspect", str(valid_cdt), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cdt_ok, "inspect valid CDT")
    if crash:
        return crash
    if r_cdt_ok.returncode != 0:
        return TestOutcome("FAIL", f"W2 FEAT-11: inspect valid CDT failed: {r_cdt_ok.stderr}")

    bad_cdt = work / "corrupt_template.cdt"
    bad_cdt.write_bytes(b"PK\x03\x04CORRUPT_CDT_HEADER")
    r_cdt_bad = run_cmd([str(ctx.cli_bin), "inspect", str(bad_cdt)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cdt_bad, "inspect corrupt CDT")
    if crash:
        return crash
    if r_cdt_bad.returncode == 0:
        return TestOutcome("FAIL", "W2 FEAT-11: corrupt CDT unexpectedly returned rc=0")

    # 5. Authentic ZCF CDRO + 16B->20B loda indirection check (generate-sample & convert --cdr-version x6)
    sample_x6 = work / "probe_x6.cdr"
    r_gen = run_cmd([str(ctx.cli_bin), "generate-sample", str(sample_x6), "--format", "cdr-x6"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_gen, "generate-sample cdr-x6")
    if crash:
        return crash
    if r_gen.returncode != 0 or not sample_x6.is_file():
        return TestOutcome("FAIL", f"W2 MND-03 generate-sample cdr-x6 failed: {r_gen.stderr}")
    try:
        zcf_info = dissect_cdr_zcf(sample_x6.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 MND-03 sample cdr-x6 ZCF dissect failed: {exc}")
    if (
        not zcf_info["has_cdro_root"]
        or not zcf_info["has_valid_indirect_pointers"]
        or zcf_info["external_pointer_count"] < 1
        or zcf_info["invalid_pointer_count"] != 0
        or zcf_info["unindirected_loda_count"] != 0
        or not zcf_info["has_20b_deref_loda"]
        or zcf_info["deref_12b_loda_count"] != 0
    ):
        return TestOutcome("FAIL", f"W2 MND-03 generated cdr-x6 lacks authentic CDRO + 16B->20B loda indirection: {zcf_info}")

    x6_src = work / "probe_x6_src.coree"
    x6_src.write_bytes(build_coree_bytes("W2 X6 Convert"))
    conv_x6 = work / "converted_x6.cdr"
    r_conv_x6 = run_cmd([str(ctx.cli_bin), "convert", str(x6_src), str(conv_x6), "--cdr-version", "x6"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_conv_x6, "convert .coree -> x6 .cdr")
    if crash:
        return crash
    if r_conv_x6.returncode != 0 or not conv_x6.is_file():
        return TestOutcome("FAIL", f"W2 MND-03 convert to x6 .cdr failed: {r_conv_x6.stderr}")
    try:
        conv_zcf = dissect_cdr_zcf(conv_x6.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 MND-03 converted x6 .cdr ZCF dissect failed: {exc}")
    if (
        not conv_zcf["has_cdro_root"]
        or not conv_zcf["has_valid_indirect_pointers"]
        or conv_zcf["external_pointer_count"] < 1
        or conv_zcf["invalid_pointer_count"] != 0
        or conv_zcf["unindirected_loda_count"] != 0
        or not conv_zcf["has_20b_deref_loda"]
        or conv_zcf["deref_12b_loda_count"] != 0
    ):
        return TestOutcome("FAIL", f"W2 MND-03 converted x6 .cdr lacks authentic CDRO + 16B->20B loda indirection: {conv_zcf}")

    for filt, lbl, min_p in (
        ("zcf", "W2 REQ-06 zcf", 10),
        ("cmx", "W2 FEAT-82 cmx", 4),
        ("cpt", "W2 FEAT-82 cpt", 4),
    ):
        fail_cdr = _run_cargo_tests(ctx, ["-p", "coree-cdr", "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail_cdr:
            return fail_cdr
    return TestOutcome("PASS", "W2 CDR/ZCF/CMX/CPT/CDT boundary, cyclic cdss & odd UnknownChunkMap passthrough verified")


def _test_w3_color_mesh_effects_lens(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W3-COLOR-MESH-EFFECTS-LENS")
    color_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/color.rs"))
    icc_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/icc.rs"))
    lens_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/lens.rs"))
    eff_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/effects.rs"))
    sty_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/style.rs"))

    if "cmyk_to_rgb_icc" not in color_rs or not re.search(r"\bchad\b", icc_rs, re.I):
        return TestOutcome("FAIL", "W3 REQ-02: ICC + chad color pipeline missing in color.rs/icc.rs")
    if not re.search(r"\bbpc\b|\bblack_point_compensation\b", icc_rs + "\n" + color_rs, re.I):
        return TestOutcome("FAIL", "W3 REQ-02: Black Point Compensation (BPC) missing in color.rs/icc.rs")
    if "frozen_snapshot" not in lens_rs:
        return TestOutcome("FAIL", "W3 REM-04: LensConfig::frozen_snapshot missing in coree-core/src/lens.rs")
    for eff_sym in ("InnerShadow", "Contour", "Blend", "Envelope", "Extrude3D"):
        if eff_sym not in eff_rs:
            return TestOutcome("FAIL", f"W3 ADD-01: Effect::{eff_sym} missing in coree-core/src/effects.rs")
    trans_blk = _extract_brace_block(eff_rs, r"\bpub\s+enum\s+TransparencyMode\s*\{") or ""
    if not re.search(r"\b(?:FountainConical|Conical|FountainSquare|Square)\b", trans_blk):
        return TestOutcome("FAIL", "W3 ADD-01: Conical/Square TransparencyMode missing in coree-core/src/effects.rs")
    mesh_node_blk = _extract_brace_block(sty_rs, r"\bpub\s+struct\s+MeshNode\s*\{") or ""
    if not all(re.search(rf"\b{h}\b", mesh_node_blk) for h in ("handle_n", "handle_s", "handle_e", "handle_w")):
        return TestOutcome("FAIL", "W3 IMP-04: Coons MeshNode handle_n/s/e/w missing in coree-core/src/style.rs")

    # Live CLI BVA renders for MeshFill (1x1 vs 2x2 Coons), InnerShadow, FountainFill, TwoColorPattern, Contour/Blend/Envelope/Extrude3D, Frozen Lens
    cmyk_cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_mag = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    cmyk_yel = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 1.0, "k": 0.0, "a": 1.0}}
    cmyk_blk = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}
    zero_pt = {"x": 0.0, "y": 0.0}

    # 1x1 degenerate MeshFill vs 2x2 Coons MeshFill
    m1_doc = _minimal_doc_dict("Mesh 1x1 BVA")
    m1_doc["objects"]["3"]["outline"] = _default_outline(cmyk_blk, 0.0, False)
    m1_doc["objects"]["3"]["fill"] = {
        "MeshFill": {
            "cols": 1,
            "rows": 1,
            "nodes": [{"row": 0, "col": 0, "pos": zero_pt, "color": cmyk_cyan, "alpha": 1.0, "handle_n": zero_pt, "handle_s": zero_pt, "handle_e": zero_pt, "handle_w": zero_pt}],
        }
    }
    m1_in = work / "mesh_1x1.coree"
    m1_png = work / "mesh_1x1.png"
    _write_coree_from_doc(m1_in, "Mesh 1x1", m1_doc)
    r_m1 = run_cmd([str(ctx.cli_bin), "render", str(m1_in), str(m1_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_m1, "W3 render 1x1 MeshFill")
    if crash:
        return crash
    if r_m1.returncode == 0:
        if not m1_png.is_file():
            return TestOutcome("FAIL", "W3 IMP-04: 1x1 MeshFill exited 0 without writing PNG")
        try:
            parse_png_info(m1_png.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W3 IMP-04: 1x1 MeshFill PNG invalid: {exc}")
    elif m1_png.exists():
        return TestOutcome("FAIL", f"W3 IMP-04: 1x1 MeshFill failed (rc={r_m1.returncode}) yet left partial PNG")

    m2_doc = _minimal_doc_dict("Mesh 2x2 Coons BVA")
    m2_doc["objects"]["3"]["outline"] = _default_outline(cmyk_blk, 0.0, False)
    m2_doc["objects"]["3"]["fill"] = {
        "MeshFill": {
            "cols": 2,
            "rows": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": cmyk_cyan, "alpha": 0.5, "handle_n": zero_pt, "handle_s": {"x": 15.0, "y": 15.0}, "handle_e": {"x": 20.0, "y": 10.0}, "handle_w": zero_pt},
                {"row": 0, "col": 1, "pos": {"x": 60.0, "y": 0.0}, "color": cmyk_mag, "alpha": 1.0, "handle_n": zero_pt, "handle_s": {"x": -15.0, "y": 15.0}, "handle_e": zero_pt, "handle_w": {"x": -20.0, "y": 10.0}},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 40.0}, "color": cmyk_yel, "alpha": 1.0, "handle_n": {"x": 15.0, "y": -15.0}, "handle_s": zero_pt, "handle_e": {"x": 20.0, "y": -10.0}, "handle_w": zero_pt},
                {"row": 1, "col": 1, "pos": {"x": 60.0, "y": 40.0}, "color": cmyk_blk, "alpha": 1.0, "handle_n": {"x": -15.0, "y": -15.0}, "handle_s": zero_pt, "handle_e": zero_pt, "handle_w": {"x": -20.0, "y": -10.0}},
            ],
        }
    }
    m2_in = work / "mesh_2x2.coree"
    m2_png = work / "mesh_2x2.png"
    _write_coree_from_doc(m2_in, "Mesh 2x2", m2_doc)
    r_m2 = run_cmd([str(ctx.cli_bin), "render", str(m2_in), str(m2_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_m2, "W3 render 2x2 Coons MeshFill")
    if crash:
        return crash
    if r_m2.returncode != 0 or not m2_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: 2x2 Coons MeshFill render failed: {r_m2.stderr}")
    try:
        m2_info = parse_png_info(m2_png.read_bytes())
        if m2_info["non_white_pixels"] <= 50 or m2_info["unique_colors_sample"] <= 4 or m2_info["luma_stddev"] <= 1.0:
            return TestOutcome("FAIL", f"W3 IMP-04: 2x2 Coons MeshFill rendered blank/flat interior: {m2_info}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: 2x2 Coons MeshFill PNG invalid: {exc}")

    # FountainFill duplicate stops (FEAT-31) & TwoColorPattern zero tile_w (FEAT-32)
    pat_doc = _minimal_doc_dict("Pattern Zero Tile BVA")
    pat_doc["objects"]["3"]["fill"] = {
        "LinearGradient": {
            "start": zero_pt,
            "end": {"x": 60.0, "y": 40.0},
            "stops": [{"offset": 0.0, "color": cmyk_cyan}, {"offset": 0.0, "color": cmyk_mag}, {"offset": 1.0, "color": cmyk_yel}],
        }
    }
    pat_doc["objects"]["4"] = dict(pat_doc["objects"]["3"], id=4, name="Zero Tile Pattern", transform={"m": [1.0, 0.0, 0.0, 1.0, 100.0, 20.0]})
    pat_doc["objects"]["4"]["fill"] = {"TwoColorPattern": {"fg": cmyk_cyan, "bg": cmyk_mag, "pattern_name": "DiagonalStripe", "tile_w": 0.0, "tile_h": 0.0}}
    pat_doc["pages"][0]["layers"][0]["objects"].append(4)
    pat_doc["next_id"] = 5
    pat_in = work / "fills_bva.coree"
    fill_png = work / "fills_bva.png"
    _write_coree_from_doc(pat_in, "Fills BVA", pat_doc)
    r_fill = run_cmd([str(ctx.cli_bin), "render", str(pat_in), str(fill_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_fill, "W3 render FountainFill/Pattern BVA")
    if crash:
        return crash
    if r_fill.returncode == 0:
        if not fill_png.is_file():
            return TestOutcome("FAIL", "W3 FEAT-31/32: fills_bva exited 0 without creating PNG")
        try:
            if parse_png_info(fill_png.read_bytes())["non_white_pixels"] <= 20:
                return TestOutcome("FAIL", "W3 FEAT-31/32: fills_bva rendered blank PNG")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W3 FEAT-31/32: fills_bva PNG invalid: {exc}")
    elif fill_png.exists():
        return TestOutcome("FAIL", f"W3 FEAT-31/32: fills_bva failed (rc={r_fill.returncode}) yet left partial PNG")

    # Baseline Anchor Rect for pixel-delta comparison against Effects & Contour
    eff_base_doc = _minimal_doc_dict("Effects Baseline")
    eff_base_in = work / "effects_base.coree"
    eff_base_png = work / "effects_base.png"
    _write_coree_from_doc(eff_base_in, "Effects Baseline", eff_base_doc)
    r_eb = run_cmd([str(ctx.cli_bin), "render", str(eff_base_in), str(eff_base_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_eb.returncode != 0 or not eff_base_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01: Effects baseline render failed: {r_eb.stderr}")

    # Isolated Contour visual delta check (FEAT-45)
    ctr_doc = _minimal_doc_dict("Contour BVA")
    ctr_doc["objects"]["3"]["effects"] = [
        {"Contour": {"mode": "Outside", "steps": 2, "offset_mm": 3.0, "fill_color": cmyk_mag, "outline_color": cmyk_blk}}
    ]
    ctr_in = work / "contour_bva.coree"
    ctr_png = work / "contour_bva.png"
    _write_coree_from_doc(ctr_in, "Contour BVA", ctr_doc)
    r_ctr = run_cmd([str(ctx.cli_bin), "render", str(ctr_in), str(ctr_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ctr, "W3 render Contour BVA")
    if crash:
        return crash
    if r_ctr.returncode != 0 or not ctr_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-45: Contour effect render failed: {r_ctr.stderr}")
    try:
        ctr_delta = compare_png_pixels(eff_base_png, ctr_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-45: Contour effect PNG invalid: {exc}")
    if ctr_delta["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", "W3 FEAT-45: Contour effect produced no pixel delta over Anchor Rect baseline")

    # InnerShadow, Contour (0 steps), Blend (0 & dangling target), Envelope (12pt), Extrude3D
    eff_doc = _minimal_doc_dict("Effects BVA")
    eff_doc["objects"]["3"]["effects"] = [
        {"InnerShadow": {"dx_mm": 1.5, "dy_mm": 1.5, "blur_mm": 2.0, "opacity": 0.6, "color": cmyk_blk, "feather_direction": "Inside"}},
        {"Contour": {"mode": "Outside", "steps": 0, "offset_mm": 2.0, "fill_color": cmyk_mag, "outline_color": cmyk_blk}},
        {"Blend": {"target_id": 99999, "steps": 0, "rotation_deg": 0.0, "along_path": None}},
        {"Envelope": {"mode": "Unconstrained", "cage": [{"x": float(i), "y": float(i % 4)} for i in range(12)]}},
        {"Extrude3D": {"vanishing_point": {"x": 90.0, "y": 15.0}, "depth": 10.0, "bevel_depth": 1.0, "shade_color": cmyk_blk, "rotation_3d": [15.0, -10.0, 5.0], "lights": [{"dir": [0.5, -0.5, 0.7], "intensity": 0.8}]}},
    ]
    eff_in = work / "effects_bva.coree"
    eff_png = work / "effects_bva.png"
    _write_coree_from_doc(eff_in, "Effects BVA", eff_doc)
    r_eff = run_cmd([str(ctx.cli_bin), "render", str(eff_in), str(eff_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_eff, "W3 render Effects BVA")
    if crash:
        return crash
    if r_eff.returncode != 0 or not eff_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01/IMP-04: Effects BVA render failed: {r_eff.stderr}")
    try:
        if parse_png_info(eff_png.read_bytes())["non_white_pixels"] <= 50:
            return TestOutcome("FAIL", "W3 ADD-01: Effects BVA rendered blank PNG")
        eff_delta = compare_png_pixels(eff_base_png, eff_png)
        if eff_delta["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", f"W3 ADD-01: Effects BVA produced no visual delta vs Anchor Rect baseline: {eff_delta}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01: Effects BVA PNG invalid: {exc}")

    # Frozen Lens: invalid base64 frozen_snapshot rejection + valid snapshot convert round-trip persistence
    bad_lens_doc = _minimal_doc_dict("Bad Frozen Lens")
    bad_lens_doc["objects"]["3"]["lens"] = {"kind": "Invert", "frozen": True, "viewpoint": None, "remove_face": False, "frozen_snapshot": "!!!not_base64!!!"}
    bad_lens_in = work / "bad_frozen_lens.coree"
    _write_coree_from_doc(bad_lens_in, "Bad Frozen Lens", bad_lens_doc)
    r_bl = run_cmd([str(ctx.cli_bin), "inspect", str(bad_lens_in)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_bl, "W3 inspect invalid frozen_snapshot base64")
    if crash:
        return crash
    if r_bl.returncode == 0:
        return TestOutcome("FAIL", "W3 REM-04: invalid base64 in frozen_snapshot unexpectedly accepted (rc=0)")

    snap_b64 = base64.b64encode(_encode_png_rgba_stdlib(8, 8, bytes([0, 180, 255, 255] * 64))).decode("ascii")
    good_lens_doc = _minimal_doc_dict("Good Frozen Lens")
    good_lens_doc["objects"]["3"]["lens"] = {"kind": "Invert", "frozen": True, "viewpoint": None, "remove_face": False, "frozen_snapshot": snap_b64}
    good_lens_in = work / "good_frozen_lens.coree"
    good_lens_rt = work / "good_frozen_lens_rt.coree"
    _write_coree_from_doc(good_lens_in, "Good Frozen Lens", good_lens_doc)
    r_gl = run_cmd([str(ctx.cli_bin), "convert", str(good_lens_in), str(good_lens_rt)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_gl, "W3 convert frozen_snapshot")
    if crash:
        return crash
    if r_gl.returncode != 0 or not good_lens_rt.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: convert valid frozen_snapshot failed: {r_gl.stderr}")
    try:
        _, gl_doc = inspect_coree_zip(good_lens_rt)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04: converted frozen_snapshot .coree invalid: {exc}")
    gl_objs = gl_doc.get("objects")
    gl_obj3 = gl_objs.get("3") if isinstance(gl_objs, dict) else None
    gl_lens = gl_obj3.get("lens") if isinstance(gl_obj3, dict) else None
    if not isinstance(gl_lens, dict) or gl_lens.get("frozen_snapshot") != snap_b64:
        return TestOutcome("FAIL", f"W3 REM-04: frozen_snapshot lost on convert round-trip: {gl_lens!r}")

    for pkg, filt, lbl, min_p in (
        ("coree-core", "icc", "W3 REQ-02 icc+bpc", 12),
        ("coree-render", "inner_shadow", "W3 ADD-01 InnerShadow", 1),
        ("coree-render", "contour", "W3 FEAT-45 Contour", 2),
        ("coree-render", "bevel", "W3 ADD-01 bevel", 2),
        ("coree-render", "extrude", "W3 ADD-01 extrude3d", 1),
        ("coree-render", "blend", "W3 IMP-04 blend", 4),
        ("coree-render", "envelope", "W3 IMP-04 envelope", 2),
        ("coree-render", "transparency", "W3 ADD-01 transparency", 5),
    ):
        fail = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail:
            return fail
    return TestOutcome("PASS", "W3 ICC bounds, Coons MeshFill, InnerShadow/Contour/Blend/Envelope/Extrude3D & Frozen Lens BVA verified")


def _test_w3_typography_stylus_sculpt(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W3-TYPOGRAPHY-STYLUS-SCULPT")
    text_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/text.rs"))
    tl_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/text_layout.rs"))
    geom_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/geometry.rs"))
    stylus_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/stylus.rs"))
    fonts_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-storage/src/fonts.rs"))
    dock_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs"))
    types_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs"))

    for sym in ("linked_next_frame", "TextWrapMode", "drop_cap"):
        if sym not in text_rs:
            return TestOutcome("FAIL", f"W3 ADD-10: {sym} missing in coree-core/src/text.rs")
    if not re.search(r"\bfvar\b", tl_rs, re.I) or "layout_text_on_path_pieces" not in tl_rs:
        return TestOutcome("FAIL", "W3 ADD-10: variable font fvar or layout_text_on_path_pieces missing in text_layout.rs")
    if not re.search(r"\bpanose\b", fonts_rs + "\n" + tl_rs, re.I):
        return TestOutcome("FAIL", "W3 FEAT-41/REQ-07: OS/2 PANOSE font substitution missing in coree-storage/src/fonts.rs / text_layout.rs")
    if "ui_font_manager_docker" not in dock_rs or "ui_glyphs_browser" not in dock_rs or "FontManager" not in types_rs:
        return TestOutcome("FAIL", "W3 FEAT-41: Glyphs browser / FontManager docker missing in coree-app")
    for sculpt_fn in ("smooth_near", "smear_near", "twirl_near", "attract_repel_near", "roughen_near"):
        if not re.search(rf"\bfn\s+{sculpt_fn}\b", geom_rs):
            return TestOutcome("FAIL", f"W3 FEAT-16: sculpting method {sculpt_fn} missing in coree-core/src/geometry.rs")
    if "ArtisticMedia" not in geom_rs:
        return TestOutcome("FAIL", "W3 FEAT-20: ArtisticMedia missing in coree-core/src/geometry.rs")
    if not re.search(r"\btrait\s+StylusProvider\b", stylus_rs):
        return TestOutcome("FAIL", "W3 IMP-03: trait StylusProvider missing in crates/coree-app/src/stylus.rs production code")

    missing_cmd = require_cli_subcommand(ctx, "convert-to-curves", "W3")
    if missing_cmd:
        return missing_cmd

    # Live CLI convert-to-curves BVA: empty doc, Arabic RTL text, nonexistent path
    empty_doc = _minimal_doc_dict("Empty ConvertToCurves")
    empty_doc["objects"] = {}
    empty_doc["pages"][0]["layers"][0]["objects"] = []
    empty_in = work / "empty_ctc.coree"
    empty_out = work / "empty_ctc_out.coree"
    _write_coree_from_doc(empty_in, "Empty CTC", empty_doc)
    r_ce = run_cmd([str(ctx.cli_bin), "convert-to-curves", str(empty_in), str(empty_out)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ce, "convert-to-curves empty doc")
    if crash:
        return crash
    if r_ce.returncode == 0:
        if not empty_out.is_file():
            return TestOutcome("FAIL", "W3 FEAT-40: convert-to-curves empty doc exited 0 without creating output .coree")
        try:
            _, empty_res_doc = inspect_coree_zip(empty_out)
            _require_dict(empty_res_doc.get("objects"), "convert-to-curves empty doc objects")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W3 FEAT-40: convert-to-curves empty doc output invalid: {exc}")
    elif empty_out.exists():
        return TestOutcome("FAIL", f"W3 FEAT-40: convert-to-curves empty doc failed (rc={r_ce.returncode}) yet left partial file")

    ar_doc = _minimal_doc_dict("Arabic ConvertToCurves")
    ar_doc["objects"]["5"] = {
        "id": 5,
        "name": "Arabic Text",
        "payload": {
            "ArtisticText": {
                "runs": [{"text": "كوري استوديو", "style": _default_text_style("Noto Naskh Arabic", 24.0)}],
                "direction": "Rtl",
                "align": "Right",
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 80.0]},
        "fill": {"Solid": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}},
        "outline": _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.0, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    ar_doc["pages"][0]["layers"][0]["objects"].append(5)
    ar_doc["next_id"] = 6
    ar_in = work / "ar_ctc.coree"
    ar_out = work / "ar_ctc_out.coree"
    _write_coree_from_doc(ar_in, "Arabic CTC", ar_doc)
    r_ca = run_cmd([str(ctx.cli_bin), "convert-to-curves", str(ar_in), str(ar_out)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ca, "convert-to-curves Arabic")
    if crash:
        return crash
    if r_ca.returncode != 0 or not ar_out.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-40: convert-to-curves Arabic failed: {r_ca.stderr}")
    try:
        _, ar_out_doc = inspect_coree_zip(ar_out)
        ar_objs = _require_dict(ar_out_doc.get("objects"), "convert-to-curves objects")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-40: convert-to-curves output malformed: {exc}")
    max_nodes = 0
    for obj in ar_objs.values():
        if not isinstance(obj, dict) or not isinstance(obj.get("payload"), dict):
            continue
        if "ArtisticText" in obj["payload"]:
            return TestOutcome("FAIL", "W3 FEAT-40: convert-to-curves left ArtisticText unconverted")
        path_v = obj["payload"].get("Path")
        if isinstance(path_v, dict) and isinstance(path_v.get("subpaths"), list):
            for sp in path_v["subpaths"]:
                if isinstance(sp, dict) and isinstance(sp.get("nodes"), list):
                    max_nodes = max(max_nodes, len(sp["nodes"]))
    if max_nodes <= 4:
        return TestOutcome("FAIL", f"W3 FEAT-40: convert-to-curves produced trivial/AABB outline (max_nodes={max_nodes})")

    r_missing = run_cmd([str(ctx.cli_bin), "convert-to-curves", str(work / "missing.coree"), str(work / "missing_out.coree")], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_missing, "convert-to-curves missing file")
    if crash:
        return crash
    if r_missing.returncode == 0:
        return TestOutcome("FAIL", "W3 FEAT-40: convert-to-curves succeeded on nonexistent input")

    # Cyclic linked_next_frame (12 <-> 13) + empty & Arabic TextOnPath BVA render (without Anchor Rect)
    cyc_doc = _minimal_doc_dict("Cyclic Linked Frame")
    cyc_doc["objects"] = {}
    cyc_doc["pages"][0]["layers"][0]["objects"] = []
    p_style = _default_text_style("Noto Naskh Arabic", 14.0)
    black_out = _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.0, False)
    for fid, nxt in ((12, 13), (13, 12)):
        cyc_doc["objects"][str(fid)] = {
            "id": fid,
            "name": f"CycleFrame{fid}",
            "payload": {
                "ParagraphText": {
                    "runs": [{"text": "نص عربي متدفق " * 5, "style": p_style}],
                    "frame_size": {"x": 40.0, "y": 20.0},
                    "columns": 1,
                    "column_gutter_mm": 4.0,
                    "linked_next_frame": nxt,
                    "wrap_mode": "None",
                    "drop_cap": None,
                    "direction": "Rtl",
                    "align": "ForceJustifyKashida",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 30.0 * (fid - 11)]},
            "fill": "None", "outline": black_out, "lens": None, "effects": [],
            "blend_mode": "Normal", "opacity": 1.0, "visible": True, "locked": False, "printable": True,
            "style_id": None, "unknown_chunks": [],
        }
    cyc_doc["objects"]["14"] = {
        "id": 14,
        "name": "TextOnPath Host",
        "payload": {"Path": {"subpaths": [{"nodes": [
            {"pos": {"x": 20.0, "y": 140.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
            {"pos": {"x": 150.0, "y": 140.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
        ], "closed": False}]}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None", "outline": _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.3, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    cyc_doc["objects"]["15"] = {
        "id": 15,
        "name": "Empty TextOnPath",
        "payload": {"TextOnPath": {"text": {"runs": [{"text": "", "style": p_style}], "direction": "Ltr", "align": "Left"}, "path_id": 14, "offset_mm": 0.0, "distance_mm": 0.0, "flip_vertical": False, "flip_horizontal": False}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None", "outline": black_out, "lens": None, "effects": [],
        "blend_mode": "Normal", "opacity": 1.0, "visible": True, "locked": False, "printable": True,
        "style_id": None, "unknown_chunks": [],
    }
    cyc_ctrl_doc = json.loads(json.dumps(cyc_doc))
    cyc_ctrl_doc["pages"][0]["layers"][0]["objects"] = [12, 13, 14, 15]
    cyc_ctrl_doc["next_id"] = 16
    cyc_ctrl_in = work / "cyclic_frames_ctrl.coree"
    cyc_ctrl_png = work / "cyclic_frames_ctrl.png"
    _write_coree_from_doc(cyc_ctrl_in, "Cyclic Frames Ctrl", cyc_ctrl_doc)
    r_cfc = run_cmd([str(ctx.cli_bin), "render", str(cyc_ctrl_in), str(cyc_ctrl_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cfc, "render cyclic linked_next_frame control")
    if crash:
        return crash
    if r_cfc.returncode != 0 or not cyc_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: cyclic linked_next_frame control render failed (rc={r_cfc.returncode}): {r_cfc.stderr}")

    cyc_doc["objects"]["16"] = {
        "id": 16,
        "name": "Arabic TextOnPath",
        "payload": {"TextOnPath": {"text": {"runs": [{"text": "كوري على المسار", "style": p_style}], "direction": "Rtl", "align": "Right"}, "path_id": 14, "offset_mm": 5.0, "distance_mm": 2.0, "flip_vertical": False, "flip_horizontal": False}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": {"Solid": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}}, "outline": black_out, "lens": None, "effects": [],
        "blend_mode": "Normal", "opacity": 1.0, "visible": True, "locked": False, "printable": True,
        "style_id": None, "unknown_chunks": [],
    }
    cyc_doc["pages"][0]["layers"][0]["objects"].extend([12, 13, 14, 15, 16])
    cyc_doc["next_id"] = 17
    cyc_in = work / "cyclic_frames.coree"
    cyc_png = work / "cyclic_frames.png"
    _write_coree_from_doc(cyc_in, "Cyclic Frames", cyc_doc)
    r_cf = run_cmd([str(ctx.cli_bin), "render", str(cyc_in), str(cyc_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_cf, "render cyclic linked_next_frame + TextOnPath")
    if crash:
        return crash
    if r_cf.returncode != 0 or not cyc_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: cyclic linked_next_frame + TextOnPath render failed (rc={r_cf.returncode}): {r_cf.stderr}")
    try:
        cyc_info = parse_png_info(cyc_png.read_bytes())
        top_delta = compare_png_pixels(cyc_ctrl_png, cyc_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-10: cyclic linked_next_frame + TextOnPath PNG invalid: {exc}")
    if cyc_info["non_white_pixels"] <= 20:
        return TestOutcome("FAIL", "W3 ADD-10: cyclic linked_next_frame + TextOnPath rendered blank PNG")
    if top_delta["mean_abs_diff"] <= 0.02 or top_delta["nonzero_diff_ratio"] <= 0.0:
        return TestOutcome("FAIL", f"W3 ADD-10/FEAT-39: Arabic TextOnPath id=16 produced no pixel delta over control: {top_delta}")

    for pkg, filt, lbl, min_p, req_rx in (
        ("coree-storage", "fonts::", "W3 FEAT-41 font quarantine & PANOSE", 2, r"panose|quarantine|substit"),
        ("coree-app", "font_manager", "W3 FEAT-41 FontManager/Glyphs UI", 1, None),
        ("coree-render", "kashida", "W3 ADD-10 kashida", 2, None),
        ("coree-render", "subpixel", "W3 ADD-10 subpixel", 1, None),
        ("coree-app", "stylus", "W3 IMP-03 stylus", 1, None),
        ("coree-core", "sculpt", "W3 FEAT-16 sculpt", 2, None),
    ):
        fail = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=min_p, timeout=120.0, require_any_rx=req_rx)
        if fail:
            return fail
    return TestOutcome("PASS", "W3 Font quarantine/Glyphs (FEAT-41), convert-to-curves BVA, linked frames, TextOnPath, Sculpt & Stylus verified")


def _test_w3_photo_studio_boundary(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W3-PHOTO-STUDIO-BOUNDARY")
    eff_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/effects.rs"))
    doc_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/document.rs"))
    types_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs"))
    for filt_sym in ("ChannelMixer", "ToneCurve"):
        if filt_sym not in eff_rs:
            return TestOutcome("FAIL", f"W3 ADD-12: RasterFilter::{filt_sym} missing in coree-core/src/effects.rs")
    if "ChannelMixer" not in types_rs:
        return TestOutcome("FAIL", "W3 FEAT-51: ChannelMixer missing in S10 Bitmap Studio UI (state/types.rs)")

    render_src = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/raster_engine.rs"))
    has_cow = bool(re.search(r"\b(?:cow|copy_on_write|CowTile)\b|\bArc\s*<", render_src, re.I))
    has_16bit = bool(re.search(r"\b(?:u16_pixels|channels_u16|PixelDepth|Bit16|Rgb16|Rgba16|Cmyk16|16bit|TileBuffer)\b|\bVec\s*<\s*u16\s*>", render_src, re.I))
    if not has_cow or not has_16bit:
        return TestOutcome("FAIL", "W3 IMP-06: 16-bit tiled CoW raster engine missing in raster_engine.rs production code")
    if "ChannelMixer" not in render_src:
        return TestOutcome("FAIL", "W3 ADD-12: ChannelMixer missing in raster_engine.rs production code")
    if not re.search(r"\b(?:PixelLayer|pixel_layers|ClipMask|clip_mask)\b", doc_rs + "\n" + render_src):
        return TestOutcome("FAIL", "W3 FEAT-65: PixelLayer / ClipMask missing in document.rs / raster_engine.rs")
    for sel_sym in ("from_lasso", "from_magic_wand", "from_rectangle"):
        if sel_sym not in render_src:
            return TestOutcome("FAIL", f"W3 FEAT-66: RasterMask::{sel_sym} missing in raster_engine.rs")
    if not re.search(r"\bpoisson\b", render_src, re.I):
        return TestOutcome("FAIL", "W3 FEAT-67: Poisson healing brush missing in raster_engine.rs")
    for brush_sym in ("paint_brush_stroke", "apply_healing_stroke", "smudge_stroke", "apply_dodge_burn_stroke", "raster_flood_fill"):
        if brush_sym not in render_src:
            return TestOutcome("FAIL", f"W3 FEAT-67: brush primitive {brush_sym} missing in raster_engine.rs")
    if not re.search(r"\blanczos\b", render_src, re.I) or "apply_cutout_background_removal" not in render_src:
        return TestOutcome("FAIL", "W3 FEAT-69: Lanczos resample or Cutout Lab missing in raster_engine.rs")

    # Live CLI BVA render: Raster with ChannelMixer + ToneCurve + out-of-bounds crop_rect (Rect2D schema)
    bva_doc = _minimal_doc_dict("Photo Studio BVA")
    asset_id = "22222222-2222-4222-8222-222222222222"
    bva_doc["assets"] = {
        asset_id: {
            "id": asset_id,
            "width": 4,
            "height": 4,
            "rgba_bytes": base64.b64encode(bytes([220, 40, 20, 255] * 16)).decode("ascii"),
            "source_dpi": 72.0,
            "color_mode": "Rgb",
        }
    }
    bva_doc["objects"]["8"] = {
        "id": 8,
        "name": "Raster BVA",
        "payload": {
            "Raster": {
                "asset_id": asset_id,
                "width_mm": 40.0,
                "height_mm": 40.0,
                "filters": [
                    {"ChannelMixer": {"red": [0.0, 1.0, 0.0], "green": [1.0, 0.0, 0.0], "blue": [0.0, 0.0, 1.0], "monochrome": False}},
                    {"ToneCurve": {"points": [[0.0, 0.0], [1.0, 1.0]]}},
                ],
                "crop_rect": {"min_x": -10.0, "min_y": -10.0, "max_x": 190.0, "max_y": 190.0},
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
        "fill": "None",
        "outline": _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.0, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    bva_doc["pages"][0]["layers"][0]["objects"] = [8]
    bva_doc["next_id"] = 9
    ras_in = work / "photo_bva.coree"
    ras_png = work / "photo_bva.png"
    _write_coree_from_doc(ras_in, "Photo BVA", bva_doc)
    r_ras = run_cmd([str(ctx.cli_bin), "render", str(ras_in), str(ras_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_ras, "W3 Photo Studio BVA render")
    if crash:
        return crash
    if r_ras.returncode != 0 or not ras_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-06/ADD-12: Photo Studio BVA render failed: {r_ras.stderr}")
    try:
        ras_bytes = ras_png.read_bytes()
        ras_info = parse_png_info(ras_bytes)
        if ras_info["non_white_pixels"] <= 20:
            return TestOutcome("FAIL", f"W3 IMP-06/ADD-12: Photo Studio BVA rendered blank PNG: {ras_info}")
        _, _, ras_rgba = _decode_png_rgba_bytes_stdlib(ras_bytes)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-06/ADD-12: Photo Studio BVA PNG invalid: {exc}")
    has_swapped_green = any(
        ras_rgba[i + 3] > 200 and ras_rgba[i + 1] > ras_rgba[i] + 50 and ras_rgba[i + 1] > ras_rgba[i + 2] + 50
        for i in range(0, len(ras_rgba), 4)
    )
    if not has_swapped_green:
        return TestOutcome("FAIL", "W3 ADD-12: ChannelMixer R<->G swap on red raster produced no green-dominant pixels")

    for filt, lbl, min_p in (
        ("16bit", "W3 IMP-06 16bit", 1),
        ("cow", "W3 IMP-06 CoW", 1),
        ("mask", "W3 FEAT-66 raster mask/selection", 2),
        ("poisson", "W3 FEAT-67 poisson healing", 1),
        ("resample", "W3 FEAT-69 resample/lanczos", 1),
    ):
        fail_ps = _run_cargo_tests(ctx, ["-p", "coree-render", "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail_ps:
            return fail_ps
    return TestOutcome("PASS", "W3 Photo Studio (16-bit CoW, Pixel Layers, Masking, 8 Brushes/Poisson, ChannelMixer/ToneCurve & Lanczos Resample) BVA verified")


def _test_w4_prepress_imposition_barcode(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W4-PREPRESS-IMPOSITION-BARCODE")
    dl_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-io/src/display_list.rs"))
    if not dl_rs or "ExportDisplayList" not in dl_rs:
        return TestOutcome("FAIL", "W4 REQ-04: ExportDisplayList in crates/coree-io/src/display_list.rs missing")
    prep_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/prepress.rs"))
    if "PerfectBound" not in prep_rs:
        return TestOutcome("FAIL", "W4 REQ-05: PerfectBound imposition missing in coree-prod/src/prepress.rs")
    bc_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/barcode.rs"))
    if "DataMatrix" not in bc_rs or "Pdf417" not in bc_rs:
        return TestOutcome("FAIL", "W4 ADD-05: DataMatrix/Pdf417 2D barcodes missing in coree-prod/src/barcode.rs")
    pf_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/preflight.rs"))
    if "MissingFont" not in pf_rs:
        return TestOutcome("FAIL", "W4 REQ-07: MissingFont preflight check missing in coree-prod/src/preflight.rs")
    col_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/collect.rs"))
    if not re.search(r"\bfsType\b|\bfstype\b", col_rs, re.I):
        return TestOutcome("FAIL", "W4 REQ-07: collect.rs missing OS/2 fsType font embedding audit")

    # Live CLI boundary tests: MissingFont preflight, read-only collect dir, negative kerf laser boundary
    mf_doc = _minimal_doc_dict("MissingFont BVA")
    mf_doc["objects"]["5"] = {
        "id": 5,
        "name": "Missing Font Text",
        "payload": {
            "ArtisticText": {
                "runs": [{"text": "Preflight Probe", "style": _default_text_style("NonExistentCoreeTestFont_XYZ_999", 18.0)}],
                "direction": "Ltr",
                "align": "Left",
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 80.0]},
        "fill": {"Solid": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}},
        "outline": _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.0, False),
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    mf_doc["pages"][0]["layers"][0]["objects"].append(5)
    mf_doc["next_id"] = 6
    mf_in = work / "missing_font_bva.coree"
    _write_coree_from_doc(mf_in, "MissingFont BVA", mf_doc)
    r_pf = run_cmd([str(ctx.cli_bin), "preflight", str(mf_in), "--min-dpi", "300", "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_pf, "W4 preflight MissingFont")
    if crash:
        return crash
    if r_pf.returncode != 0:
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json failed: {r_pf.stderr}")
    try:
        pf_rep = _require_dict(json.loads(r_pf.stdout), "W4 preflight MissingFont")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json invalid JSON: {exc}")
    issues = pf_rep.get("issues")
    if not isinstance(issues, list) or "MissingFont" not in json.dumps(issues):
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json did not report MissingFont: {issues!r}")

    ro_dir = work / "ro_collect_target"
    ro_dir.mkdir(exist_ok=True)
    ro_sub = ro_dir / "nested"
    os.chmod(ro_dir, 0o500)
    try:
        r_col = run_cmd([str(ctx.cli_bin), "collect", str(mf_in), str(ro_sub)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    finally:
        os.chmod(ro_dir, 0o700)
    crash = _assert_no_panic_or_crash(r_col, "W4 collect read-only target")
    if crash:
        return crash
    if os.geteuid() != 0 and r_col.returncode == 0:
        return TestOutcome("FAIL", "W4 REQ-07: collect unexpectedly succeeded on read-only target directory")

    for pkg, filt, lbl in (
        ("coree-io", "display_list", "W4 REQ-04 display_list"),
        ("coree-app", "ipp", "W4 REQ-10 cups ipp"),
        ("coree-prod", "perfect_bound", "W4 REQ-05 perfect_bound"),
        ("coree-prod", "fefco", "W4 ADD-05 fefco"),
    ):
        fail = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=1, timeout=120.0)
        if fail:
            return fail
    fail_bc = _run_cargo_tests(
        ctx,
        ["-p", "coree-prod", "--lib"],
        "W4 ADD-05 2D barcodes",
        min_passed=1,
        timeout=120.0,
        required_test_rxs=(r"data_?matrix", r"pdf_?417"),
    )
    if fail_bc:
        return fail_bc
    return TestOutcome("PASS", "W4 Prepress/Imposition/Barcode/Preflight/Collect/CUPS IPP/DisplayList boundary checks verified")


def _test_w4_exchange_dwg_emf_psd_bva(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W4-EXCHANGE-DWG-EMF-PSD-BVA")
    for rel in (
        "crates/coree-io/src/dwg.rs",
        "crates/coree-io/src/wmf_emf.rs",
        "crates/coree-io/src/palette_io.rs",
    ):
        if not (ctx.repo_root / rel).is_file():
            return TestOutcome("FAIL", f"W4 ADD-06: required codec file missing: {rel}")

    pal_raw = (ctx.repo_root / "crates" / "coree-io" / "src" / "palette_io.rs").read_text(encoding="utf-8", errors="replace")
    pal_comments_only = _strip_cfg_test_modules(strip_rust_comments_only(pal_raw))
    if "8BCB" not in pal_comments_only:
        return TestOutcome("FAIL", "W4 ADD-06: palette_io.rs production code missing Adobe Color Book 8BCB (.acb) support")

    # 1. Positive control inspect --json on valid DWG, EMF, ACB probe fixtures
    for fname, builder, fmt_tok in (
        ("probe_ok.dwg", build_dwg_probe_bytes, "dwg"),
        ("probe_ok.emf", build_emf_probe_bytes, "emf"),
        ("probe_ok.acb", build_acb_probe_bytes, "acb"),
    ):
        probe_p = work / fname
        probe_p.write_bytes(builder())
        r_ok = run_cmd([str(ctx.cli_bin), "inspect", str(probe_p), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r_ok, f"W4 inspect {fname}")
        if crash:
            return crash
        if r_ok.returncode != 0:
            return TestOutcome("FAIL", f"W4 ADD-06: valid {fname} inspect failed (rc={r_ok.returncode}): {r_ok.stderr}")
        try:
            rep = _require_dict(json.loads(r_ok.stdout), f"inspect {fname}")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W4 ADD-06: {fname} inspect invalid JSON: {exc}")
        det = str(rep.get("detected_format", "")).lower()
        if fmt_tok not in det:
            return TestOutcome("FAIL", f"W4 ADD-06: {fname} detected_format={det!r}, expected {fmt_tok!r}")

    # 2. Negative DWG/EMF/WMF/ACB boundaries
    bad_dwg = bytearray(build_dwg_probe_bytes())
    bad_dwg[-1] ^= 0xFF
    bad_dwg_p = work / "bad_crc.dwg"
    bad_dwg_p.write_bytes(bytes(bad_dwg))

    trunc_emf_p = work / "trunc.emf"
    trunc_emf_p.write_bytes(build_emf_probe_bytes()[:32])

    trunc_wmf_p = work / "trunc.wmf"
    trunc_wmf_p.write_bytes(b"\xd7\xcd\xc6\x9a\x00\x00\x01\x00\x00\x00\x00\x00")

    trunc_acb_p = work / "trunc.acb"
    trunc_acb_p.write_bytes(build_acb_probe_bytes()[:16])

    for lbl, p in (("bad_dwg", bad_dwg_p), ("trunc_emf", trunc_emf_p), ("trunc_wmf", trunc_wmf_p), ("trunc_acb", trunc_acb_p)):
        r = run_cmd([str(ctx.cli_bin), "inspect", str(p)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r, f"W4 inspect {lbl}")
        if crash:
            return crash
        if r.returncode == 0:
            return TestOutcome("FAIL", f"W4 inspect {lbl} unexpectedly succeeded on corrupt input")

    # 3. Oversized (>2048px) valid sparse PackBits PSD acceptance (full 2560x4 preserved, no 2048px truncation) + PackBits overrun + ZIP overflow rejection
    big_psd_p = work / "oversized_2560x4.psd"
    big_psd_bytes = _build_packbits_psd_bytes(2560, 4)
    dissect_psd(big_psd_bytes)
    big_psd_p.write_bytes(big_psd_bytes)
    big_psd_coree = work / "oversized_2560x4.coree"
    r_psd_coree = run_cmd([str(ctx.cli_bin), "convert", str(big_psd_p), str(big_psd_coree)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_psd_coree, "W4 convert >2048px PackBits PSD -> .coree")
    if crash:
        return crash
    if r_psd_coree.returncode != 0 or not big_psd_coree.is_file():
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px sparse PackBits PSD -> .coree failed: {r_psd_coree.stderr}")
    try:
        _, psd_doc = inspect_coree_zip(big_psd_coree)
        asset_dims: list[tuple[int, int]] = []
        raw_assets = psd_doc.get("assets")
        if isinstance(raw_assets, dict) and raw_assets:
            for meta in raw_assets.values():
                if isinstance(meta, dict) and isinstance(meta.get("width"), int) and isinstance(meta.get("height"), int):
                    asset_dims.append((meta["width"], meta["height"]))
        if not asset_dims:
            with zipfile.ZipFile(big_psd_coree, "r") as zf:
                if "assets/manifest.json" in zf.namelist():
                    am = json.loads(zf.read("assets/manifest.json").decode("utf-8"))
                    if isinstance(am, list):
                        for item in am:
                            if isinstance(item, dict) and isinstance(item.get("width"), int) and isinstance(item.get("height"), int):
                                asset_dims.append((item["width"], item["height"]))
                for entry_name in zf.namelist():
                    if entry_name.startswith("assets/") and entry_name.endswith(".png"):
                        apng = parse_png_info(zf.read(entry_name))
                        asset_dims.append((apng["width"], apng["height"]))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px PSD converted .coree invalid: {exc}")
    if not asset_dims or any(w != 2560 or h != 4 for w, h in asset_dims):
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px PSD truncated/resized in .coree assets (expected 2560x4): {asset_dims}")

    big_psd_png = work / "oversized_2560x4.png"
    r_psd_ok = run_cmd([str(ctx.cli_bin), "render", str(big_psd_p), str(big_psd_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_psd_ok, "W4 render >2048px PackBits PSD --dpi 72")
    if crash:
        return crash
    if r_psd_ok.returncode != 0 or not big_psd_png.is_file():
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px sparse PackBits PSD --dpi 72 render failed: {r_psd_ok.stderr}")
    try:
        psd_png_bytes = big_psd_png.read_bytes()
        psd_png_info = parse_png_info(psd_png_bytes)
        _, _, psd_rgba = _decode_png_rgba_bytes_stdlib(psd_png_bytes)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px PSD output PNG invalid: {exc}")
    if psd_png_info["width"] < 2560 or psd_png_info["height"] < 4 or psd_png_info["non_white_pixels"] < 2560:
        return TestOutcome("FAIL", f"W4 FEAT-86/IMP-06: >2048px PSD --dpi 72 PNG invalid bounds/blank: {psd_png_info}")
    has_expected_gray = any(
        abs(psd_rgba[i] - 200) <= 2 and abs(psd_rgba[i + 1] - 200) <= 2 and abs(psd_rgba[i + 2] - 200) <= 2 and psd_rgba[i + 3] == 255
        for i in range(0, len(psd_rgba), 4)
    )
    if not has_expected_gray:
        return TestOutcome("FAIL", "W4 FEAT-86/IMP-06: >2048px PSD render missing expected [200, 200, 200, 255] gray pixels")

    overrun_psd_p = work / "packbits_overrun.psd"
    overrun_psd_p.write_bytes(
        b"8BPS" + struct.pack(">H", 1) + b"\x00" * 6 + struct.pack(">HIIHH", 1, 2, 16, 8, 1)
        + struct.pack(">III", 0, 0, 0) + struct.pack(">H", 1) + struct.pack(">HH", 65000, 65000) + b"\x81\xff"
    )
    zip_bomb_psd_p = work / "zip_overflow.psd"
    zip_bomb_psd_p.write_bytes(
        b"8BPS" + struct.pack(">H", 1) + b"\x00" * 6 + struct.pack(">HIIHH", 1, 64, 64, 8, 1)
        + struct.pack(">III", 0, 0, 0) + struct.pack(">H", 2) + zlib.compress(b"\x00" * (128 * 1024))
    )
    for lbl, p in (("packbits_overrun_psd", overrun_psd_p), ("zip_overflow_psd", zip_bomb_psd_p)):
        r = run_cmd([str(ctx.cli_bin), "inspect", str(p)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r, f"W4 inspect {lbl}")
        if crash:
            return crash
        if r.returncode == 0:
            return TestOutcome("FAIL", f"W4 FEAT-86: {lbl} unexpectedly succeeded")

    # 4. Cyclic DXF INSERT, EPS unbounded repeat bomb, malformed PLT
    cyc_dxf = work / "cyclic_insert.dxf"
    cyc_dxf.write_text(
        "0\nSECTION\n2\nBLOCKS\n"
        "0\nBLOCK\n8\n0\n2\nB1\n10\n0\n20\n0\n30\n0\n"
        "0\nINSERT\n8\n0\n2\nB2\n10\n0\n20\n0\n"
        "0\nENDBLK\n"
        "0\nBLOCK\n8\n0\n2\nB2\n10\n0\n20\n0\n30\n0\n"
        "0\nINSERT\n8\n0\n2\nB1\n10\n0\n20\n0\n"
        "0\nENDBLK\n"
        "0\nENDSEC\n0\nSECTION\n2\nENTITIES\n"
        "0\nINSERT\n8\n0\n2\nB1\n10\n0\n20\n0\n"
        "0\nENDSEC\n0\nEOF\n",
        encoding="utf-8",
    )
    r_dxf = run_cmd([str(ctx.cli_bin), "inspect", str(cyc_dxf)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_dxf, "W4 cyclic DXF INSERT")
    if crash:
        return crash

    eps_bomb = work / "repeat_bomb.eps"
    eps_bomb.write_text("%!PS-Adobe-3.0 EPSF-3.0\n%%BoundingBox: 0 0 100 100\n10000 { 10000 { } repeat } repeat\n", encoding="utf-8")
    r_eps = run_cmd([str(ctx.cli_bin), "inspect", str(eps_bomb)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_eps, "W4 EPS repeat bomb")
    if crash:
        return crash
    if r_eps.returncode == 0:
        return TestOutcome("FAIL", "W4 FEAT-84: EPS unbounded repeat bomb unexpectedly returned rc=0")

    bad_plt = work / "malformed.plt"
    bad_plt.write_text("IN;SP999;PDabc,xyz;PU10,20,30;BADCMD;", encoding="utf-8")
    r_plt = run_cmd([str(ctx.cli_bin), "inspect", str(bad_plt)], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_plt, "W4 malformed PLT")
    if crash:
        return crash

    for filt, lbl, min_p in (
        ("dwg", "W4 ADD-06 dwg", 1),
        ("emf", "W4 ADD-06 emf", 1),
        ("acb", "W4 ADD-06 acb", 1),
        ("psd", "W4 FEAT-86 psd", 8),
    ):
        fail_io = _run_cargo_tests(ctx, ["-p", "coree-io", "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail_io:
            return fail_io
    return TestOutcome("PASS", "W4 DWG/EMF/WMF/ACB/PSD (>2048px & corrupt RLE/CRC) + cyclic DXF/EPS bomb/PLT boundary checks verified")


def _test_w4_cql_rhai_plugin_sandbox(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W4-CQL-RHAI-PLUGIN-SANDBOX")
    missing_cmd = require_cli_subcommand(ctx, "cql", "W4")
    if missing_cmd:
        return missing_cmd

    cql_help = run_cmd([str(ctx.cli_bin), "cql", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(cql_help, "cql --help")
    if crash:
        return crash
    if cql_help.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-11: cql --help failed: {cql_help.stderr}")
    use_query_flag = "--query" in cql_help.stdout

    doc_p = work / "cql_target.coree"
    doc_p.write_bytes(build_coree_bytes("CQL Boundary"))

    # Positive control query first to prove CQL CLI syntax is valid
    pos_cmd = [str(ctx.cli_bin), "cql", str(doc_p), *(["--query"] if use_query_flag else []), "@name == 'Primary Rect'"]
    r_pos = run_cmd(pos_cmd, cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_pos, "cql positive control")
    if crash:
        return crash
    if r_pos.returncode != 0 or "Primary Rect" not in r_pos.stdout or "Die Cut Box" in r_pos.stdout:
        return TestOutcome("FAIL", f"W4 ADD-11: cql positive control query failed (rc={r_pos.returncode}): {r_pos.stdout[:200]} {r_pos.stderr[:200]}")

    for bad_q in ("", "(((unclosed", "fill == 12345"):
        q_cmd = [str(ctx.cli_bin), "cql", str(doc_p), *(["--query"] if use_query_flag else []), bad_q]
        r = run_cmd(q_cmd, cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
        crash = _assert_no_panic_or_crash(r, f"cql invalid {bad_q!r}")
        if crash:
            return crash
        if r.returncode == 0:
            return TestOutcome("FAIL", f"cql invalid query {bad_q!r} unexpectedly exited 0")

    inf_loop = work / "inf_loop.rhai"
    inf_loop.write_text("while true {}\n", encoding="utf-8")
    r_loop = run_cmd(
        [str(ctx.cli_bin), "run-macro", str(doc_p), str(inf_loop), str(work / "out.coree")],
        cwd=work,
        timeout=15.0,
        timeout_scale=ctx.timeout_scale,
    )
    crash = _assert_no_panic_or_crash(r_loop, "run-macro infinite loop sandbox")
    if crash:
        return crash
    if r_loop.returncode == 0:
        return TestOutcome("FAIL", "run-macro infinite loop should have aborted cleanly via max_operations")

    for filt, lbl, min_p in (
        ("jsonrpc", "W4 ADD-11 jsonrpc", 1),
        ("on_before_export", "W4 ADD-11 event hooks", 1),
        ("cql", "W4 ADD-11 cql", 3),
    ):
        fail_plug = _run_cargo_tests(ctx, ["-p", "coree-prod", "--", filt], lbl, min_passed=min_p, timeout=120.0)
        if fail_plug:
            return fail_plug
    return TestOutcome("PASS", "W4 CQL syntax/OOB, Rhai max_operations sandbox & JSON-RPC plugin manifest bounds verified")


def _test_w5_welcome_dockers_presets(ctx: RunContext) -> TestOutcome:
    types_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs"))
    if "Illustrator" not in types_rs:
        return TestOutcome("FAIL", "W5 ADD-09: WorkspacePreset::Illustrator missing in crates/coree-app/src/state/types.rs")
    canvas_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs"))
    if "DrawingBox" in canvas_rs:
        return TestOutcome("FAIL", "W5 IMP-08: generic CanvasDragState::DrawingBox fallback still present in canvas.rs")

    pal_raw = (ctx.repo_root / "crates" / "coree-app" / "src" / "ui" / "palette_bar.rs").read_text(encoding="utf-8", errors="replace")
    pal_rs = _strip_cfg_test_modules(strip_rust_comments_and_strings(pal_raw))
    has_tint_grid = bool(re.search(r"\btint_grid\b|\bTintGrid\b|\bshow_tint_grid\b", pal_rs))
    if not has_tint_grid:
        return TestOutcome("FAIL", "W5 ADD-09: Long-press Tint Grid missing in palette_bar.rs production code")
    if not re.search(r"\b(?:click_and_drag|drag_started|dnd_set_payload|DragSwatch|drag_swatch)\b", pal_rs):
        return TestOutcome("FAIL", "W5 ADD-09: Drag-and-drop swatch (click_and_drag/drag_swatch) missing in palette_bar.rs")

    ty_shortcut = _extract_brace_block(types_rs, r"\bfn\s+shortcut_action\b")
    if not ty_shortcut or re.search(r"_\s*=>\s*None\b", ty_shortcut):
        return TestOutcome("FAIL", "W5 IMP-09: ToolId::shortcut_action leaves tools unmapped (65/65 required)")

    tb_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/toolbox.rs"))
    pti_body = _extract_brace_block(tb_rs, r"\bfn\s+paint_tool_icon\b") or ""
    shared_arms = re.findall(r"ToolId::([A-Za-z0-9_]+)\s*\|", pti_body)
    distinct_tools = set(re.findall(r"ToolId::([A-Za-z0-9_]+)", pti_body))
    if shared_arms or len(distinct_tools) < 65:
        return TestOutcome("FAIL", f"W5 IMP-08: paint_tool_icon lacks 65 distinct per-tool icons (shared={shared_arms}, distinct={len(distinct_tools)})")

    # FEAT-03: 14 Rearrangeable Dockers via egui_dock
    dock_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs"))
    prod_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/state/production.rs"))
    docker_impl = _extract_brace_block(types_rs, r"\bimpl\s+DockerTab\b") or ""
    docker_all_m = re.search(r"\bpub\s+const\s+ALL\s*:\s*\[\s*Self\s*;\s*14\s*\]\s*=\s*\[(.*?)\]", docker_impl, re.S)
    docker_variants = re.findall(r"\bSelf::([A-Za-z0-9_]+)\b", docker_all_m.group(1) if docker_all_m else "")
    if len(docker_variants) != 14 or "DockerTab" not in types_rs:
        return TestOutcome("FAIL", f"W5 FEAT-03: DockerTab::ALL must declare 14 dockers (found {len(docker_variants)})")
    if "egui_dock" not in dock_rs or "DockArea" not in dock_rs or "show_dockers_panel" not in dock_rs:
        return TestOutcome("FAIL", "W5 FEAT-03: egui_dock DockArea / show_dockers_panel missing in ui/dockers.rs")
    if "toggle_docker" not in prod_rs or "focus_docker" not in prod_rs:
        return TestOutcome("FAIL", "W5 FEAT-03: docker toggle/focus handling missing in state/production.rs")

    for filt, lbl, min_p, req_rx in (
        ("welcome", "W5 FEAT-01 welcome", 1, None),
        ("illustrator", "W5 ADD-09 illustrator", 1, None),
        ("docker", "W5 FEAT-03 dockers", 2, r"rearrange|toggle.*docker|docker.*(?:rearrange|toggle|14|panel)"),
    ):
        fail_w5 = _run_cargo_tests(ctx, ["-p", "coree-app", "--", filt], lbl, min_passed=min_p, timeout=180.0, require_any_rx=req_rx)
        if fail_w5:
            return fail_w5
    fail_app = _run_cargo_tests(ctx, ["-p", "coree-app", "--lib"], "W5 FEAT-01/03/06/37 coree-app", min_passed=20, timeout=180.0)
    if fail_app:
        return fail_app
    fail_integ = _run_cargo_tests(
        ctx,
        ["-p", "coree-app", "--test", "studio_integration"],
        "W5 S01-S18 headless egui",
        min_passed=57,
        timeout=180.0,
        require_any_rx=r"welcome|illustrator|tint_grid|drag_swatch|rearrange.*docker|docker.*rearrange",
    )
    if fail_integ:
        return fail_integ
    return TestOutcome("PASS", "W5 Welcome Hub S01, Multi-doc tabs, 14 Dockers (FEAT-03), 65/65 icons, Illustrator preset & Swatch TintGrid/DnD verified")


def _test_w5_powertrace_interactive(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W5-POWERTRACE-INTERACTIVE")
    dock_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs"))
    if not re.search(r"split_preview|SplitPreview", dock_rs):
        return TestOutcome("FAIL", "W5 IMP-05: Split Preview control missing in S11 PowerTRACE docker")
    trace_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/trace.rs"))
    if "potrace" not in trace_rs.lower():
        return TestOutcome("FAIL", "W5 IMP-05: topological Potrace missing in coree-render/src/trace.rs")

    # >16MP (4200x4200) solid PNG trace boundary under RLIMIT_AS=512 MiB (clean clamp or rejection, 0 panics)
    big_png = work / "over16mp_4200x4200.png"
    big_png.write_bytes(_build_streaming_solid_png_bytes(4200, 4200))
    big_svg = work / "over16mp_traced.svg"
    r_big = run_cmd([str(ctx.cli_bin), "trace", str(big_png), str(big_svg), "--mode", "outline", "--colors", "2"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_big, "W5 trace >16MP 4200x4200")
    if crash:
        return crash
    if r_big.returncode == 0:
        if not big_svg.is_file():
            return TestOutcome("FAIL", "W5 FEAT-72: trace >16MP exited 0 without creating SVG")
        try:
            big_svg_s = parse_svg_structure(big_svg.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W5 FEAT-72: >16MP traced SVG malformed: {exc}")
        if big_svg_s.get("counts", {}).get("svg", 0) < 1:
            return TestOutcome("FAIL", f"W5 FEAT-72: >16MP trace output invalid SVG: {big_svg_s}")
    elif big_svg.exists():
        return TestOutcome("FAIL", f"W5 FEAT-72: trace >16MP failed (rc={r_big.returncode}) yet left partial SVG")

    fail_tr = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "trace"], "W5 FEAT-72 coree-render trace", min_passed=3, timeout=120.0)
    if fail_tr:
        return fail_tr
    fail_ui = _run_cargo_tests(ctx, ["-p", "coree-app", "--", "trace"], "W5 FEAT-72 coree-app S11", min_passed=8, timeout=120.0)
    if fail_ui:
        return fail_ui
    return TestOutcome("PASS", "W5 PowerTRACE S11 interactive sliders (colors/detail/smoothing/corners/remove_bg/SplitPreview) & >16MP BVA verified")


def _test_w5_view_guides_navigator_printui(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI")
    cli_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/cli.rs"))
    if "SimpleWireframe" not in cli_rs:
        return TestOutcome("FAIL", "W5 ADD-08: ViewModeArg::SimpleWireframe missing in crates/coree-app/src/cli.rs")
    doc_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-core/src/document.rs"))
    if "printable_background" not in doc_rs:
        return TestOutcome("FAIL", "W5 REQ-12: Page::printable_background missing in coree-core/src/document.rs")
    ren_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs"))
    layout_fn = _extract_brace_block(ren_rs, r"\bfn\s+compute_page_layouts\b") or ""
    if "facing_pages" not in layout_fn:
        return TestOutcome("FAIL", "W5 FEAT-10/ADD-07: compute_page_layouts in renderer.rs missing facing_pages spread layout")

    canvas_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs"))
    hit_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-render/src/hit_test.rs"))
    tools_dir = ctx.repo_root / "crates" / "coree-tools" / "src"
    tools_src = "\n".join(
        _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
        for p in sorted(tools_dir.rglob("*.rs"))
    ) if tools_dir.is_dir() else ""
    ui_dir = ctx.repo_root / "crates" / "coree-app" / "src" / "ui"
    ui_src = "\n".join(
        _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
        for p in sorted(ui_dir.rglob("*.rs"))
    ) if ui_dir.is_dir() else ""

    if "paint_rulers" not in canvas_rs or "handle_ruler_guide_drag" not in canvas_rs:
        return TestOutcome("FAIL", "W5 FEAT-04: Interactive Rulers/Guideline drag missing in ui/canvas.rs")
    if "SnapOptions" not in hit_rs or "dynamic_guides" not in hit_rs:
        return TestOutcome("FAIL", "W5 FEAT-04: SnapOptions / dynamic_guides missing in hit_test.rs")
    for snap_sym in ("baseline", "quadrant", "tangent", "perpendicular", "intersection"):
        if not re.search(rf"\b{snap_sym}\b", hit_rs + "\n" + tools_src, re.I):
            return TestOutcome("FAIL", f"W5 FEAT-04: geometric snap {snap_sym} missing in hit_test.rs / coree-tools")
    if not re.search(r"\bnavigator\b", ui_src, re.I):
        return TestOutcome("FAIL", "W5 FEAT-13: Navigator popup missing in crates/coree-app/src/ui/")
    if "ReorderPages" not in ui_src:
        return TestOutcome("FAIL", "W5 FEAT-08: S03 drag-reorder CommandOp::ReorderPages missing in crates/coree-app/src/ui/")

    dlg_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/dialogs.rs"))
    dock_rs = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs"))
    pp_thumb_fn = _extract_brace_block(dlg_rs, r"\bfn\s+draw_print_preview_thumbnail\b") or ""
    if "separations_preview" not in pp_thumb_fn:
        return TestOutcome("FAIL", "W5 IMP-09: draw_print_preview_thumbnail missing separations_preview in dialogs.rs")
    if not re.search(r"\bCP1256\b", dlg_rs + "\n" + dock_rs, re.I):
        return TestOutcome("FAIL", "W5 REQ-08: S15 CP1256/CP1251/UTF-8 codepage selector missing")
    if not re.search(r"\bHex\b|hex_view", dock_rs):
        return TestOutcome("FAIL", "W5 EXT-F04: S16 Hex/ASCII chunk byte viewer missing in dockers.rs")

    # Live CLI render --view simple-wireframe, facing_pages --multipage spread & Page.printable_background BVA
    r_help = run_cmd([str(ctx.cli_bin), "render", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if "simple-wireframe" not in r_help.stdout.lower():
        return TestOutcome("FAIL", "W5 ADD-08: simple-wireframe missing in coree-cli render --help")

    doc_in = work / "view_bva.coree"
    doc_in.write_bytes(build_coree_bytes("View BVA"))
    swf_png = work / "swf.png"
    r_swf = run_cmd([str(ctx.cli_bin), "render", str(doc_in), str(swf_png), "--dpi", "72", "--view", "simple-wireframe"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_swf, "W5 render simple-wireframe")
    if crash:
        return crash
    if r_swf.returncode != 0 or not swf_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-08: render --view simple-wireframe failed: {r_swf.stderr}")

    # FEAT-10: 3-page Facing Pages --multipage spread diff vs non-facing
    fp_doc = _minimal_doc_dict("Facing Pages BVA")
    fp_p2 = json.loads(json.dumps(fp_doc["pages"][0]))
    fp_p2["id"] = 101
    fp_p2["name"] = "Page 2"
    fp_p3 = json.loads(json.dumps(fp_doc["pages"][0]))
    fp_p3["id"] = 103
    fp_p3["name"] = "Page 3"
    fp_doc["pages"] = [fp_doc["pages"][0], fp_p2, fp_p3]
    fp_doc["next_id"] = 105
    fp_off_in, fp_off_png = work / "fp_off.coree", work / "fp_off.png"
    _write_coree_from_doc(fp_off_in, "Facing Off", fp_doc)
    r_fpoff = run_cmd([str(ctx.cli_bin), "render", str(fp_off_in), str(fp_off_png), "--dpi", "48", "--multipage"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    for pg in fp_doc["pages"]:
        pg["facing_pages"] = True
    fp_on_in, fp_on_png = work / "fp_on.coree", work / "fp_on.png"
    _write_coree_from_doc(fp_on_in, "Facing On", fp_doc)
    r_fpon = run_cmd([str(ctx.cli_bin), "render", str(fp_on_in), str(fp_on_png), "--dpi", "48", "--multipage"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_fpoff.returncode != 0 or r_fpon.returncode != 0 or not fp_off_png.is_file() or not fp_on_png.is_file():
        return TestOutcome("FAIL", f"W5 FEAT-10: facing_pages multipage render failed: {r_fpon.stderr or r_fpoff.stderr}")
    try:
        if parse_png_info(fp_on_png.read_bytes())["non_white_pixels"] <= 20:
            return TestOutcome("FAIL", "W5 FEAT-10: facing_pages multipage render blank")
        if compare_png_pixels(fp_off_png, fp_on_png)["mean_abs_diff"] <= 0.2:
            return TestOutcome("FAIL", "W5 FEAT-10: facing_pages=True produced identical multipage output to facing_pages=False")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 FEAT-10: facing_pages PNG invalid: {exc}")

    bg_doc = _minimal_doc_dict("Printable Bg BVA")
    bg_doc["pages"][0]["printable_background"] = True
    bg_doc["pages"][0]["background_fill"] = {"Solid": {"Rgba": {"r": 0.12, "g": 0.50, "b": 0.75, "a": 1.0}}}
    bg_in = work / "printable_bg.coree"
    bg_png = work / "printable_bg.png"
    _write_coree_from_doc(bg_in, "Printable Bg BVA", bg_doc)
    r_bg = run_cmd([str(ctx.cli_bin), "render", str(bg_in), str(bg_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    crash = _assert_no_panic_or_crash(r_bg, "W5 render printable_background")
    if crash:
        return crash
    if r_bg.returncode != 0 or not bg_png.is_file():
        return TestOutcome("FAIL", f"W5 REQ-12: render printable_background failed: {r_bg.stderr}")
    try:
        bg_info = parse_png_info(bg_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 REQ-12: printable_background PNG invalid: {exc}")
    if bg_info["corner_pixel"][:3] == (255, 255, 255):
        return TestOutcome("FAIL", f"W5 REQ-12: printable_background=True left white corner pixel: {bg_info}")

    for pkg, filt, lbl, min_p, req_rx in (
        ("coree-core", "drawing_scale", "W5 ADD-07 coree-core drawing_scale", 1, None),
        ("coree-render", "drawing_scale", "W5 ADD-07 coree-render drawing_scale", 1, None),
        ("coree-render", "snap", "W5 FEAT-04 5 geometric snaps", 3, None),
        ("coree-core", "autofit", "W5 FEAT-10 coree-core autofit", 2, None),
        ("coree-app", "autofit", "W5 FEAT-10 coree-app autofit", 3, None),
        ("coree-app", "navigator", "W5 FEAT-13 navigator", 1, None),
        ("coree-app", "reorder", "W5 FEAT-08 S03 ReorderPages", 1, r"reorder.*page|page.*reorder"),
    ):
        fail_w5 = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], lbl, min_passed=min_p, timeout=120.0, require_any_rx=req_rx)
        if fail_w5:
            return fail_w5
    return TestOutcome("PASS", "W5 Rulers/Guides/5-Snaps/6-Views/Navigator/FacingPages/Autofit/ReorderPages/PrintPreviewDialog/S15 CP1256/S16 Hex verified")


def _test_w5_i18n_rtl_shell_mirroring(ctx: RunContext) -> TestOutcome:
    app_dir = ctx.repo_root / "crates" / "coree-app" / "src"
    app_src = "\n".join(
        _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
        for p in sorted(app_dir.rglob("*.rs"))
    ) if app_dir.is_dir() else ""
    ui_dir = app_dir / "ui"
    ui_src = "\n".join(
        _strip_cfg_test_modules(strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
        for p in sorted(ui_dir.rglob("*.rs"))
    ) if ui_dir.is_dir() else ""
    app_rs_src = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-app/src/app.rs"))

    if "I18nKey" not in app_src:
        return TestOutcome("FAIL", "W5 IMP-10: typed enum I18nKey missing in crates/coree-app/src/")
    if "ShellLayoutDirections" not in app_src:
        return TestOutcome("FAIL", "W5 IMP-10: struct ShellLayoutDirections RTL mirroring missing in crates/coree-app/src/")
    if "ShellLayoutDirections" not in (ui_src + "\n" + app_rs_src):
        return TestOutcome("FAIL", "W5 IMP-10: ShellLayoutDirections is never referenced in crates/coree-app/src/ui/ or app.rs")
    if "I18nKey::" not in (ui_src + "\n" + app_rs_src):
        return TestOutcome("FAIL", "W5 IMP-10: I18nKey is never used in crates/coree-app/src/ui/ or app.rs")

    i18n_block = _extract_brace_block(app_src, r"\benum\s+I18nKey\b")
    variants = re.findall(r"^\s*([A-Z][A-Za-z0-9_]*)\s*,", i18n_block or "", re.M)
    if len(variants) < 488:
        return TestOutcome("FAIL", f"W5 IMP-10: I18nKey has {len(variants)} variants (< 488 required)")

    fail_i18n = _run_cargo_tests(ctx, ["-p", "coree-app", "--", "i18n"], "W5 IMP-10 i18n/RTL", min_passed=1, timeout=180.0)
    if fail_i18n:
        return fail_i18n
    return TestOutcome("PASS", f"W5 I18nKey ({len(variants)} EN/AR variants), ShellLayoutDirections RTL mirroring & text_focus shortcut gating verified")


# ---------------------------------------------------------------------------
# Tier 2 Test Registry (All 67/67 T2 FEAT-* + EXT-F03/04/05/07/10)
# ---------------------------------------------------------------------------

def get_tests() -> list[TestCase]:
    return [
        # --- Baseline (smoke=True) ---
        TestCase(
            id="T2-BASE-CLI-BVA-NUM",
            tier=2,
            title="CLI numeric/flag BVA (0, min, max, max+1, neg, NaN, Inf) with 0 panics",
            roadmap_ids=("REQ-04", "REQ-07", "IMP-05", "IMP-07", "REM-09"),
            feature_refs=("FEAT-07", "FEAT-77", "FEAT-83", "FEAT-84", "FEAT-85", "FEAT-86", "EXT-F07"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_cli_bva_numeric,
        ),
        TestCase(
            id="T2-BASE-CORRUPT-HEADERS",
            tier=2,
            title="0-byte (17 exts), truncated RIFF/CMX/BMP, encrypted PDF, bad PSD/PNG/CPT rejection",
            roadmap_ids=("MND-04", "REQ-06", "REM-07"),
            feature_refs=("FEAT-81", "FEAT-82", "FEAT-83", "FEAT-84", "FEAT-85", "FEAT-86", "EXT-F04"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_corrupt_headers,
        ),
        TestCase(
            id="T2-BASE-ADVERSARIAL-ARCHIVE-XML",
            tier=2,
            title="Zip-Slip, Zip-Bomb (ratio & 528M), Zlib-Bomb, depth-200, 65k DIB & XML DTD/depth bombs",
            roadmap_ids=("MND-04", "REQ-06", "REQ-04"),
            feature_refs=("FEAT-81", "FEAT-84", "FEAT-86", "EXT-F03"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_adversarial_archive_xml,
        ),
        TestCase(
            id="T2-BASE-ARABIC-CP1256-EDGE",
            tier=2,
            title="CP1256 Arabic (explicit+heuristic+0x81/0x8D/0xFF), RTL Kashida/ZW & Print Merge CSV BVA",
            roadmap_ids=("REQ-08", "IMP-07", "IMP-10"),
            feature_refs=("FEAT-38", "FEAT-78", "FEAT-81", "EXT-F05"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_arabic_cp1256_edge,
        ),
        TestCase(
            id="T2-BASE-JSON-COREE-NAN-CYCLE",
            tier=2,
            title="0x0/1-pt path, PowerClip/Group cycle, 0-page/NaN JSON & Arabic UTF-8 file paths",
            roadmap_ids=("REQ-01", "MND-04"),
            feature_refs=("FEAT-07", "FEAT-21", "FEAT-22", "FEAT-24", "FEAT-31", "FEAT-35", "FEAT-42"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_json_coree_nan_cycle,
        ),

        # --- Wave 1 (smoke=False) ---
        TestCase(
            id="T2-W1-TOOLCORE-INTERACTION",
            tier=2,
            title="W1 MND-02: ToolCore headless pointer/snap/pivot/InteractiveFill/Eyedropper/Connector",
            roadmap_ids=("MND-02", "IMP-11"),
            feature_refs=("FEAT-12", "FEAT-34", "FEAT-02", "FEAT-21", "FEAT-22", "FEAT-23", "FEAT-29"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_toolcore_interaction,
        ),
        TestCase(
            id="T2-W1-COMMAND-DELTA-BVA",
            tier=2,
            title="W1 REQ-01: CommandOp/DocumentDelta BVA, ReorderPages OOB, FitPowerClip & FindAndReplace",
            roadmap_ids=("REQ-01", "IMP-11"),
            feature_refs=("FEAT-07", "FEAT-08", "FEAT-14", "FEAT-37", "FEAT-42"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_command_delta_bva,
        ),
        TestCase(
            id="T2-W1-IPC-UDS-CLIPBOARD",
            tier=2,
            title="W1 REQ-09/10: UDS 0600 JSON-RPC IPC boundary & Live Vector Clipboard (SVG/PDF/EMF)",
            roadmap_ids=("REQ-09", "REQ-10"),
            feature_refs=("FEAT-01", "FEAT-06", "EXT-F01"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_ipc_uds_clipboard,
        ),
        TestCase(
            id="T2-W1-SQLITE-QUARANTINE-WAL",
            tier=2,
            title="W1 IMP-07: Corrupt SQLite DB quarantine, SHA-256 snapshot & differential WAL recovery",
            roadmap_ids=("IMP-07",),
            feature_refs=("FEAT-01", "FEAT-78", "EXT-F10"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_sqlite_quarantine_wal,
        ),

        # --- Wave 2 (smoke=False) ---
        TestCase(
            id="T2-W2-CDRCRV-GEOMETRY-BVA",
            tier=2,
            title="W2 REM-01/03/10/IMP-05: Analytical erase_circle, knife, continuity, Boolean & DCEL SmartFill",
            roadmap_ids=("REM-01", "REM-03", "REM-10", "IMP-05"),
            feature_refs=("FEAT-15", "FEAT-17", "FEAT-24", "FEAT-25", "FEAT-35"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_cdrcrv_geometry_bva,
        ),
        TestCase(
            id="T2-W2-DRAWING-MODES-MASTER",
            tier=2,
            title="W2 ADD-01..04/MND-03: BSpline, ArtisticMedia, Clone, MasterLayer, Symmetry, Perspective, Table, Dim",
            roadmap_ids=("ADD-01", "ADD-02", "ADD-03", "ADD-04", "REM-03", "MND-03"),
            feature_refs=("FEAT-09", "FEAT-18", "FEAT-19", "FEAT-20", "FEAT-26", "FEAT-27", "FEAT-28", "FEAT-30", "FEAT-42"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_drawing_modes_master,
        ),
        TestCase(
            id="T2-W2-CDR-CODEC-BOUNDARY",
            tier=2,
            title="W2 MND-03/REQ-06/REM-05..07: ZCF OOB ptr/XML entity, cyclic cdss, XDOC/XOBJ, CMX, CPT, CDT",
            roadmap_ids=("MND-03", "REQ-06", "REM-05", "REM-06", "REM-07"),
            feature_refs=("FEAT-11", "FEAT-81", "FEAT-82", "EXT-F03", "EXT-F04"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_cdr_codec_boundary,
        ),

        # --- Wave 3 (smoke=False) ---
        TestCase(
            id="T2-W3-COLOR-MESH-EFFECTS-LENS",
            tier=2,
            title="W3 REQ-02/ADD-01/IMP-04/REM-04: ICC limits, MeshFill, InnerShadow/Contour/Blend/Envelope/Extrude, Frozen Lens",
            roadmap_ids=("REQ-02", "ADD-01", "IMP-04", "REM-02", "REM-04"),
            feature_refs=("FEAT-31", "FEAT-32", "FEAT-33", "FEAT-43", "FEAT-45", "FEAT-46", "FEAT-48", "FEAT-49", "FEAT-63"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_color_mesh_effects_lens,
        ),
        TestCase(
            id="T2-W3-TYPOGRAPHY-STYLUS-SCULPT",
            tier=2,
            title="W3 ADD-10/IMP-03/REQ-07: Font quarantine & Glyphs (FEAT-41), linked frames, TextOnPath, Sculpt, Stylus",
            roadmap_ids=("ADD-10", "IMP-03", "REQ-07", "IMP-10", "REM-02"),
            feature_refs=("FEAT-41", "FEAT-16", "FEAT-20", "FEAT-38", "FEAT-39", "FEAT-40"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_typography_stylus_sculpt,
        ),
        TestCase(
            id="T2-W3-PHOTO-STUDIO-BOUNDARY",
            tier=2,
            title="W3 IMP-06/ADD-12: 16-bit CoW Photo Studio, Pixel Layers, Masks, 8 Brushes, ChannelMixer & Resample",
            roadmap_ids=("IMP-06", "ADD-12", "REQ-02"),
            feature_refs=("FEAT-51", "FEAT-64", "FEAT-65", "FEAT-66", "FEAT-67", "FEAT-68", "FEAT-69"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_photo_studio_boundary,
        ),

        # --- Wave 4 (smoke=False) ---
        TestCase(
            id="T2-W4-PREPRESS-IMPOSITION-BARCODE",
            tier=2,
            title="W4 REQ-03..07/10/ADD-05: Imposition, 2D Barcodes, FEFCO, Preflight/Collect, CUPS IPP & PDF",
            roadmap_ids=("REQ-03", "REQ-04", "REQ-05", "REQ-07", "REQ-10", "ADD-05"),
            feature_refs=("FEAT-73", "FEAT-74", "FEAT-77", "FEAT-78", "FEAT-79", "FEAT-80", "FEAT-83"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_prepress_imposition_barcode,
        ),
        TestCase(
            id="T2-W4-EXCHANGE-DWG-EMF-PSD-BVA",
            tier=2,
            title="W4 ADD-06/REM-08/MND-04: DWG CRC, EMF/WMF/ACB bounds, >2048px/RLE PSD, DXF/EPS/PLT",
            roadmap_ids=("ADD-06", "REM-08", "MND-04"),
            feature_refs=("FEAT-37", "FEAT-84", "FEAT-85", "FEAT-86"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_exchange_dwg_emf_psd_bva,
        ),
        TestCase(
            id="T2-W4-CQL-RHAI-PLUGIN-SANDBOX",
            tier=2,
            title="W4 ADD-11/REQ-01: CQL syntax/OOB, Rhai max_operations sandbox & JSON-RPC plugin limits",
            roadmap_ids=("ADD-11", "REQ-01"),
            feature_refs=("FEAT-14", "EXT-F11"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_cql_rhai_plugin_sandbox,
        ),

        # --- Wave 5 (smoke=False) ---
        TestCase(
            id="T2-W5-WELCOME-DOCKERS-PRESETS",
            tier=2,
            title="W5 REQ-12/ADD-09/IMP-08..12: S01 Welcome/Tabs (FEAT-01), 14 Dockers (FEAT-03), 65/65, Tint/Drag, Illustrator",
            roadmap_ids=("REQ-12", "ADD-09", "IMP-08", "IMP-09", "IMP-10", "IMP-11", "IMP-12"),
            feature_refs=("FEAT-01", "FEAT-03", "FEAT-02", "FEAT-06", "FEAT-37"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_welcome_dockers_presets,
        ),
        TestCase(
            id="T2-W5-POWERTRACE-INTERACTIVE",
            tier=2,
            title="W5 IMP-05: PowerTRACE S11 interactive sliders, SplitPreview & >16MP clamp (FEAT-72)",
            roadmap_ids=("IMP-05",),
            feature_refs=("FEAT-72",),
            target_wave="W5",
            smoke=False,
            run=_test_w5_powertrace_interactive,
        ),
        TestCase(
            id="T2-W5-VIEW-GUIDES-NAVIGATOR-PRINTUI",
            tier=2,
            title="W5 ADD-07/08/IMP-01/02/09/REQ-08/12: Rulers/Guides/6-Views/Navigator/Facing/PrintPreview/S15/S16",
            roadmap_ids=("ADD-07", "ADD-08", "IMP-01", "IMP-02", "IMP-09", "REQ-08", "REQ-12"),
            feature_refs=("FEAT-04", "FEAT-05", "FEAT-08", "FEAT-10", "FEAT-11", "FEAT-13", "FEAT-73", "EXT-F04"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_view_guides_navigator_printui,
        ),
        TestCase(
            id="T2-W5-I18N-RTL-SHELL-MIRRORING",
            tier=2,
            title="W5 IMP-10/REQ-11: 488+ I18nKey, ShellLayoutDirections RTL mirroring, text_focus & packaging",
            roadmap_ids=("IMP-10", "REQ-11"),
            feature_refs=("FEAT-01", "FEAT-02", "FEAT-03", "FEAT-38", "EXT-F05"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_i18n_rtl_shell_mirroring,
        ),
    ]
