"""Tier 1: CLI, Codec Round-Trip, External Oracles & Workspace Hygiene."""

from __future__ import annotations

import base64
import json
import math
import os
import re
import struct
import subprocess
import sys
import tomllib
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Any, Optional

from e2e_suite.harness import (
    RunContext,
    SubprocessResult,
    TestCase,
    TestOutcome,
    _ZIP_MAX_TOTAL_BYTES,
    _bounded_zip_read_info,
    _decode_png_rgba_bytes_stdlib,
    _encode_png_rgba_stdlib,
    _validate_zip_infolist,
    build_blank_white_png,
    build_cdr_riff_with_unknown_xdoc,
    build_coree_bytes,
    compare_png_pixels,
    crop_png_center,
    dissect_acb,
    dissect_cdr_riff,
    dissect_cdr_zcf,
    dissect_dwg,
    dissect_emf,
    dissect_psd,
    dissect_wmf,
    inspect_coree_zip,
    parse_dxf_structure,
    parse_pdf_structure,
    parse_plt_commands,
    parse_png_info,
    parse_svg_structure,
    require_cli_subcommand,
    run_cmd,
    run_file_magic,
    run_ghostscript_render,
    run_pdftoppm,
    run_poppler_fonts,
    run_poppler_info,
    run_poppler_text,
    strip_rust_comments_and_strings,
    strip_rust_comments_only,
)

TIER = 1
TIER_NAME = "CLI, Codec Round-Trip & Hygiene"

_BASELINE_SUBCOMMANDS = frozenset({
    "generate-sample",
    "inspect",
    "convert",
    "render",
    "thumbnail",
    "laser",
    "preflight",
    "print-merge",
    "collect",
    "trace",
    "run-macro",
    "help",
})


def _f(ctx: RunContext, name: str) -> Path:
    return ctx.fixtures_dir / "tier1" / name


def _read_crates_text(ctx: RunContext, rel: str) -> str:
    p = ctx.repo_root / rel
    return p.read_text(encoding="utf-8", errors="replace") if p.is_file() else ""


def _read_rust_stripped(ctx: RunContext, rel: str) -> str:
    p = ctx.repo_root / rel
    files: list[Path] = []
    if p.is_file():
        files.append(p)
    # Support IMP-11 modularized directory layout (e.g. ui/canvas.rs + ui/canvas/*.rs)
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


def _read_rust_comments_only(ctx: RunContext, rel: str) -> str:
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
        strip_rust_comments_only(f.read_text(encoding="utf-8", errors="replace"))
        for f in files
    )


def _extract_all_brace_blocks_with_spans(
    stripped_code: str,
    header_rx: str,
) -> list[tuple[re.Match[str], int, int, str]]:
    """Extract all `(match, body_start, body_end, brace_body)` tuples following `header_rx` in stripped Rust source."""
    n = len(stripped_code)
    results: list[tuple[re.Match[str], int, int, str]] = []
    for m in re.finditer(header_rx, stripped_code):
        i = m.end() - 1 if m.group(0).endswith("{") else m.end()
        paren = 0
        bracket = 0
        aborted_semi = False
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
                if depth != 0:
                    break
                k = j
                while k < n and stripped_code[k].isspace():
                    k += 1
                if k < n and (
                    stripped_code[k] in (">", ",", "+", "-", "*", "%", "&", "|", "^", "<", "=")
                    or (stripped_code[k] == "/" and stripped_code[k:k + 2] not in ("//", "/*"))
                    or stripped_code[k:k + 2] == "::"
                ):
                    i = j
                    continue
                results.append((m, i, j, stripped_code[i:j]))
                break
            elif ch == ";" and paren == 0 and bracket == 0:
                aborted_semi = True
                break
            i += 1
        if aborted_semi:
            continue
    return results


def _extract_all_brace_blocks(stripped_code: str, header_rx: str) -> list[tuple[re.Match[str], str]]:
    """Extract all `(match, brace_body)` pairs following `header_rx` in stripped Rust source."""
    return [(m, body) for m, _s, _e, body in _extract_all_brace_blocks_with_spans(stripped_code, header_rx)]


def _extract_brace_block(stripped_code: str, header_rx: str) -> Optional[str]:
    """Extract the brace-balanced `{ ... }` block following `header_rx` in stripped Rust source."""
    blocks = _extract_all_brace_blocks(stripped_code, header_rx)
    return blocks[0][1] if blocks else None


_CLI_PARSE_EXCEPTIONS = (
    ValueError,
    KeyError,
    zipfile.BadZipFile,
    json.JSONDecodeError,
    ET.ParseError,
    UnicodeDecodeError,
    OSError,
)


def _require_dict(val: Any, context: str) -> dict[str, Any]:
    if not isinstance(val, dict):
        raise ValueError(f"{context}: expected JSON object (dict), got {type(val).__name__}")
    return val


def _compare_same_page_pngs(
    png_a: Path,
    png_b: Path,
    context: str,
    *,
    min_non_white_a: int = 20,
    min_non_white_b: int = 20,
) -> dict[str, Any]:
    if min_non_white_a > 0:
        info_a = parse_png_info(png_a.read_bytes())
        if info_a["non_white_pixels"] < min_non_white_a:
            raise ValueError(f"{context}: baseline/control PNG {png_a.name} blank/degenerate ({info_a})")
    if min_non_white_b > 0:
        info_b = parse_png_info(png_b.read_bytes())
        if info_b["non_white_pixels"] < min_non_white_b:
            raise ValueError(f"{context}: rendered PNG {png_b.name} blank/degenerate ({info_b})")
    diff = compare_png_pixels(png_a, png_b)
    if not diff.get("size_match"):
        raise ValueError(f"{context}: PNG dimension mismatch {diff.get('size_a')} vs {diff.get('size_b')}")
    return diff


def _strip_not_clauses(expr: str) -> str:
    """Remove balanced `not(...)` subexpressions from a Rust `cfg(...)` predicate."""
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
    """Remove `#[cfg(...test...)]` gated `mod`/`fn`/`impl` blocks from stripped Rust source (preserving `#[cfg_attr(test, ...)]` production items)."""
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


_DUMMY_LET_PREFIX_RX = re.compile(
    r"^\s*let\s+(?:_[A-Za-z0-9_]*|\(\s*_[A-Za-z0-9_]*(?:\s*,\s*_[A-Za-z0-9_]*)+\s*\))\s*"
)


def _strip_leading_dummy_lets(inner: str) -> str:
    """Strip leading `let _... = <non-call rhs>;` statements (including balanced tuple/grouped RHS)."""
    cur = inner
    while True:
        m = _DUMMY_LET_PREFIX_RX.match(cur)
        if not m:
            break
        n = len(cur)
        i = m.end()
        t_p = 0
        t_b = 0
        eq_pos = -1
        while i < n:
            ch = cur[i]
            if ch in ";{}":
                break
            if ch == "(":
                t_p += 1
            elif ch == ")":
                t_p = max(0, t_p - 1)
            elif ch == "[":
                t_b += 1
            elif ch == "]":
                t_b = max(0, t_b - 1)
            elif ch == "=" and t_p == 0 and t_b == 0:
                if i + 1 < n and cur[i + 1] in "=>":
                    break
                eq_pos = i
                break
            i += 1
        if eq_pos < 0:
            break
        rhs_start = eq_pos + 1
        j = rhs_start
        p_depth = 0
        b_depth = 0
        rhs_end = -1
        bad_rhs = False
        while j < n:
            ch = cur[j]
            if ch in "{}":
                bad_rhs = True
                break
            if ch == ";":
                if p_depth == 0 and b_depth == 0:
                    rhs_end = j
                    break
                bad_rhs = True
                break
            if ch == "(":
                k = j - 1
                while k >= rhs_start and cur[k].isspace():
                    k -= 1
                if k >= rhs_start and (cur[k].isalnum() or cur[k] in "_!:)]"):
                    bad_rhs = True
                    break
                p_depth += 1
            elif ch == ")":
                p_depth -= 1
                if p_depth < 0:
                    bad_rhs = True
                    break
            elif ch == "[":
                k = j - 1
                while k >= rhs_start and cur[k].isspace():
                    k -= 1
                if k >= rhs_start and cur[k] == "!":
                    bad_rhs = True
                    break
                b_depth += 1
            elif ch == "]":
                b_depth -= 1
                if b_depth < 0:
                    bad_rhs = True
                    break
            j += 1
        if bad_rhs or rhs_end < 0 or p_depth != 0 or b_depth != 0:
            break
        if not cur[rhs_start:rhs_end].strip():
            break
        cur = cur[rhs_end + 1:]
    return cur


def _is_trivial_stub_body(body: str) -> bool:
    """Return True if a Rust function body is empty, `todo!`/`unimplemented!`, or a trivial empty return."""
    inner = body.strip("{} \t\r\n")
    cleaned = _strip_leading_dummy_lets(inner).strip()
    if len(cleaned) <= 10 and "(" not in cleaned and "!" not in cleaned:
        return True
    if re.search(r"\b(?:todo|unimplemented)\s*!", cleaned):
        return True
    if re.fullmatch(
        r"(?:return\s+)?(?:Ok\s*\(\s*)?(?:Vec::new\s*\(\s*\)|vec!\s*\[\s*\]|String::new\s*\(\s*\)|\(\s*\))(?:\s*\))?\s*;?",
        cleaned,
    ):
        return True
    return False


def _extract_match_arm_bodies(code: str, variant_rx: str) -> list[str]:
    """Extract brace-balanced or comma-delimited match-arm bodies for `variant_rx`."""
    arms: dict[tuple[int, int], str] = {}
    n = len(code)
    cur = 0
    base_paren = 0
    base_bracket = 0
    pk_stack: list[tuple[int, int]] = []
    for m in re.finditer(rf"(?:{variant_rx})\b", code):
        while cur < m.start():
            c = code[cur]
            if c == "{":
                pk_stack.append((base_paren, base_bracket))
                base_paren = 0
                base_bracket = 0
            elif c == "}":
                if pk_stack:
                    base_paren, base_bracket = pk_stack.pop()
                else:
                    base_paren = 0
                    base_bracket = 0
            elif c == "(":
                base_paren += 1
            elif c == ")":
                base_paren = max(0, base_paren - 1)
            elif c == "[":
                base_bracket += 1
            elif c == "]":
                base_bracket = max(0, base_bracket - 1)
            cur += 1
        i = m.end()
        brace = 0
        paren = base_paren
        bracket = base_bracket
        arrow_end = -1
        while i < n:
            ch = code[i]
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren = max(0, paren - 1)
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket = max(0, bracket - 1)
            elif ch == "{":
                brace += 1
            elif ch == "}":
                brace -= 1
                if brace < 0:
                    break
            elif brace == 0 and paren == 0 and bracket == 0:
                if ch in (";", ","):
                    break
                if ch == "=":
                    if i + 1 < n and code[i + 1] == ">":
                        arrow_end = i + 2
                        break
                    prev_ch = code[i - 1] if i > 0 else ""
                    next_ch = code[i + 1] if i + 1 < n else ""
                    if prev_ch not in ("=", "!", "<", ">") and next_ch != "=":
                        if not re.search(r"(?:\bif|\&\&)\s+let\b[^=;]*$", code[m.end():i]):
                            break
            i += 1
        if arrow_end < 0:
            continue
        j = arrow_end
        while j < n and code[j].isspace():
            j += 1
        if j >= n:
            continue
        if code[j] == "{":
            d = 1
            k = j + 1
            while k < n and d > 0:
                if code[k] == "{":
                    d += 1
                elif code[k] == "}":
                    d -= 1
                k += 1
            arms[(j, k)] = code[j:k]
        else:
            d_b = d_p = d_k = 0
            k = j
            while k < n:
                ch = code[k]
                if ch == "{":
                    d_b += 1
                elif ch == "}":
                    if d_b == 0 and d_p == 0 and d_k == 0:
                        break
                    d_b -= 1
                elif ch == "(":
                    d_p += 1
                elif ch == ")":
                    d_p -= 1
                elif ch == "[":
                    d_k += 1
                elif ch == "]":
                    d_k -= 1
                elif ch == "," and d_b == 0 and d_p == 0 and d_k == 0:
                    break
                k += 1
            arms[(j, k)] = code[j:k]
    return list(arms.values())


def _has_empty_match_arm(code: str, variant_rx: str) -> bool:
    """Return True if any match arm matching `variant_rx` is an empty/return/continue no-op."""
    pat = (
        rf"(?:{variant_rx})\b"
        r"(?:(?!=>|;|\bmatch\b|(?<![=!<>])=(?![=>]))[\s\S]){0,300}?=>\s*"
        r"(?:\{\s*(?:(?:return|continue)\s*;?\s*)?\}|\(\s*\)|(?:return|continue)\b)"
    )
    return bool(re.search(pat, code))


_SVG_NUM_RX = re.compile(
    r"[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?"
)


def _svg_arc_points(
    x1: float,
    y1: float,
    rx: float,
    ry: float,
    x_axis_rotation_deg: float,
    large_arc_flag: float,
    sweep_flag: float,
    x2: float,
    y2: float,
) -> list[tuple[float, float]]:
    """Convert an SVG elliptical arc (F.6.5/F.6.6 center parameterization) into sampled points."""
    if abs(x2 - x1) < 1e-12 and abs(y2 - y1) < 1e-12:
        return []
    rx = abs(rx)
    ry = abs(ry)
    if rx <= 1e-12 or ry <= 1e-12:
        return [(x2, y2)]

    phi = math.radians(x_axis_rotation_deg % 360.0)
    cos_phi = math.cos(phi)
    sin_phi = math.sin(phi)
    dx2 = 0.5 * (x1 - x2)
    dy2 = 0.5 * (y1 - y2)
    x1p = cos_phi * dx2 + sin_phi * dy2
    y1p = -sin_phi * dx2 + cos_phi * dy2

    lam = (x1p * x1p) / (rx * rx) + (y1p * y1p) / (ry * ry)
    if lam > 1.0:
        scale = math.sqrt(lam)
        rx *= scale
        ry *= scale

    rx2 = rx * rx
    ry2 = ry * ry
    x1p2 = x1p * x1p
    y1p2 = y1p * y1p
    denom = rx2 * y1p2 + ry2 * x1p2
    if denom <= 1e-24:
        return [(x2, y2)]

    num = max(0.0, rx2 * ry2 - rx2 * y1p2 - ry2 * x1p2)
    factor = math.sqrt(num / denom)
    fa = 1 if large_arc_flag != 0.0 else 0
    fs = 1 if sweep_flag != 0.0 else 0
    if fa == fs:
        factor = -factor
    cxp = factor * (rx * y1p) / ry
    cyp = factor * -(ry * x1p) / rx

    cx_c = cos_phi * cxp - sin_phi * cyp + 0.5 * (x1 + x2)
    cy_c = sin_phi * cxp + cos_phi * cyp + 0.5 * (y1 + y2)

    def _vec_angle(ux: float, uy: float, vx: float, vy: float) -> float:
        len_uv = math.hypot(ux, uy) * math.hypot(vx, vy)
        if len_uv <= 1e-24:
            return 0.0
        ratio = max(-1.0, min(1.0, (ux * vx + uy * vy) / len_uv))
        ang = math.acos(ratio)
        if (ux * vy - uy * vx) < 0.0:
            ang = -ang
        return ang

    vx1 = (x1p - cxp) / rx
    vy1 = (y1p - cyp) / ry
    vx2 = (-x1p - cxp) / rx
    vy2 = (-y1p - cyp) / ry
    theta1 = _vec_angle(1.0, 0.0, vx1, vy1)
    dtheta = _vec_angle(vx1, vy1, vx2, vy2)
    if fs == 0 and dtheta > 0.0:
        dtheta -= 2.0 * math.pi
    elif fs == 1 and dtheta < 0.0:
        dtheta += 2.0 * math.pi

    steps = max(4, int(math.ceil(abs(dtheta) / (math.pi / 8.0))))
    pts: list[tuple[float, float]] = []
    for step_i in range(1, steps + 1):
        if step_i == steps:
            pts.append((x2, y2))
        else:
            ang = theta1 + dtheta * (step_i / steps)
            ex = rx * math.cos(ang)
            ey = ry * math.sin(ang)
            px = cos_phi * ex - sin_phi * ey + cx_c
            py = sin_phi * ex + cos_phi * ey + cy_c
            pts.append((px, py))
    return pts


def _parse_svg_path_subpaths_with_closed(
    d_attr: str,
    min_points: int = 3,
) -> list[tuple[list[tuple[float, float]], bool]]:
    """Command-aware SVG path parser across M/m/L/l/H/h/V/v/C/c/S/s/Q/q/T/t/A/a/Z/z with per-subpath closedness."""
    s = d_attr.strip()
    if not s:
        return []
    n = len(s)

    def _skip_sep(p: int) -> int:
        while p < n and (s[p].isspace() or s[p] == ","):
            p += 1
        return p

    def _read_num(p: int, cmd_ch: str) -> tuple[float, int]:
        p = _skip_sep(p)
        if p >= n:
            raise ValueError(f"SVG '{cmd_ch}' command missing expected number at offset {p}")
        nm = _SVG_NUM_RX.match(s, p)
        if not nm:
            raise ValueError(f"Malformed SVG path number at offset {p}: {s[p:p + 24]!r}")
        return float(nm.group(0)), nm.end()

    def _read_flag(p: int, cmd_ch: str) -> tuple[float, int]:
        p = _skip_sep(p)
        if p >= n or s[p] not in ("0", "1"):
            raise ValueError(f"Malformed SVG '{cmd_ch}' arc flag at offset {p}: {s[p:p + 24]!r}")
        return (1.0 if s[p] == "1" else 0.0), p + 1

    polys: list[tuple[list[tuple[float, float]], bool]] = []
    cur: list[tuple[float, float]] = []
    cx = cy = 0.0
    sx = sy = 0.0
    last_cx2 = last_cy2 = 0.0
    last_qx1 = last_qy1 = 0.0
    prev_cmd = ""
    has_moveto = False
    pos = 0
    while pos < n:
        pos = _skip_sep(pos)
        if pos >= n:
            break
        if not s[pos].isalpha():
            raise ValueError(f"Expected SVG path command at offset {pos}: {s[pos:pos + 24]!r}")
        cmd = s[pos]
        pos += 1
        nums: list[float] = []
        if cmd in ("A", "a"):
            while True:
                pos = _skip_sep(pos)
                if pos >= n or s[pos].isalpha():
                    break
                rx, pos = _read_num(pos, cmd)
                ry, pos = _read_num(pos, cmd)
                x_rot, pos = _read_num(pos, cmd)
                fa, pos = _read_flag(pos, cmd)
                fs, pos = _read_flag(pos, cmd)
                x2, pos = _read_num(pos, cmd)
                y2, pos = _read_num(pos, cmd)
                nums.extend((rx, ry, x_rot, fa, fs, x2, y2))
        else:
            while True:
                pos = _skip_sep(pos)
                if pos >= n or s[pos].isalpha():
                    break
                val, pos = _read_num(pos, cmd)
                nums.append(val)

        if cmd in ("M", "m"):
            if len(nums) < 2 or len(nums) % 2 != 0:
                raise ValueError(f"SVG '{cmd}' command requires even >=2 coordinates, got {len(nums)}")
            if len(cur) >= min_points:
                polys.append((cur, False))
            rel = (cmd == "m")
            for p_i in range(0, len(nums), 2):
                nx, ny = nums[p_i], nums[p_i + 1]
                if rel:
                    cx += nx
                    cy += ny
                else:
                    cx, cy = nx, ny
                if p_i == 0:
                    sx, sy = cx, cy
                    has_moveto = True
                    cur = [(cx, cy)]
                else:
                    cur.append((cx, cy))
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = ("L" if cmd == "M" else "l") if len(nums) > 2 else cmd
            continue

        if not has_moveto:
            raise ValueError(f"SVG path command '{cmd}' encountered before initial M/m")

        if cmd in ("L", "l"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 2 or len(nums) % 2 != 0:
                raise ValueError(f"SVG '{cmd}' command requires even >=2 coordinates, got {len(nums)}")
            rel = (cmd == "l")
            for p_i in range(0, len(nums), 2):
                nx, ny = nums[p_i], nums[p_i + 1]
                if rel:
                    cx += nx
                    cy += ny
                else:
                    cx, cy = nx, ny
                cur.append((cx, cy))
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = cmd
        elif cmd in ("H", "h"):
            if not cur:
                cur = [(cx, cy)]
            if not nums:
                raise ValueError(f"SVG '{cmd}' command requires >=1 x coordinate")
            rel = (cmd == "h")
            for nx in nums:
                cx = (cx + nx) if rel else nx
                cur.append((cx, cy))
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = cmd
        elif cmd in ("V", "v"):
            if not cur:
                cur = [(cx, cy)]
            if not nums:
                raise ValueError(f"SVG '{cmd}' command requires >=1 y coordinate")
            rel = (cmd == "v")
            for ny in nums:
                cy = (cy + ny) if rel else ny
                cur.append((cx, cy))
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = cmd
        elif cmd in ("C", "c"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 6 or len(nums) % 6 != 0:
                raise ValueError(f"SVG '{cmd}' command requires 6*k coordinates, got {len(nums)}")
            rel = (cmd == "c")
            for p_i in range(0, len(nums), 6):
                x0, y0 = cx, cy
                x1, y1, x2, y2, x3, y3 = nums[p_i:p_i + 6]
                if rel:
                    x1 += x0
                    y1 += y0
                    x2 += x0
                    y2 += y0
                    x3 += x0
                    y3 += y0
                for t in (0.25, 0.5, 0.75, 1.0):
                    mt = 1.0 - t
                    px = (mt ** 3) * x0 + 3.0 * (mt ** 2) * t * x1 + 3.0 * mt * (t ** 2) * x2 + (t ** 3) * x3
                    py = (mt ** 3) * y0 + 3.0 * (mt ** 2) * t * y1 + 3.0 * mt * (t ** 2) * y2 + (t ** 3) * y3
                    cur.append((px, py))
                last_cx2, last_cy2 = x2, y2
                cx, cy = x3, y3
                prev_cmd = cmd
            last_qx1, last_qy1 = cx, cy
        elif cmd in ("S", "s"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 4 or len(nums) % 4 != 0:
                raise ValueError(f"SVG '{cmd}' command requires 4*k coordinates, got {len(nums)}")
            rel = (cmd == "s")
            for p_i in range(0, len(nums), 4):
                x0, y0 = cx, cy
                if prev_cmd in ("C", "c", "S", "s"):
                    x1, y1 = 2.0 * x0 - last_cx2, 2.0 * y0 - last_cy2
                else:
                    x1, y1 = x0, y0
                x2, y2, x3, y3 = nums[p_i:p_i + 4]
                if rel:
                    x2 += x0
                    y2 += y0
                    x3 += x0
                    y3 += y0
                for t in (0.25, 0.5, 0.75, 1.0):
                    mt = 1.0 - t
                    px = (mt ** 3) * x0 + 3.0 * (mt ** 2) * t * x1 + 3.0 * mt * (t ** 2) * x2 + (t ** 3) * x3
                    py = (mt ** 3) * y0 + 3.0 * (mt ** 2) * t * y1 + 3.0 * mt * (t ** 2) * y2 + (t ** 3) * y3
                    cur.append((px, py))
                last_cx2, last_cy2 = x2, y2
                cx, cy = x3, y3
                prev_cmd = cmd
            last_qx1, last_qy1 = cx, cy
        elif cmd in ("Q", "q"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 4 or len(nums) % 4 != 0:
                raise ValueError(f"SVG '{cmd}' command requires 4*k coordinates, got {len(nums)}")
            rel = (cmd == "q")
            for p_i in range(0, len(nums), 4):
                x0, y0 = cx, cy
                x1, y1, x2, y2 = nums[p_i:p_i + 4]
                if rel:
                    x1 += x0
                    y1 += y0
                    x2 += x0
                    y2 += y0
                for t in (0.25, 0.5, 0.75, 1.0):
                    mt = 1.0 - t
                    px = (mt ** 2) * x0 + 2.0 * mt * t * x1 + (t ** 2) * x2
                    py = (mt ** 2) * y0 + 2.0 * mt * t * y1 + (t ** 2) * y2
                    cur.append((px, py))
                last_qx1, last_qy1 = x1, y1
                cx, cy = x2, y2
                prev_cmd = cmd
            last_cx2, last_cy2 = cx, cy
        elif cmd in ("T", "t"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 2 or len(nums) % 2 != 0:
                raise ValueError(f"SVG '{cmd}' command requires 2*k coordinates, got {len(nums)}")
            rel = (cmd == "t")
            for p_i in range(0, len(nums), 2):
                x0, y0 = cx, cy
                if prev_cmd in ("Q", "q", "T", "t"):
                    x1, y1 = 2.0 * x0 - last_qx1, 2.0 * y0 - last_qy1
                else:
                    x1, y1 = x0, y0
                x2, y2 = nums[p_i:p_i + 2]
                if rel:
                    x2 += x0
                    y2 += y0
                for t in (0.25, 0.5, 0.75, 1.0):
                    mt = 1.0 - t
                    px = (mt ** 2) * x0 + 2.0 * mt * t * x1 + (t ** 2) * x2
                    py = (mt ** 2) * y0 + 2.0 * mt * t * y1 + (t ** 2) * y2
                    cur.append((px, py))
                last_qx1, last_qy1 = x1, y1
                cx, cy = x2, y2
                prev_cmd = cmd
            last_cx2, last_cy2 = cx, cy
        elif cmd in ("A", "a"):
            if not cur:
                cur = [(cx, cy)]
            if len(nums) < 7 or len(nums) % 7 != 0:
                raise ValueError(f"SVG '{cmd}' command requires 7*k parameters, got {len(nums)}")
            rel = (cmd == "a")
            for p_i in range(0, len(nums), 7):
                rx, ry, x_rot, large_arc, sweep, x2, y2 = nums[p_i:p_i + 7]
                if rel:
                    x2 += cx
                    y2 += cy
                arc_pts = _svg_arc_points(cx, cy, rx, ry, x_rot, large_arc, sweep, x2, y2)
                cur.extend(arc_pts)
                cx, cy = x2, y2
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = cmd
        elif cmd in ("Z", "z"):
            if nums:
                raise ValueError(f"SVG '{cmd}' command takes no coordinates, got {nums}")
            cx, cy = sx, sy
            if len(cur) >= min_points:
                polys.append((cur, True))
            cur = []
            last_cx2, last_cy2 = cx, cy
            last_qx1, last_qy1 = cx, cy
            prev_cmd = cmd
        else:
            raise ValueError(f"Unsupported SVG path command: {cmd!r}")

    if len(cur) >= min_points:
        polys.append((cur, False))
    return polys


def _parse_svg_path_subpaths(d_attr: str, min_points: int = 3) -> list[list[tuple[float, float]]]:
    """Command-aware SVG path parser across M/m/L/l/H/h/V/v/C/c/S/s/Q/q/T/t/A/a/Z/z."""
    return [pts for pts, _closed in _parse_svg_path_subpaths_with_closed(d_attr, min_points=min_points)]


_SVG_GEOM_TAGS = {"rect", "circle", "ellipse", "line", "polygon", "polyline", "path"}
_NUM_RX = re.compile(r"[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?")


def _transform_is_non_axis_aligned(tf_str: str) -> bool:
    """Return True iff SVG transform string introduces genuine non-axis-aligned rotation/skew/shear."""
    for m in re.finditer(r"([A-Za-z]+)\s*\(([^)]*)\)", tf_str):
        op = m.group(1).lower()
        args = [float(x) for x in _NUM_RX.findall(m.group(2))]
        if op == "rotate" and args:
            rem = abs(args[0]) % 90.0
            if rem > 1e-6 and abs(90.0 - rem) > 1e-6:
                return True
        elif op in ("skewx", "skewy") and args:
            rem = abs(args[0]) % 180.0
            if rem > 1e-6 and abs(180.0 - rem) > 1e-6:
                return True
        elif op == "matrix" and len(args) >= 6:
            a, b, c, d = args[0], args[1], args[2], args[3]
            if (abs(a) > 1e-6 and abs(b) > 1e-6) or (abs(c) > 1e-6 and abs(d) > 1e-6):
                return True
    return False


def _pattern_has_diagonal_geometry(pat_el: ET.Element) -> bool:
    """Verify a DiagonalStripe <pattern> contains actual diagonal geometry rather than static axis-aligned rects."""
    for el in pat_el.iter():
        tag = el.tag.split("}", 1)[-1]
        if tag == "line":
            try:
                x1 = float(el.attrib.get("x1", "0"))
                y1 = float(el.attrib.get("y1", "0"))
                x2 = float(el.attrib.get("x2", "0"))
                y2 = float(el.attrib.get("y2", "0"))
                if abs(x2 - x1) > 1e-6 and abs(y2 - y1) > 1e-6:
                    return True
            except ValueError:
                continue
        elif tag in ("polygon", "polyline"):
            raw_pts = el.attrib.get("points", "").strip()
            nums = [float(x) for x in _NUM_RX.findall(raw_pts)]
            pts = list(zip(nums[0::2], nums[1::2]))
            if len(pts) >= 2:
                limit = len(pts) if tag == "polygon" else len(pts) - 1
                for i in range(limit):
                    a, b = pts[i], pts[(i + 1) % len(pts)]
                    if abs(b[0] - a[0]) > 1e-6 and abs(b[1] - a[1]) > 1e-6:
                        return True
        elif tag == "path":
            d_attr = el.attrib.get("d", "").strip()
            if d_attr:
                try:
                    subpaths = _parse_svg_path_subpaths_with_closed(d_attr, min_points=2)
                except ValueError:
                    subpaths = []
                for sp, has_z in subpaths:
                    limit = len(sp) if (has_z and len(sp) >= 3) else len(sp) - 1
                    for i in range(limit):
                        a, b = sp[i], sp[(i + 1) % len(sp)]
                        if abs(b[0] - a[0]) > 1e-6 and abs(b[1] - a[1]) > 1e-6:
                            return True
        if (
            tag in _SVG_GEOM_TAGS
            or (tag == "g" and any(desc.tag.split("}", 1)[-1] in _SVG_GEOM_TAGS for desc in el.iter()))
        ) and el.attrib.get("transform", "").strip():
            if _transform_is_non_axis_aligned(el.attrib["transform"]):
                return True
    return False


def _svg_references_pattern_id(svg_root: ET.Element, pat_id: str) -> bool:
    """Return True if any element outside <pattern> subtrees references `url(#pat_id)` via fill or style."""
    if not pat_id:
        return False
    ref_rx = re.compile(rf"url\(\s*['\"]?#{re.escape(pat_id)}['\"]?\s*\)")
    stack: list[ET.Element] = [svg_root]
    while stack:
        el = stack.pop()
        tag = el.tag.split("}", 1)[-1]
        if tag == "pattern":
            continue
        fill_attr = el.attrib.get("fill", "")
        style_attr = el.attrib.get("style", "")
        if ref_rx.search(fill_attr) or ref_rx.search(style_attr):
            return True
        stack.extend(el)
    return False


def _pattern_structure_signature(pat_el: ET.Element) -> tuple[Any, ...]:
    """Return a canonical structural signature of a <pattern> element with `id` attributes stripped."""
    def _node_sig(el: ET.Element) -> tuple[Any, ...]:
        tag = el.tag.split("}", 1)[-1]
        attrs = tuple(sorted((k, v.strip()) for k, v in el.attrib.items() if k != "id"))
        text = (el.text or "").strip()
        children = tuple(_node_sig(c) for c in el)
        return (tag, attrs, text, children)

    return _node_sig(pat_el)


def _pattern_has_curved_dot_geometry(pat_el: ET.Element) -> bool:
    """Return True if a PolkaDots <pattern> contains circular or curved dot geometry."""
    for el in pat_el.iter():
        tag = el.tag.split("}", 1)[-1]
        if tag in ("circle", "ellipse"):
            return True
        if tag == "path" and re.search(r"[CcSsQqTtAa]", el.attrib.get("d", "")):
            return True
    return False


def _count_enum_variants(enum_block: str) -> int:
    """Count top-level variants inside a stripped Rust `{ ... }` enum block."""
    inner = enum_block[1:-1] if enum_block.startswith("{") and enum_block.endswith("}") else enum_block
    inner = re.sub(r"#\[[^\[\]]*(?:\[[^\[\]]*\][^\[\]]*)*\]", "", inner)
    variants = 0
    depth = 0
    cur: list[str] = []
    for ch in inner:
        if ch in "({[":
            depth += 1
            cur.append(ch)
        elif ch in ")}]":
            depth = max(0, depth - 1)
            cur.append(ch)
        elif ch == "," and depth == 0:
            tok = "".join(cur).strip()
            if tok and re.match(r"^[A-Za-z_][A-Za-z0-9_]*", tok):
                variants += 1
            cur = []
        else:
            cur.append(ch)
    tok = "".join(cur).strip()
    if tok and re.match(r"^[A-Za-z_][A-Za-z0-9_]*", tok):
        variants += 1
    return variants


def _run_cargo(ctx: RunContext, args: list[str], timeout: float = 90.0) -> SubprocessResult:
    cargo_bin = str(Path.home() / ".cargo" / "bin")
    cur_path = os.environ.get("PATH", "")
    new_path = f"{cargo_bin}:{cur_path}" if cargo_bin not in cur_path else cur_path
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
) -> Optional[TestOutcome]:
    cmd_args = list(args) if args and args[0] == "test" else ["test", *args]
    ct = _run_cargo(ctx, cmd_args, timeout=timeout)
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
    return None


def _write_coree_from_doc(dst: Path, title: str, doc: dict[str, Any]) -> None:
    manifest = {
        "format_version": 1,
        "app": "coree",
        "app_version": "0.3.1",
        "document_id": doc["id"],
        "title": title,
        "created_at": "2026-11-22T00:00:00Z",
        "modified_at": "2026-11-22T00:00:00Z",
        "page_count": len(doc["pages"]),
        "asset_count": len(doc["assets"]),
        "has_preview": False,
    }
    with zipfile.ZipFile(dst, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        zf.writestr("manifest.json", json.dumps(manifest, indent=2, ensure_ascii=False))
        zf.writestr("document.json", json.dumps(doc, indent=2, ensure_ascii=False))


# ---------------------------------------------------------------------------
# BASELINE (smoke=True) TESTS — Must PASS cleanly on v0.3.1
# ---------------------------------------------------------------------------

def _test_base_cli_core(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-CLI-CORE")
    v_res = run_cmd([str(ctx.cli_bin), "--version"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if v_res.returncode != 0 or not v_res.stdout.strip().startswith("coree "):
        return TestOutcome("FAIL", f"--version failed: rc={v_res.returncode} out={v_res.stdout!r}")

    d_res = run_cmd([str(ctx.cli_bin), "--diagnostics"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if d_res.returncode != 0:
        return TestOutcome("FAIL", f"--diagnostics failed: rc={d_res.returncode} err={d_res.stderr}")
    diag = d_res.stdout
    for token in (
        "MIT OR Apache-2.0",
        "Pure-Rust",
        "#![forbid(unsafe_code)]",
        "coree-core",
        "coree-cdr",
        "coree-io",
        "coree-render",
        "coree-prod",
        "Local System Fonts Indexed:",
    ):
        if token not in diag:
            return TestOutcome("FAIL", f"--diagnostics missing invariant token {token!r}")

    m = re.search(r"Local System Fonts Indexed:\s*(\d+)", diag)
    if not m or int(m.group(1)) <= 0:
        return TestOutcome("FAIL", f"Expected >0 local system fonts in diagnostics, got: {diag}")

    missing_cmds = sorted(_BASELINE_SUBCOMMANDS - set(ctx.cli_subcommands))
    if missing_cmds:
        return TestOutcome("FAIL", f"Missing baseline subcommands in --help: {missing_cmds}")

    if ctx.gui_bin is None or not ctx.gui_bin.is_file():
        return TestOutcome("FAIL", "Sibling GUI binary `coree` missing beside coree-cli")
    gui_v = run_cmd([str(ctx.gui_bin), "--version"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if gui_v.returncode != 0 or gui_v.stdout.strip() != v_res.stdout.strip():
        return TestOutcome("FAIL", f"GUI binary --version mismatch: {gui_v.stdout!r} vs {v_res.stdout!r}")

    return TestOutcome(
        "PASS",
        f"CLI+GUI verified ({v_res.stdout.strip()}, {len(ctx.cli_subcommands)} subcommands, {m.group(1)} local fonts)",
        metrics={"fonts_indexed": int(m.group(1)), "rss_kb": d_res.max_rss_kb},
    )


def _test_base_generate_and_inspect(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-GENERATE-INSPECT")
    samples = {
        "sample.coree": "coree",
        "sample_riff.cdr": "cdr-riff",
        "sample_x4.cdr": "cdr-x4",
        "sample_x6.cdr": "cdr-x6",
        "sample.cdt": "cdt",
        "sample.cmx": "cmx",
        "sample.svg": "svg",
    }
    for fname, fmt in samples.items():
        out = work / fname
        res = run_cmd([str(ctx.cli_bin), "generate-sample", str(out), "--format", fmt], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if res.returncode != 0 or not out.is_file():
            return TestOutcome("FAIL", f"generate-sample --format {fmt} failed: {res.stderr}")

    # Inspect both generated files and independent tier1 binary fixtures
    targets: list[tuple[Path, str]] = [
        (work / "sample.coree", "NativeCoree"),
        (work / "sample_riff.cdr", "CdrRiff"),
        (work / "sample_x4.cdr", "CdrZipX4"),
        (work / "sample_x6.cdr", "CdrZipX6"),
        (work / "sample.cdt", "Cdt"),
        (work / "sample.cmx", "Cmx"),
        (work / "sample.svg", "Svg"),
        (_f(ctx, "fixture.coree"), "NativeCoree"),
        (_f(ctx, "fixture_riff.cdr"), "CdrRiff"),
        (_f(ctx, "fixture_x4.cdr"), "CdrZipX4"),
        (_f(ctx, "fixture_x6.cdr"), "CdrZipX6"),
        (_f(ctx, "fixture.cdt"), "Cdt"),
        (_f(ctx, "fixture.cmx"), "Cmx"),
        (_f(ctx, "fixture.cpt"), "Cpt"),
        (_f(ctx, "fixture.svg"), "Svg"),
        (_f(ctx, "fixture.svgz"), "Svg"),
        (_f(ctx, "fixture.eps"), "Eps"),
        (_f(ctx, "fixture.pdf"), "Pdf"),
        (_f(ctx, "fixture.dxf"), "Dxf"),
        (_f(ctx, "fixture.plt"), "Plt"),
        (_f(ctx, "fixture.psd"), "Psd"),
    ]
    for path, fmt_hint in targets:
        res = run_cmd([str(ctx.cli_bin), "inspect", str(path), "--json"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if res.returncode != 0:
            return TestOutcome("FAIL", f"inspect --json failed on {path.name}: {res.stderr}")
        try:
            rep = _require_dict(json.loads(res.stdout), f"inspect {path.name}")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"Invalid JSON from inspect {path.name}: {exc}")
        for key in ("detected_format", "page_count", "layer_count", "object_count", "color_models"):
            if key not in rep:
                return TestOutcome("FAIL", f"inspect {path.name} missing key {key!r}")
        det_str = json.dumps(rep["detected_format"])
        if fmt_hint not in det_str:
            return TestOutcome("FAIL", f"inspect {path.name} detected {det_str}, expected substring {fmt_hint!r}")
        if not isinstance(rep["page_count"], int) or isinstance(rep["page_count"], bool) or rep["page_count"] < 1:
            return TestOutcome("FAIL", f"inspect {path.name} reported page_count={rep['page_count']}")

    # Independent binary dissectors + /usr/bin/file check (never trusting coree alone)
    try:
        riff_info = dissect_cdr_riff((work / "sample_riff.cdr").read_bytes())
        if riff_info["form_type"] != "CDRC" or not riff_info["chunks"]:
            return TestOutcome("FAIL", f"Independent RIFF dissector failed on sample_riff.cdr: {riff_info}")

        x6_info = dissect_cdr_zcf((work / "sample_x6.cdr").read_bytes())
        if not x6_info["entries"] or x6_info["bad_crc_entry"] is not None:
            return TestOutcome("FAIL", f"Independent ZCF ZIP dissector failed on sample_x6.cdr: {x6_info}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Independent dissector rejected CLI generated sample: {exc}")

    psd_info = dissect_psd(_f(ctx, "fixture.psd").read_bytes())
    if psd_info["width"] != 4 or psd_info["height"] != 4 or psd_info["channels"] != 3:
        return TestOutcome("FAIL", f"Independent PSD dissector mismatch: {psd_info}")

    magic_pdf = run_file_magic(_f(ctx, "fixture.pdf"), timeout_scale=ctx.timeout_scale)
    if magic_pdf["ok"] and "pdf" not in magic_pdf["mime"].lower():
        return TestOutcome("FAIL", f"/usr/bin/file did not recognize fixture.pdf as PDF: {magic_pdf}")

    return TestOutcome("PASS", f"Generated & dissected {len(targets)} format samples cleanly")


def _test_base_roundtrip_vector_and_external_oracles(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-ROUNDTRIP-VEC")
    src = _f(ctx, "fixture.coree")
    _, src_doc = inspect_coree_zip(src)
    src_obj_count = len(src_doc["objects"])

    # 1. .coree -> .svg -> .coree semantic round-trip
    out_svg = work / "out.svg"
    out_coree_rt = work / "rt_from_svg.coree"
    r1 = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_svg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r1.returncode != 0 or not out_svg.is_file():
        return TestOutcome("FAIL", f"convert .coree -> .svg failed: {r1.stderr}")
    try:
        svg_struct = parse_svg_structure(out_svg.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Exported SVG malformed: {exc}")
    if svg_struct["vector_elements"] < 2 or "CutContour" not in svg_struct["layers"]:
        return TestOutcome("FAIL", f"Exported SVG missing semantic geometry/CutContour layer: {svg_struct}")

    r2 = run_cmd([str(ctx.cli_bin), "convert", str(out_svg), str(out_coree_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r2.returncode != 0:
        return TestOutcome("FAIL", f"convert .svg -> .coree failed: {r2.stderr}")
    try:
        _, rt_doc = inspect_coree_zip(out_coree_rt)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Re-imported .coree malformed: {exc}")
    rt_objs = rt_doc.get("objects")
    rt_pages = rt_doc.get("pages")
    if not isinstance(rt_objs, dict) or not isinstance(rt_pages, list) or len(rt_objs) < 2 or len(rt_pages) != 1:
        return TestOutcome("FAIL", f"Re-imported .coree lost vector objects: objects={type(rt_objs).__name__}, pages={type(rt_pages).__name__}")

    # 2. .coree -> .pdf (--cmyk --crop-marks AND unmarked --cmyk) + Ghostscript + Poppler + TrimBox pixel diff vs coree render
    out_pdf = work / "out_cmyk_marks.pdf"
    r_pdf = run_cmd(
        [str(ctx.cli_bin), "convert", str(src), str(out_pdf), "--cmyk", "--crop-marks"],
        cwd=work,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r_pdf.returncode != 0 or not out_pdf.is_file():
        return TestOutcome("FAIL", f"convert .coree -> .pdf (--crop-marks) failed: {r_pdf.stderr}")
    try:
        pdf_struct = parse_pdf_structure(out_pdf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Exported PDF malformed: {exc}")
    if not (pdf_struct["has_xref"] and pdf_struct["has_eof"] and pdf_struct["has_device_cmyk"]):
        return TestOutcome("FAIL", f"Exported PDF missing CMYK/xref structure: {pdf_struct}")

    pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
    if not pinfo["ok"] or pinfo["fields"].get("Pages") != "1":
        return TestOutcome("FAIL", f"Poppler pdfinfo failed on exported PDF: {pinfo}")

    gs_pdf_png = work / "gs_pdf.png"
    gs_pdf = run_ghostscript_render(out_pdf, gs_pdf_png, dpi=96, device="png16m", timeout_scale=ctx.timeout_scale)
    if not gs_pdf["ok"] or gs_pdf["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"Ghostscript failed to rasterize exported PDF cleanly: {gs_pdf}")

    ppm_marks = run_pdftoppm(out_pdf, work / "poppler_marks", dpi=96, timeout_scale=ctx.timeout_scale)
    if not ppm_marks["ok"] or ppm_marks["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"Poppler pdftoppm failed on --crop-marks PDF: {ppm_marks}")

    out_unmarked_pdf = work / "out_cmyk_unmarked.pdf"
    r_updf = run_cmd(
        [str(ctx.cli_bin), "convert", str(src), str(out_unmarked_pdf), "--cmyk"],
        cwd=work,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r_updf.returncode != 0 or not out_unmarked_pdf.is_file():
        return TestOutcome("FAIL", f"convert .coree -> unmarked .pdf failed: {r_updf.stderr}")
    ppm_unmarked = run_pdftoppm(out_unmarked_pdf, work / "poppler_unmarked", dpi=96, timeout_scale=ctx.timeout_scale)
    if not ppm_unmarked["ok"] or ppm_unmarked["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"Poppler pdftoppm failed on unmarked PDF: {ppm_unmarked}")
    if ppm_marks["png_info"]["width"] <= ppm_unmarked["png_info"]["width"]:
        return TestOutcome("FAIL", "--crop-marks PDF was not larger than unmarked PDF")

    coree_png = work / "coree_render.png"
    r_ren = run_cmd([str(ctx.cli_bin), "render", str(src), str(coree_png), "--dpi", "96", "--view", "enhanced"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ren.returncode != 0 or not coree_png.is_file():
        return TestOutcome("FAIL", f"coree render failed: {r_ren.stderr}")
    try:
        coree_info = parse_png_info(coree_png.read_bytes())
        ppm_trim_png = work / "poppler_trim.png"
        crop_png_center(ppm_unmarked["out_png"], coree_info["width"], coree_info["height"], ppm_trim_png)

        px_cmp = compare_png_pixels(coree_png, ppm_trim_png)
        if not px_cmp["size_match"]:
            return TestOutcome("FAIL", f"TrimBox pixel size mismatch: {px_cmp}")
        if px_cmp["mean_abs_diff"] >= 8.0 or px_cmp["non_white_iou"] < 0.85:
            return TestOutcome("FAIL", f"coree render vs Poppler pdftoppm diverged severely: {px_cmp}")

        blank_png = work / "blank_white.png"
        build_blank_white_png(coree_info["width"], coree_info["height"], blank_png)
        blank_cmp = compare_png_pixels(coree_png, blank_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"PNG oracle parsing failed on coree render: {exc}")
    if px_cmp["mean_abs_diff"] >= blank_cmp["mean_abs_diff"] * 0.6 or blank_cmp["non_white_iou"] > 0.01:
        return TestOutcome(
            "FAIL",
            f"Pixel oracle failed to cleanly beat blank-page control: pdf_cmp={px_cmp}, blank_cmp={blank_cmp}",
        )

    # 3. .coree -> .eps + Ghostscript validation
    out_eps = work / "out.eps"
    r_eps = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_eps)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_eps.returncode != 0 or not out_eps.is_file():
        return TestOutcome("FAIL", f"convert .coree -> .eps failed: {r_eps.stderr}")
    eps_txt = out_eps.read_text(encoding="latin-1")
    if "%!PS" not in eps_txt or "%%BoundingBox:" not in eps_txt:
        return TestOutcome("FAIL", "Exported EPS missing %!PS or %%BoundingBox")
    gs_eps_png = work / "gs_eps.png"
    gs_eps = run_ghostscript_render(out_eps, gs_eps_png, dpi=96, device="pngalpha", timeout_scale=ctx.timeout_scale)
    if not gs_eps["ok"] or gs_eps["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"Ghostscript failed on exported EPS: {gs_eps}")

    # 4. .coree -> .dxf & .plt semantic checks
    out_dxf = work / "out.dxf"
    out_plt = work / "out.plt"
    for ext_out in (out_dxf, out_plt):
        rr = run_cmd([str(ctx.cli_bin), "convert", str(src), str(ext_out)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if rr.returncode != 0 or not ext_out.is_file():
            return TestOutcome("FAIL", f"convert .coree -> {ext_out.suffix} failed: {rr.stderr}")
    try:
        dxf_s = parse_dxf_structure(out_dxf.read_bytes())
        if "ENTITIES" not in dxf_s["sections"] or not dxf_s["has_eof"] or not dxf_s["entities"]:
            return TestOutcome("FAIL", f"Exported DXF missing entities/EOF: {dxf_s}")
        plt_s = parse_plt_commands(out_plt.read_bytes())
        if not plt_s["has_pu_pd"] or plt_s["command_count"] < 4:
            return TestOutcome("FAIL", f"Exported PLT missing PU/PD commands: {plt_s}")

        # 5. .coree -> .cdr (riff, x4, x6)
        for ver in ("riff", "x4", "x6"):
            out_cdr = work / f"out_{ver}.cdr"
            rc = run_cmd(
                [str(ctx.cli_bin), "convert", str(src), str(out_cdr), "--cdr-version", ver],
                cwd=work,
                timeout=25.0,
                timeout_scale=ctx.timeout_scale,
            )
            if rc.returncode != 0 or not out_cdr.is_file():
                return TestOutcome("FAIL", f"convert .coree -> .cdr ({ver}) failed: {rc.stderr}")
            if ver == "riff":
                rinfo = dissect_cdr_riff(out_cdr.read_bytes())
                if not rinfo["loda_headers"]:
                    return TestOutcome("FAIL", f"Exported RIFF .cdr missing loda objects: {rinfo}")
            else:
                zinfo = dissect_cdr_zcf(out_cdr.read_bytes())
                if zinfo["bad_crc_entry"] is not None or not zinfo["entries"]:
                    return TestOutcome("FAIL", f"Exported {ver} .cdr ZIP invalid: {zinfo}")

        # 6. .pdf / .eps / .dxf / .plt / .psd baseline import -> .svg / .png
        for inp_name in ("fixture.pdf", "fixture.eps", "fixture.dxf", "fixture.plt", "fixture.psd"):
            inp = _f(ctx, inp_name)
            out_png = work / f"from_{inp.stem}_{inp.suffix[1:]}.png"
            rc = run_cmd([str(ctx.cli_bin), "convert", str(inp), str(out_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
            if rc.returncode != 0 or not out_png.is_file():
                return TestOutcome("FAIL", f"Baseline import convert {inp_name} -> PNG failed: {rc.stderr}")
            p_info = parse_png_info(out_png.read_bytes())
            if p_info["non_white_pixels"] == 0:
                return TestOutcome("FAIL", f"Imported {inp_name} rendered completely blank PNG")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Export/import output dissector failed: {exc}")

    return TestOutcome(
        "PASS",
        "Vector round-trips + Ghostscript + Poppler + PIL/stdlib pixel oracles passed",
        metrics={"pdf_vs_coree_mean_diff": round(px_cmp.get("mean_abs_diff", 0.0), 2)},
    )


def _test_base_pure_rust_empty_path_fallback(ctx: RunContext) -> TestOutcome:
    """Verify coree-cli built-in pure-Rust engines work with PATH='' (no gs/poppler/external tools)."""
    work = ctx.make_scratch_dir("T1-BASE-EMPTY-PATH")
    src = _f(ctx, "fixture.coree")
    out_pdf = work / "selfcontained.pdf"
    out_eps = work / "selfcontained.eps"
    out_png = work / "selfcontained.png"

    clean_env = {"PATH": ""}
    for args in (
        ["convert", str(src), str(out_pdf), "--cmyk"],
        ["convert", str(src), str(out_eps)],
        ["render", str(src), str(out_png), "--dpi", "96"],
    ):
        res = run_cmd([str(ctx.cli_bin), *args], cwd=work, env=clean_env, timeout=25.0, timeout_scale=ctx.timeout_scale, measure_rss=False)
        if res.returncode != 0:
            return TestOutcome("FAIL", f"Pure-Rust fallback failed under PATH='': {args} -> {res.stderr}")

    try:
        pdf_s = parse_pdf_structure(out_pdf.read_bytes())
        png_s = parse_png_info(out_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"PATH='' output malformed: {exc}")
    if not (pdf_s["has_xref"] and pdf_s["has_device_cmyk"]) or png_s["non_white_pixels"] < 100:
        return TestOutcome("FAIL", f"PATH='' outputs invalid: pdf={pdf_s}, png={png_s}")

    return TestOutcome("PASS", "Pure-Rust self-contained PDF/EPS/render verified with PATH=''")


def _test_base_render_viewmodes_and_thumbnail(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-RENDER-THUMB")
    sample = work / "multi.coree"
    g_res = run_cmd([str(ctx.cli_bin), "generate-sample", str(sample), "--format", "coree"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if g_res.returncode != 0:
        return TestOutcome("FAIL", f"generate-sample failed: {g_res.stderr}")

    infos: dict[str, dict[str, Any]] = {}
    for view in ("wireframe", "normal", "enhanced", "pixels"):
        out = work / f"view_{view}.png"
        r = run_cmd(
            [str(ctx.cli_bin), "render", str(sample), str(out), "--dpi", "72", "--view", view],
            cwd=work,
            timeout=25.0,
            timeout_scale=ctx.timeout_scale,
        )
        if r.returncode != 0 or not out.is_file():
            return TestOutcome("FAIL", f"render --view {view} failed: {r.stderr}")
        try:
            info = parse_png_info(out.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"render --view {view} output malformed PNG: {exc}")
        if info["width"] <= 0 or info["height"] <= 0 or info["non_white_pixels"] == 0 or info["luma_stddev"] < 1.0:
            return TestOutcome("FAIL", f"render --view {view} produced blank/degenerate PNG: {info}")
        infos[view] = info

    # Anti-illusion check: enhanced filled artwork MUST have substantially more non-white pixels than wireframe
    if infos["enhanced"]["non_white_pixels"] <= infos["wireframe"]["non_white_pixels"]:
        return TestOutcome(
            "FAIL",
            f"Semantic render bug: enhanced non_white ({infos['enhanced']['non_white_pixels']}) "
            f"<= wireframe ({infos['wireframe']['non_white_pixels']})",
        )

    # Multipage render must be wider than single-page render
    out_mp = work / "multipage.png"
    r_mp = run_cmd(
        [str(ctx.cli_bin), "render", str(sample), str(out_mp), "--dpi", "72", "--multipage"],
        cwd=work,
        timeout=25.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r_mp.returncode != 0 or not out_mp.is_file():
        return TestOutcome("FAIL", f"render --multipage failed: {r_mp.stderr}")
    try:
        mp_info = parse_png_info(out_mp.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"render --multipage output malformed PNG: {exc}")
    if mp_info["width"] <= infos["enhanced"]["width"]:
        return TestOutcome("FAIL", f"Multipage width {mp_info['width']} not > single page {infos['enhanced']['width']}")

    # Thumbnail --size 64
    thumb = work / "thumb64.png"
    r_th = run_cmd([str(ctx.cli_bin), "thumbnail", str(sample), str(thumb), "--size", "64"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_th.returncode != 0 or not thumb.is_file():
        return TestOutcome("FAIL", f"thumbnail --size 64 failed: {r_th.stderr}")
    try:
        th_info = parse_png_info(thumb.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"thumbnail --size 64 output malformed PNG: {exc}")
    if th_info["width"] > 64 or th_info["height"] > 64 or th_info["luma_stddev"] < 1.0:
        return TestOutcome("FAIL", f"thumbnail --size 64 invalid dimensions or blank: {th_info}")

    return TestOutcome(
        "PASS",
        f"4 render view modes + multipage ({mp_info['width']}x{mp_info['height']}) + thumbnail ({th_info['width']}x{th_info['height']}) verified",
    )


def _test_base_laser_cut(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-LASER")
    src = _f(ctx, "fixture.coree")

    for dialect in ("grbl", "marlin", "ruida"):
        out_gc = work / f"cut_{dialect}.gcode"
        r = run_cmd(
            [
                str(ctx.cli_bin), "laser", str(src), str(out_gc),
                "--dialect", dialect, "--kerf", "0.1", "--weeding-box",
            ],
            cwd=work,
            timeout=25.0,
            timeout_scale=ctx.timeout_scale,
        )
        if r.returncode != 0 or not out_gc.is_file():
            return TestOutcome("FAIL", f"laser --dialect {dialect} failed: {r.stderr}")
        txt = out_gc.read_text(encoding="utf-8", errors="replace")
        if ("G0" not in txt and "G1" not in txt) or len(txt.splitlines()) < 10:
            return TestOutcome("FAIL", f"laser {dialect} G-Code missing motion commands")

    out_lb = work / "lightburn_job.svg"
    r_lb = run_cmd(
        [str(ctx.cli_bin), "laser", str(src), str(out_lb), "--plugin", "lightburn-bridge"],
        cwd=work,
        timeout=25.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r_lb.returncode != 0 or not out_lb.is_file():
        return TestOutcome("FAIL", f"laser --plugin lightburn-bridge failed: {r_lb.stderr}")
    try:
        lb_svg = parse_svg_structure(out_lb.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"lightburn-bridge SVG output malformed: {exc}")
    if lb_svg["counts"].get("path", 0) < 1:
        return TestOutcome("FAIL", f"lightburn-bridge SVG output had 0 paths: {lb_svg}")

    for fmt, ext in (("dxf", "dxf"), ("plt", "plt")):
        out_f = work / f"laser.{ext}"
        r = run_cmd([str(ctx.cli_bin), "laser", str(src), str(out_f), "--format", fmt], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r.returncode != 0 or not out_f.is_file():
            return TestOutcome("FAIL", f"laser --format {fmt} failed: {r.stderr}")
        try:
            if fmt == "dxf":
                d = parse_dxf_structure(out_f.read_bytes())
                if not d["entities"]:
                    return TestOutcome("FAIL", f"laser DXF output had 0 entities: {d}")
            else:
                p = parse_plt_commands(out_f.read_bytes())
                if not p["has_pu_pd"]:
                    return TestOutcome("FAIL", f"laser PLT output missing PU/PD: {p}")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"laser --format {fmt} output malformed: {exc}")

    return TestOutcome("PASS", "Laser G-Code (GRBL/Marlin/Ruida) + DXF + PLT + lightburn-bridge verified")


def _test_base_preflight_and_collect(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-PREFLIGHT-COLLECT")
    sample = work / "sample.coree"
    run_cmd([str(ctx.cli_bin), "generate-sample", str(sample)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

    p_res = run_cmd([str(ctx.cli_bin), "preflight", str(sample), "--json"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if p_res.returncode != 0:
        return TestOutcome("FAIL", f"preflight --json failed: {p_res.stderr}")
    try:
        rep = _require_dict(json.loads(p_res.stdout), "preflight --json")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"preflight --json returned invalid JSON: {exc}")
    for k in ("document_title", "color_mode", "page_count", "object_count", "issues"):
        if k not in rep:
            return TestOutcome("FAIL", f"PreflightReport missing key {k!r}")
    if not isinstance(rep["issues"], list) or len(rep["issues"]) == 0:
        return TestOutcome("FAIL", "Expected sample.coree preflight to flag RGB-in-CMYK issues")

    collect_dir = work / "collected"
    c_res = run_cmd([str(ctx.cli_bin), "collect", str(sample), str(collect_dir)], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if c_res.returncode != 0:
        return TestOutcome("FAIL", f"collect failed: {c_res.stderr}")
    for req_file in ("manifest.json", "preflight.json", "fonts/fonts_manifest.json"):
        if not (collect_dir / req_file).is_file():
            return TestOutcome("FAIL", f"collect output missing {req_file}")
    if not list(collect_dir.glob("*.coree.json")):
        return TestOutcome("FAIL", "collect output missing *.coree.json")

    try:
        manifest = _require_dict(
            json.loads((collect_dir / "manifest.json").read_text(encoding="utf-8")),
            "collect manifest.json",
        )
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"collect manifest.json malformed: {exc}")
    pm = manifest.get("production_metrics")
    tv_len = pm.get("total_vector_length_mm") if isinstance(pm, dict) else None
    if (
        not isinstance(pm, dict)
        or not isinstance(tv_len, (int, float))
        or isinstance(tv_len, bool)
        or tv_len <= 0.0
        or "cut_length_mm" not in pm
        or "material_utilization_pct" not in pm
    ):
        return TestOutcome("FAIL", f"collect manifest.json missing valid ProductionMetrics (Path Meter): {pm}")

    return TestOutcome("PASS", f"Preflight ({len(rep['issues'])} issues) + Collect bundle + ProductionMetrics verified")


def _test_base_print_merge(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-PRINT-MERGE")
    tmpl = _f(ctx, "print_merge_template.coree")
    csv_p = _f(ctx, "merge_data.csv")
    out_coree = work / "merged.coree"
    out_pdf = work / "merged.pdf"

    r1 = run_cmd([str(ctx.cli_bin), "print-merge", str(tmpl), str(csv_p), str(out_coree)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r1.returncode != 0 or not out_coree.is_file():
        return TestOutcome("FAIL", f"print-merge -> .coree failed: {r1.stderr}")
    try:
        _, doc = inspect_coree_zip(out_coree)
        pages = doc["pages"]
        objs = doc["objects"]
        if not isinstance(pages, list) or not isinstance(objs, dict):
            raise ValueError("print-merge .coree pages/objects schema invalid")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"print-merge .coree output malformed: {exc}")
    if len(pages) != 2:
        return TestOutcome("FAIL", f"Expected 2 merged pages from 2 CSV rows, got {len(pages)}")

    texts = []
    for obj in objs.values():
        if not isinstance(obj, dict):
            continue
        payload = obj.get("payload")
        if not isinstance(payload, dict):
            continue
        at = payload.get("ArtisticText")
        if isinstance(at, dict) and isinstance(at.get("runs"), list):
            for run in at["runs"]:
                if isinstance(run, dict):
                    texts.append(str(run.get("text", "")))
    joined = "\n".join(texts)
    if "Alice, A." not in joined or "أحمد علي" not in joined or "<<Name>>" in joined:
        return TestOutcome("FAIL", f"Print-merge failed semantic Arabic/Latin substitution: {texts!r}")

    r2 = run_cmd([str(ctx.cli_bin), "print-merge", str(tmpl), str(csv_p), str(out_pdf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r2.returncode != 0 or not out_pdf.is_file():
        return TestOutcome("FAIL", f"print-merge -> .pdf failed: {r2.stderr}")
    pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
    if not pinfo["ok"] or pinfo["fields"].get("Pages") != "2":
        return TestOutcome("FAIL", f"Merged PDF Poppler page count != 2: {pinfo}")

    return TestOutcome("PASS", "Print-merge substituted Latin + Arabic UTF-8 rows into 2-page .coree & PDF")


def _test_base_trace_raster(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-TRACE")
    for img_name in ("trace_shape.png", "trace_shape.bmp"):
        img = _f(ctx, img_name)
        out_svg = work / f"traced_{img.stem}_{img.suffix[1:]}.svg"
        r = run_cmd(
            [str(ctx.cli_bin), "trace", str(img), str(out_svg), "--mode", "outline", "--colors", "4"],
            cwd=work,
            timeout=25.0,
            timeout_scale=ctx.timeout_scale,
        )
        if r.returncode != 0 or not out_svg.is_file():
            return TestOutcome("FAIL", f"trace {img_name} failed: {r.stderr}")
        try:
            svg_s = parse_svg_structure(out_svg.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"trace {img_name} SVG output malformed: {exc}")
        if svg_s["counts"].get("path", 0) < 1:
            return TestOutcome("FAIL", f"trace {img_name} produced 0 <path> elements: {svg_s}")

        out_ren = work / f"retraced_{img.stem}_{img.suffix[1:]}.png"
        rr = run_cmd([str(ctx.cli_bin), "render", str(out_svg), str(out_ren), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if rr.returncode != 0:
            return TestOutcome("FAIL", f"render traced SVG failed: {rr.stderr}")
        try:
            png_s = parse_png_info(out_ren.read_bytes())
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"rendered traced SVG PNG malformed: {exc}")
        if png_s["non_white_pixels"] < 20:
            return TestOutcome("FAIL", f"Rendered traced SVG is blank: {png_s}")

    return TestOutcome("PASS", "Raster trace (PNG + BMP -> SVG <path> -> render) verified")


def _test_base_rhai_macro(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-BASE-MACRO")
    src = _f(ctx, "fixture.coree")
    out_rhai = work / "macro_file.coree"
    out_builtin = work / "macro_builtin.coree"

    r1 = run_cmd([str(ctx.cli_bin), "run-macro", str(src), str(_f(ctx, "macro_recolor.rhai")), str(out_rhai)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r1.returncode != 0 or not out_rhai.is_file():
        return TestOutcome("FAIL", f"run-macro .rhai failed: {r1.stderr}")
    try:
        _, doc1 = inspect_coree_zip(out_rhai)
        objs1 = _require_dict(doc1.get("objects"), "run-macro .rhai objects")
        obj3 = _require_dict(objs1.get("3"), "run-macro .rhai objects['3']")
        rect_fill = _require_dict(obj3.get("fill"), "run-macro .rhai objects['3'].fill")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"run-macro .rhai output .coree malformed: {exc}")
    if "Solid" not in rect_fill or not isinstance(rect_fill["Solid"], dict) or "Cmyk" not in rect_fill["Solid"]:
        return TestOutcome("FAIL", f"Macro recolor did not produce CMYK Solid fill: {rect_fill}")

    # Builtin recolor-cmyk MUST convert an actual RGB document's fills/outlines to CMYK
    rgb_src = work / "rgb_input.coree"
    _, rgb_doc = inspect_coree_zip(src)
    rgb_doc["color_mode"] = "Rgb"
    rgb_doc["objects"]["3"]["fill"] = {"Solid": {"Rgba": {"r": 0.0, "g": 0.65, "b": 0.9, "a": 1.0}}}
    rgb_doc["objects"]["4"]["fill"] = {"Solid": {"Rgba": {"r": 0.9, "g": 0.15, "b": 0.3, "a": 1.0}}}
    _write_coree_from_doc(rgb_src, "RGB Macro Source", rgb_doc)

    r2 = run_cmd([str(ctx.cli_bin), "run-macro", str(rgb_src), "recolor-cmyk", str(out_builtin)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r2.returncode != 0 or not out_builtin.is_file():
        return TestOutcome("FAIL", f"run-macro builtin recolor-cmyk failed: {r2.stderr}")
    try:
        _, doc2 = inspect_coree_zip(out_builtin)
        objs2 = _require_dict(doc2.get("objects"), "run-macro builtin objects")
        for oid in ("3", "4"):
            obj_i = _require_dict(objs2.get(oid), f"run-macro builtin objects[{oid!r}]")
            fill2 = _require_dict(obj_i.get("fill"), f"run-macro builtin objects[{oid!r}].fill")
            if "Solid" not in fill2 or not isinstance(fill2["Solid"], dict) or "Cmyk" not in fill2["Solid"]:
                return TestOutcome("FAIL", f"Builtin recolor-cmyk failed to convert object {oid} RGB->CMYK: {fill2}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"run-macro builtin output .coree malformed: {exc}")

    return TestOutcome("PASS", "Rhai file macro + builtin recolor-cmyk verified semantically")


def _test_base_hygiene_and_palettes(ctx: RunContext) -> TestOutcome:
    crates_dir = ctx.repo_root / "crates"
    entry_files = sorted(crates_dir.glob("*/src/lib.rs")) + [crates_dir / "coree-app" / "src" / "main.rs"]
    for ef in entry_files:
        head = "\n".join(ef.read_text(encoding="utf-8", errors="replace").splitlines()[:10])
        if "#![forbid(unsafe_code)]" not in head:
            return TestOutcome("FAIL", f"{ef.relative_to(ctx.repo_root)} missing #![forbid(unsafe_code)]")

    for rs in crates_dir.rglob("*.rs"):
        txt = rs.read_text(encoding="utf-8", errors="replace")
        if re.search(r"(?<!#\[forbid\()\bunsafe\s*\{", txt):
            return TestOutcome("FAIL", f"Found unsafe block in {rs.relative_to(ctx.repo_root)}")

    cargo_toml = _read_crates_text(ctx, "Cargo.toml")
    if 'license = "MIT OR Apache-2.0"' not in cargo_toml:
        return TestOutcome("FAIL", "Cargo.toml missing MIT OR Apache-2.0 license")
    for net_crate in ("reqwest", "ureq", "hyper", "tokio", "curl", "surf", "isahc"):
        if re.search(rf"(?m)^\s*{net_crate}\s*=", cargo_toml):
            return TestOutcome("FAIL", f"Forbidden network crate {net_crate!r} found in Cargo.toml")

    tokens_raw = (ctx.repo_root / "replica" / "design" / "tokens.json").read_text(encoding="utf-8").lower()
    if "#009c4a" in tokens_raw:
        return TestOutcome("FAIL", "Forbidden emerald color #009c4a found in replica/design/tokens.json")

    for pal_name, magic in (("fixture.gpl", b"GIMP Palette"), ("fixture.ase", b"ASEF"), ("fixture.pal", b"JASC-PAL")):
        p = _f(ctx, pal_name)
        if not p.is_file() or not p.read_bytes().startswith(magic):
            return TestOutcome("FAIL", f"Invalid palette fixture {pal_name}")

    return TestOutcome("PASS", f"Verified #![forbid(unsafe_code)] across {len(entry_files)} roots, 0 network crates, clean tokens & palettes")


def _test_base_all_eleven_lenses(ctx: RunContext) -> TestOutcome:
    """Exercise all 11 LensKind variants (FEAT-52..FEAT-62) via coree-cli render vs lens-free control."""
    work = ctx.make_scratch_dir("T1-BASE-LENSES")
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))

    # Compact 100x100mm page with high-contrast shapes + text under a 70x60mm lens overlay
    doc["pages"][0]["width_mm"] = 100.0
    doc["pages"][0]["height_mm"] = 100.0
    doc["pages"][0]["bleed_mm"] = 0.0
    doc["pages"][0]["layers"][1]["objects"] = []

    base_outline = dict(doc["objects"]["3"]["outline"])
    lens_outline = dict(base_outline)
    lens_outline["width_mm"] = 0.2

    doc["objects"]["3"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 10.0]}
    doc["objects"]["3"]["payload"] = {
        "Shape": {
            "Rectangle": {
                "width": 60.0,
                "height": 50.0,
                "corner_radii": [2.0, 2.0, 2.0, 2.0],
                "corner_style": "Rounded",
                "lock_corners": True,
            }
        }
    }
    doc["objects"]["4"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 60.0, 55.0]}
    doc["objects"]["5"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 22.0, 48.0]}
    doc["objects"].pop("7", None)

    doc["objects"]["8"] = {
        "id": 8,
        "name": "Lens Window",
        "payload": {
            "Shape": {
                "Rectangle": {
                    "width": 70.0,
                    "height": 60.0,
                    "corner_radii": [0.0, 0.0, 0.0, 0.0],
                    "corner_style": "Rounded",
                    "lock_corners": True,
                }
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 15.0, 15.0]},
        "fill": "None",
        "outline": lens_outline,
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
    doc["pages"][0]["layers"][0]["objects"] = [3, 4, 5, 8]
    doc["next_id"] = 9

    ctrl_coree = work / "lens_control.coree"
    ctrl_png = work / "lens_control.png"
    _write_coree_from_doc(ctrl_coree, "Lens Control", doc)
    rc = run_cmd([str(ctx.cli_bin), "render", str(ctrl_coree), str(ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rc.returncode != 0 or not ctrl_png.is_file():
        return TestOutcome("FAIL", f"Lens control render failed: {rc.stderr}")
    try:
        ctrl_info = parse_png_info(ctrl_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Lens control render PNG malformed: {exc}")
    if ctrl_info["non_white_pixels"] < 100:
        return TestOutcome("FAIL", f"Lens control rendered blank: {ctrl_info}")

    eleven_lenses: list[tuple[str, str, Any]] = [
        ("FEAT-52", "Brighten", {"Brighten": {"rate_pct": 55.0}}),
        ("FEAT-53", "ColorAdd", {"ColorAdd": {"color": {"Rgba": {"r": 0.9, "g": 0.2, "b": 0.1, "a": 1.0}}, "rate_pct": 65.0}}),
        ("FEAT-54", "ColorLimit", {"ColorLimit": {"color": {"Rgba": {"r": 0.2, "g": 0.9, "b": 0.3, "a": 1.0}}, "rate_pct": 85.0}}),
        ("FEAT-55", "CustomColorMap", {
            "CustomColorMap": {
                "from_color": {"Rgba": {"r": 0.1, "g": 0.0, "b": 0.4, "a": 1.0}},
                "to_color": {"Rgba": {"r": 1.0, "g": 0.9, "b": 0.2, "a": 1.0}},
                "palette_mode": "ForwardRainbow",
            }
        }),
        ("FEAT-56", "FishEye", {"FishEye": {"rate_pct": 180.0}}),
        ("FEAT-57", "HeatMap", {"HeatMap": {"palette_rotation_deg": 45.0}}),
        ("FEAT-58", "Invert", "Invert"),
        ("FEAT-59", "Magnify", {"Magnify": {"amount": 2.5}}),
        ("FEAT-60", "TintedGrayscale", {"TintedGrayscale": {"tint": {"Rgba": {"r": 0.3, "g": 0.15, "b": 0.05, "a": 1.0}}}}),
        ("FEAT-61", "Transparency", {"Transparency": {"color": {"Rgba": {"r": 0.95, "g": 0.85, "b": 0.1, "a": 1.0}}, "rate_pct": 60.0}}),
        ("FEAT-62", "Wireframe", {
            "Wireframe": {
                "override_outline": {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}},
                "override_fill": {"Rgba": {"r": 1.0, "g": 0.95, "b": 0.4, "a": 1.0}},
            }
        }),
    ]

    diffs: dict[str, float] = {}
    for feat_id, lens_name, kind_obj in eleven_lenses:
        doc["objects"]["8"]["lens"] = {
            "kind": kind_obj,
            "frozen": False,
            "viewpoint": None,
            "remove_face": False,
        }
        in_p = work / f"lens_{feat_id}.coree"
        out_p = work / f"lens_{feat_id}.png"
        _write_coree_from_doc(in_p, f"Lens {lens_name}", doc)
        r = run_cmd([str(ctx.cli_bin), "render", str(in_p), str(out_p), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r.returncode != 0 or not out_p.is_file():
            return TestOutcome("FAIL", f"{feat_id} ({lens_name}) CLI render failed: {r.stderr}")
        try:
            l_info = parse_png_info(out_p.read_bytes())
            if l_info["non_white_pixels"] < 50:
                return TestOutcome("FAIL", f"{feat_id} ({lens_name}) rendered blank PNG: {l_info}")
            cmp = _compare_same_page_pngs(ctrl_png, out_p, f"{feat_id} ({lens_name})")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"{feat_id} ({lens_name}) rendered malformed PNG: {exc}")
        if cmp["mean_abs_diff"] <= 0.2:
            return TestOutcome(
                "FAIL",
                f"{feat_id} ({lens_name}) lens was a visual no-op vs control (mean_abs_diff={cmp['mean_abs_diff']:.4f})",
            )
        diffs[lens_name] = round(float(cmp["mean_abs_diff"]), 2)

    return TestOutcome(
        "PASS",
        f"All 11 Lenses (FEAT-52..FEAT-62) rendered with semantic pixel diffs > 0.2 vs control",
        metrics={"lens_mean_abs_diffs": diffs},
    )


def _test_replica_contrast(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    work = ctx.make_scratch_dir("T1-REPLICA-CONTRAST")
    script = ctx.replica_skills_dir / "replica-design" / "contrast.py"
    tokens = ctx.repo_root / "replica" / "design" / "tokens.json"
    res = run_cmd([sys.executable, str(script), str(tokens), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if res.returncode != 0:
        return TestOutcome("FAIL", f"contrast.py failed (rc={res.returncode}): {res.stderr}\n{res.stdout}")
    try:
        rows = json.loads(res.stdout)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"contrast.py returned invalid JSON: {exc}")
    if not isinstance(rows, list) or len(rows) != 35 or not all(isinstance(r, dict) and r.get("aa") is True and "error" not in r for r in rows):
        return TestOutcome("FAIL", f"contrast.py expected 35/35 AA pass, got {rows!r}")

    neg = run_cmd([sys.executable, str(script), "#777777", "#888888", "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if neg.returncode != 1:
        return TestOutcome("FAIL", f"contrast.py negative pair expected rc=1, got rc={neg.returncode}")
    return TestOutcome("PASS", "contrast.py 35/35 AA pass + negative pair rejection verified")


def _test_replica_listing(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    work = ctx.make_scratch_dir("T1-REPLICA-LISTING-NEG")
    script = ctx.replica_skills_dir / "replica-launch" / "listing.py"
    listing = ctx.repo_root / "replica" / "launch" / "listing.json"
    res = run_cmd([sys.executable, str(script), str(listing), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if res.returncode != 0:
        return TestOutcome("FAIL", f"listing.py failed (rc={res.returncode}): {res.stdout}")
    try:
        issues = json.loads(res.stdout)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"listing.py returned invalid JSON: {exc}")
    if not isinstance(issues, list) or any(isinstance(i, dict) and i.get("level") == "error" for i in issues):
        return TestOutcome("FAIL", f"listing.py reported errors: {issues}")

    bad_json = work / "bad_listing.json"
    bad_json.write_text(json.dumps({"avoid": ["CorelDRAW"], "app_store": {"name": "CorelDRAW Clone", "description": "test"}}), encoding="utf-8")
    neg = run_cmd([sys.executable, str(script), str(bad_json), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if neg.returncode != 1:
        return TestOutcome("FAIL", f"listing.py negative avoid-term expected rc=1, got rc={neg.returncode}")
    return TestOutcome("PASS", "listing.py 0 errors/warnings + negative avoid-term check verified")


def _test_replica_parity(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    work = ctx.make_scratch_dir("T1-REPLICA-PARITY")
    script = ctx.replica_skills_dir / "replica-diff" / "parity.py"
    csv_p = ctx.repo_root / "replica" / "features.csv"
    res = run_cmd([sys.executable, str(script), str(csv_p), "--json"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if res.returncode != 0:
        return TestOutcome("FAIL", f"parity.py failed: {res.stderr}")
    try:
        rep = _require_dict(json.loads(res.stdout), "parity.py")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"parity.py returned invalid JSON: {exc}")
    if (
        rep.get("counted") != 86
        or rep.get("must_have_total") != 68
        or not isinstance(rep.get("skipped"), list)
        or len(rep["skipped"]) != 6
        or not isinstance(rep.get("extras"), list)
        or len(rep["extras"]) != 12
        or rep.get("problems") != []
    ):
        return TestOutcome("FAIL", f"parity.py invariant mismatch: {rep}")
    return TestOutcome("PASS", "parity.py verified 86 counted / 68 must / 6 skipped / 12 extras / 0 problems")


def _test_replica_sweep_app(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    script = ctx.replica_skills_dir / "replica-brand" / "sweep.py"
    brand = ctx.repo_root / "replica" / "brand.json"
    res = run_cmd(
        [sys.executable, str(script), "crates/coree-app", "--config", str(brand), "--json"],
        cwd=ctx.repo_root,
        timeout=20.0,
        timeout_scale=ctx.timeout_scale,
    )
    if res.returncode != 0:
        return TestOutcome("FAIL", f"sweep.py crates/coree-app failed (rc={res.returncode})")
    try:
        hits = json.loads(res.stdout)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"sweep.py crates/coree-app returned invalid JSON: {exc}")
    if hits:
        return TestOutcome("FAIL", f"Expected 0 hits in crates/coree-app, got {len(hits)}")
    return TestOutcome("PASS", "sweep.py crates/coree-app returned 0 hits (exit 0)")


# ---------------------------------------------------------------------------
# FUTURE-WAVE TESTS (smoke=False, target_wave="W1".."W5") — Honest FAIL until implemented
# ---------------------------------------------------------------------------

def _test_w1_sweep_crates(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    brand_sweep = ctx.repo_root / "scripts" / "brand_sweep.py"
    if not brand_sweep.is_file():
        return TestOutcome("FAIL", "W1 MND-01: scripts/brand_sweep.py MIME allowlist CI gate not yet present")

    bs_res = run_cmd(
        [sys.executable, str(brand_sweep), "--targets", "crates", "--require-external-sweep"],
        cwd=ctx.repo_root,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if bs_res.returncode != 0:
        return TestOutcome(
            "FAIL",
            f"W1 MND-01: scripts/brand_sweep.py --targets crates failed (rc={bs_res.returncode}): {bs_res.stderr or bs_res.stdout}",
        )

    script = ctx.replica_skills_dir / "replica-brand" / "sweep.py"
    brand = ctx.repo_root / "replica" / "brand.json"
    brand_cfg = json.loads(brand.read_text(encoding="utf-8"))

    res = run_cmd(
        [sys.executable, str(script), "crates", "--config", str(brand), "--json"],
        cwd=ctx.repo_root,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if res.returncode not in (0, 1):
        return TestOutcome(
            "FAIL",
            f"W1 MND-01: sweep.py crates failed (rc={res.returncode}): {res.stderr or res.stdout}",
        )
    try:
        hits = json.loads(res.stdout)
    except ValueError as exc:
        return TestOutcome("FAIL", f"W1 MND-01: sweep.py crates returned invalid JSON: {exc}")
    if not isinstance(hits, list):
        return TestOutcome("FAIL", "W1 MND-01: sweep.py crates did not return a JSON list")
    unallowlisted = _validate_sweep_hits(hits, brand_cfg, ctx.repo_root, target_prefix="crates")
    if unallowlisted:
        return TestOutcome(
            "FAIL",
            f"W1 MND-01: crates/ has {len(unallowlisted)} unallowlisted/non-MIME hits: {unallowlisted[:5]}",
            metrics={"unallowlisted_hits": len(unallowlisted)},
        )
    return TestOutcome(
        "PASS",
        f"scripts/brand_sweep.py --targets crates exit 0 (0 non-MIME hits; {len(hits)} allowlisted MIME hits)",
        metrics={"allowlisted_mime_hits": len(hits)},
    )


def _validate_sweep_hits(
    hits: list[dict[str, Any]],
    brand_cfg: dict[str, Any],
    repo_root: Path,
    *,
    target_prefix: str,
) -> list[str]:
    """Validate sweep.py hits under target_prefix/ against replica/brand.json mime_allowlist using full disk lines."""
    allow_cfg = brand_cfg.get("mime_allowlist")
    if not isinstance(allow_cfg, dict):
        return ["missing mime_allowlist in brand.json"]
    raw_literals = allow_cfg.get("literals", [])
    raw_allowed_files = allow_cfg.get("allowed_files", [])
    literals = sorted({str(x) for x in raw_literals if str(x)}, key=len, reverse=True)
    allowed_files = {str(x).replace("\\", "/") for x in raw_allowed_files if str(x)}
    if not literals or not allowed_files:
        return ["empty mime_allowlist.literals or allowed_files in brand.json"]

    mime_left = r"(?<![A-Za-z0-9_.\-+/])"
    mime_right = r"(?![A-Za-z0-9_.\-+/])"
    literal_patterns = [
        (lit, re.compile(f"{mime_left}{re.escape(lit)}{mime_right}"))
        for lit in literals
    ]

    hex_rx = re.compile(r"#(?:[0-9a-fA-F]{6}|[0-9a-fA-F]{3})(?![0-9a-zA-Z])")

    def _norm_hex(h: str) -> str:
        s = h.lower().lstrip("#")
        if len(s) == 3:
            s = "".join(c * 2 for c in s)
        return "#" + s

    pats: list[tuple[str, str, re.Pattern[str]]] = []
    for name in brand_cfg.get("avoid", []):
        name_s = str(name).strip()
        if not name_s:
            continue
        parts = [re.escape(p) for p in re.split(r"[\s_-]+", name_s) if p]
        pats.append(("name", name_s, re.compile(r"[\s_-]?".join(parts), re.I)))
    for dom in brand_cfg.get("domains", []):
        dom_s = str(dom).strip().lower()
        if dom_s:
            pats.append(("domain", dom_s, re.compile(re.escape(dom_s), re.I)))
    pat_by_key = {(k, lbl): rx for k, lbl, rx in pats}
    cols = {_norm_hex(str(c)) for c in brand_cfg.get("colors", []) if hex_rx.fullmatch(str(c).strip())}

    violations: list[str] = []
    line_cache: dict[Path, list[str]] = {}
    for h in hits:
        if not isinstance(h, dict):
            violations.append(f"malformed sweep hit entry (expected object, got {type(h).__name__})")
            continue
        rel_path = os.path.normpath(os.path.join(target_prefix, str(h.get("file", "")))).replace("\\", "/")
        kind = str(h.get("kind", ""))
        raw_line = h.get("line", 0)
        if isinstance(raw_line, bool):
            line_no = 0
        else:
            try:
                line_no = int(raw_line or 0)
            except (TypeError, ValueError):
                line_no = 0
        match_label = str(h.get("match", ""))

        if not (rel_path == target_prefix or rel_path.startswith(target_prefix + "/")):
            violations.append(f"{rel_path}:{line_no} path traversal outside {target_prefix}/ ({match_label})")
            continue
        if kind != "name":
            violations.append(f"{rel_path}:{line_no} non-name hit ({kind}:{match_label})")
            continue
        if line_no <= 0:
            violations.append(f"{rel_path}:{line_no} invalid line hit ({match_label})")
            continue
        if rel_path not in allowed_files:
            violations.append(f"{rel_path}:{line_no} unallowlisted file ({match_label})")
            continue

        full_file = repo_root / rel_path
        if full_file not in line_cache:
            try:
                line_cache[full_file] = full_file.read_text(encoding="utf-8", errors="replace").splitlines()
            except OSError as exc:
                violations.append(f"{rel_path}:{line_no} unreadable file: {exc}")
                continue
        file_lines = line_cache[full_file]
        if line_no > len(file_lines):
            violations.append(f"{rel_path}:{line_no} line number out of range")
            continue

        stripped = file_lines[line_no - 1]
        matched_literals: list[str] = []
        for lit, lit_rx in literal_patterns:
            stripped, count = lit_rx.subn(" ", stripped)
            if count > 0:
                matched_literals.append(lit)

        if not matched_literals:
            violations.append(f"{rel_path}:{line_no} no delimited MIME literal on line")
            continue
        hit_rx = pat_by_key.get((kind, match_label))
        if hit_rx is None or not any(hit_rx.search(lit) for lit in matched_literals):
            violations.append(f"{rel_path}:{line_no} hit '{match_label}' not inside matched MIME literals")
            continue

        remainder_matches = [f"{pk}:{pl}" for pk, pl, rx in pats if rx.search(stripped)]
        if cols:
            for m in hex_rx.finditer(stripped):
                if _norm_hex(m.group(0)) in cols:
                    remainder_matches.append(f"color:{_norm_hex(m.group(0))}")
        if remainder_matches:
            violations.append(f"{rel_path}:{line_no} disallowed remainder {remainder_matches}")

    return violations


def _validate_packaging_sweep_hits(
    hits: list[dict[str, Any]],
    brand_cfg: dict[str, Any],
    repo_root: Path,
) -> list[str]:
    """Validate packaging/ sweep.py hits against replica/brand.json mime_allowlist using full disk lines."""
    return _validate_sweep_hits(hits, brand_cfg, repo_root, target_prefix="packaging")


def _test_w1_sweep_packaging_and_brand_md(ctx: RunContext) -> TestOutcome:
    if ctx.replica_skills_dir is None:
        return TestOutcome("FAIL", "Replica skills directory not found")
    brand_sweep = ctx.repo_root / "scripts" / "brand_sweep.py"
    if not brand_sweep.is_file():
        return TestOutcome("FAIL", "W1 MND-01: scripts/brand_sweep.py MIME allowlist CI gate not yet present")

    bs_res = run_cmd(
        [sys.executable, str(brand_sweep), "--require-external-sweep"],
        cwd=ctx.repo_root,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if bs_res.returncode != 0:
        return TestOutcome("FAIL", f"W1 MND-01: scripts/brand_sweep.py failed (rc={bs_res.returncode}): {bs_res.stderr or bs_res.stdout}")

    script = ctx.replica_skills_dir / "replica-brand" / "sweep.py"
    brand = ctx.repo_root / "replica" / "brand.json"
    res = run_cmd(
        [sys.executable, str(script), "packaging", "--config", str(brand), "--json"],
        cwd=ctx.repo_root,
        timeout=25.0,
        timeout_scale=ctx.timeout_scale,
    )
    if res.returncode not in (0, 1):
        return TestOutcome(
            "FAIL",
            f"W1 MND-01: sweep.py packaging failed (rc={res.returncode}): {res.stderr or res.stdout}",
        )
    try:
        hits = json.loads(res.stdout)
    except ValueError as exc:
        return TestOutcome("FAIL", f"W1 MND-01: sweep.py packaging returned invalid JSON: {exc}")
    if not isinstance(hits, list):
        return TestOutcome("FAIL", "W1 MND-01: sweep.py packaging did not return a JSON list")

    brand_cfg = json.loads(brand.read_text(encoding="utf-8"))
    non_mime = _validate_packaging_sweep_hits(hits, brand_cfg, ctx.repo_root)
    if non_mime:
        return TestOutcome(
            "FAIL",
            f"W1 MND-01: packaging/ has {len(non_mime)} non-MIME brand hits: {non_mime[:5]}",
            metrics={"non_mime": len(non_mime)},
        )

    ci_lines = [
        ln.split("#", 1)[0]
        for ln in _read_crates_text(ctx, ".github/workflows/ci.yml").splitlines()
    ]
    if not any("brand_sweep.py" in ln for ln in ci_lines):
        return TestOutcome("FAIL", "W1 MND-01: .github/workflows/ci.yml does not invoke scripts/brand_sweep.py")

    brand_md = _read_crates_text(ctx, "replica/brand.md")
    if "pending" not in brand_md.lower() and "trademark" not in brand_md.lower():
        return TestOutcome("FAIL", "W1 MND-01: replica/brand.md missing documented trademark review log")
    return TestOutcome("PASS", f"scripts/brand_sweep.py exit 0 + packaging/ 0 non-MIME hits ({len(hits)} allowlisted) + ci.yml verified")


def _extract_drag_state_match_arms(code: str) -> list[str]:
    """Extract brace-balanced match-arm and `if/while` bodies for `CanvasDragState::` patterns."""
    arms: dict[tuple[int, int], str] = {}
    n = len(code)
    if_header_spans: list[tuple[int, int]] = []

    def _is_kw_at(pos: int, kw: str) -> bool:
        end = pos + len(kw)
        if code[pos:end] != kw:
            return False
        if pos > 0 and (code[pos - 1].isalnum() or code[pos - 1] == "_"):
            return False
        if end < n and (code[end].isalnum() or code[end] == "_"):
            return False
        return True

    def _brace_depth_between(start: int, end: int) -> int:
        d = 0
        for idx in range(start, end):
            if code[idx] == "{":
                d += 1
            elif code[idx] == "}":
                d -= 1
        return d

    # 1. Extract `{ ... }` bodies of `if` / `while` statements (arbitrary-length `let`-chains,
    #    struct patterns `let Foo { id } = ...`, `if matches!(...)`), stopping on `=>` at depth 0
    #    so `match` arm `if`/`if let` guards are left for the match-arm scanner below.
    for m in re.finditer(r"(?<!\.)\b(?:if|while)\b", code):
        i = m.end()
        paren = 0
        bracket = 0
        brace = 0
        in_let_pat = False
        body_start = -1
        while i < n:
            ch = code[i]
            if paren == 0 and bracket == 0 and brace == 0 and _is_kw_at(i, "let"):
                in_let_pat = True
                i += 3
                continue
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren -= 1
                if paren < 0:
                    break
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket -= 1
                if bracket < 0:
                    break
            elif ch == "=" and paren == 0 and bracket == 0 and brace == 0:
                if i + 1 < n and code[i + 1] == ">":
                    break
                prev_ch = code[i - 1] if i > 0 else ""
                next_ch = code[i + 1] if i + 1 < n else ""
                if in_let_pat and prev_ch not in ("=", "!", "<", ">") and next_ch != "=":
                    in_let_pat = False
            elif ch == "{":
                if paren == 0 and bracket == 0 and brace == 0 and not in_let_pat:
                    cond = code[m.end():i]
                    if re.search(r"\bCanvasDragState::", cond):
                        body_start = i
                        if_header_spans.append((m.start(), i))
                    break
                brace += 1
            elif ch == "}":
                brace -= 1
                if brace < 0:
                    break
            elif ch == ";" and paren == 0 and bracket == 0 and brace == 0:
                break
            i += 1
        if body_start >= 0:
            d = 1
            k = body_start + 1
            while k < n and d > 0:
                if code[k] == "{":
                    d += 1
                elif code[k] == "}":
                    d -= 1
                k += 1
            arms[(body_start, k)] = code[body_start:k]

    # 2. Extract match-arm RHS (`{ ... }` or comma-terminated expr) for `CanvasDragState::`
    #    in match-arm patterns and `match` arm `if` / `if let` guards.
    cur = 0
    base_paren = 0
    base_bracket = 0
    pk_stack: list[tuple[int, int]] = []
    for m in re.finditer(r"\bCanvasDragState::", code):
        while cur < m.start():
            c = code[cur]
            if c == "{":
                pk_stack.append((base_paren, base_bracket))
                base_paren = 0
                base_bracket = 0
            elif c == "}":
                if pk_stack:
                    base_paren, base_bracket = pk_stack.pop()
                else:
                    base_paren = 0
                    base_bracket = 0
            elif c == "(":
                base_paren += 1
            elif c == ")":
                base_paren = max(0, base_paren - 1)
            elif c == "[":
                base_bracket += 1
            elif c == "]":
                base_bracket = max(0, base_bracket - 1)
            cur += 1
        if any(s <= m.start() < e and _brace_depth_between(s, m.start()) == 0 for s, e in if_header_spans):
            continue
        i = m.start()
        brace = 0
        paren = base_paren
        bracket = base_bracket
        arrow_end = -1
        while i < n:
            ch = code[i]
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren = max(0, paren - 1)
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket = max(0, bracket - 1)
            elif ch == "{":
                brace += 1
            elif ch == "}":
                brace -= 1
                if brace < 0:
                    break
            elif ch in (";", ",") and brace == 0 and paren == 0 and bracket == 0:
                break
            elif ch == "=" and brace == 0 and paren == 0 and bracket == 0:
                if i + 1 < n and code[i + 1] == ">":
                    arrow_end = i + 2
                    break
            i += 1
        if arrow_end < 0:
            continue
        j = arrow_end
        while j < n and code[j].isspace():
            j += 1
        if j >= n:
            continue
        if code[j] == "{":
            d = 1
            k = j + 1
            while k < n and d > 0:
                if code[k] == "{":
                    d += 1
                elif code[k] == "}":
                    d -= 1
                k += 1
            arms[(j, k)] = code[j:k]
        else:
            d_b = d_p = d_k = 0
            k = j
            while k < n:
                ch = code[k]
                if ch == "{":
                    d_b += 1
                elif ch == "}":
                    if d_b == 0 and d_p == 0 and d_k == 0:
                        break
                    d_b -= 1
                elif ch == "(":
                    d_p += 1
                elif ch == ")":
                    d_p -= 1
                elif ch == "[":
                    d_k += 1
                elif ch == "]":
                    d_k -= 1
                elif ch == "," and d_b == 0 and d_p == 0 and d_k == 0:
                    break
                k += 1
            arms[(j, k)] = code[j:k]
    return list(arms.values())


def _test_w1_coree_tools_separation(ctx: RunContext) -> TestOutcome:
    ct_toml = ctx.repo_root / "crates" / "coree-tools" / "Cargo.toml"
    if not ct_toml.is_file():
        return TestOutcome("FAIL", "W1 MND-02: crates/coree-tools/Cargo.toml does not exist yet")
    manifest = tomllib.loads(ct_toml.read_text(encoding="utf-8"))
    deps: set[str] = set()
    for sec in ("dependencies", "dev-dependencies", "build-dependencies"):
        tbl = manifest.get(sec)
        if isinstance(tbl, dict):
            deps.update(str(k).lower() for k in tbl)
    target_tbl = manifest.get("target")
    if isinstance(target_tbl, dict):
        for target_cfg in target_tbl.values():
            if isinstance(target_cfg, dict):
                for sec in ("dependencies", "dev-dependencies", "build-dependencies"):
                    tbl = target_cfg.get(sec)
                    if isinstance(tbl, dict):
                        deps.update(str(k).lower() for k in tbl)
    if "egui" in deps or "eframe" in deps:
        return TestOutcome("FAIL", f"W1 MND-02: coree-tools/Cargo.toml must not depend on egui/eframe (deps={sorted(deps)})")

    canvas_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs")
    if not canvas_code:
        return TestOutcome("FAIL", "W1 MND-02: canvas.rs source missing")
    get_mut_cnt = len(re.findall(r"\b(?:objects\s*\.\s*get_mut|object_mut|[A-Za-z0-9_]*objects_mut)\s*\(", canvas_code))
    if get_mut_cnt > 0:
        return TestOutcome("FAIL", f"W1 MND-02: canvas.rs still contains {get_mut_cnt} direct mutable objects calls")

    # Verify 0 mark_all_dirty() calls inside drag / CanvasDragState match-arm blocks
    drag_arms = _extract_drag_state_match_arms(canvas_code)
    drag_mark_dirty = sum(1 for arm in drag_arms if "mark_all_dirty" in arm)
    if drag_mark_dirty > 0:
        return TestOutcome("FAIL", f"W1 MND-02: canvas.rs drag handlers still call mark_all_dirty() ({drag_mark_dirty} drag match arms)")

    fail = _run_cargo_tests(ctx, ["-p", "coree-tools"], "W1 MND-02", timeout=120.0)
    if fail is not None:
        return fail
    return TestOutcome("PASS", "coree-tools (no egui) cargo test passed + 0 objects.get_mut & 0 drag mark_all_dirty() in canvas.rs")


def _test_w1_fuzz_and_real_cdr_pack(ctx: RunContext) -> TestOutcome:
    fuzz_dir = ctx.repo_root / "fuzz"
    real_cdr_dir = ctx.repo_root / "crates" / "coree-cdr" / "tests" / "fixtures_real_cdr"
    fuzz_targets = list((fuzz_dir / "fuzz_targets").glob("*.rs")) if (fuzz_dir / "fuzz_targets").is_dir() else []
    real_cdrs = sorted(real_cdr_dir.glob("*.cdr")) if real_cdr_dir.is_dir() else []
    if not fuzz_targets or not real_cdrs or not (fuzz_dir / "Cargo.toml").is_file():
        return TestOutcome(
            "FAIL",
            f"W1-W3 MND-04: fuzz_targets={len(fuzz_targets)}, fixtures_real_cdr={len(real_cdrs)} (both required > 0)",
        )
    fc = _run_cargo(ctx, ["check", "--manifest-path", str(fuzz_dir / "Cargo.toml")], timeout=120.0)
    if fc.returncode != 0:
        return TestOutcome("FAIL", f"W1 MND-04: cargo check fuzz/ failed: {fc.stderr[-600:]}")
    work = ctx.make_scratch_dir("T1-W1-FUZZ-PACK")
    for cdr_p in real_cdrs[:5]:
        ir = run_cmd([str(ctx.cli_bin), "inspect", str(cdr_p), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        if ir.returncode != 0:
            return TestOutcome("FAIL", f"W1 MND-04: coree-cli inspect failed on real CDR {cdr_p.name}: {ir.stderr}")
        try:
            rep = _require_dict(json.loads(ir.stdout), f"inspect {cdr_p.name}")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W1 MND-04: coree-cli inspect --json returned invalid JSON for {cdr_p.name}: {exc}")
        pc = rep.get("page_count")
        oc = rep.get("object_count")
        if not isinstance(pc, int) or isinstance(pc, bool) or not isinstance(oc, int) or isinstance(oc, bool) or pc < 1 or oc < 1:
            return TestOutcome(
                "FAIL",
                f"W1 MND-04: inspect --json on real CDR {cdr_p.name} reported empty/invalid scene "
                f"(page_count={pc}, object_count={oc})",
            )
    return TestOutcome("PASS", f"Compiled {len(fuzz_targets)} fuzz targets & inspected {len(real_cdrs)} real .cdr fixtures")


def _test_w1_version_sync_and_ci(ctx: RunContext) -> TestOutcome:
    verify_script = ctx.repo_root / "scripts" / "verify_invariants.py"
    if not verify_script.is_file():
        return TestOutcome("FAIL", "W1 MND-05: scripts/verify_invariants.py CI gate not yet created")

    v_res = run_cmd(
        [sys.executable, str(verify_script)],
        cwd=ctx.repo_root,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    if v_res.returncode != 0:
        return TestOutcome("FAIL", f"W1 MND-05: scripts/verify_invariants.py failed (rc={v_res.returncode}): {v_res.stderr or v_res.stdout}")

    cargo_txt = _read_crates_text(ctx, "Cargo.toml")
    m_cargo = re.search(r'(?m)^version\s*=\s*"([^"]+)"', cargo_txt)
    rc_txt = _read_crates_text(ctx, "packaging/windows/coree.rc")
    m_rc = re.search(r'VALUE\s+"ProductVersion",\s*"([^"\\]+)', rc_txt)
    wxs_txt = _read_crates_text(ctx, "packaging/windows/wix/main.wxs")
    m_wxs = re.search(r'define\s+Version\s*=\s*"([^"]+)"', wxs_txt)
    cl_txt = _read_crates_text(ctx, "CHANGELOG.md")
    m_cl = re.search(r'(?m)^##\s+\[([^\]]+)\]', cl_txt)
    meta_txt = _read_crates_text(ctx, "packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml")
    m_meta = re.search(r'<release\s+version="([^"]+)"', meta_txt)
    versions = {
        "Cargo.toml": m_cargo.group(1) if m_cargo else None,
        "coree.rc": m_rc.group(1) if m_rc else None,
        "main.wxs": m_wxs.group(1) if m_wxs else None,
        "CHANGELOG.md": m_cl.group(1) if m_cl else None,
        "metainfo.xml": m_meta.group(1) if m_meta else None,
    }
    if len(set(versions.values())) != 1 or None in versions.values():
        return TestOutcome("FAIL", f"W1 MND-05: Version mismatch across manifests: {versions}")

    ci_lines = [
        ln.split("#", 1)[0]
        for ln in _read_crates_text(ctx, ".github/workflows/ci.yml").splitlines()
    ]
    if not any("verify_invariants.py" in ln for ln in ci_lines):
        return TestOutcome("FAIL", "W1 MND-05: .github/workflows/ci.yml does not invoke scripts/verify_invariants.py")
    return TestOutcome("PASS", f"verify_invariants.py exit 0 & version synced ({versions['Cargo.toml']}) across manifests + ci.yml")


def _test_w1_command_ops_and_delta(ctx: RunContext) -> TestOutcome:
    cmd_code = _read_rust_stripped(ctx, "crates/coree-core/src/command.rs")
    prod_code = _read_rust_stripped(ctx, "crates/coree-app/src/state/production.rs")
    doc_code = _read_rust_stripped(ctx, "crates/coree-core/src/document.rs")

    op_body = _extract_brace_block(cmd_code, r"\bpub\s+enum\s+CommandOp\s*\{")
    if not op_body:
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01: pub enum CommandOp not found in command.rs")
    missing = [op for op in ("FindAndReplace", "DuplicateObjects", "ReorderPages", "FitPowerClip") if not re.search(rf"\b{op}\b", op_body)]
    if missing:
        return TestOutcome("FAIL", f"[Blocker][VGCore] W1 REQ-01: CommandOp missing variants: {missing}")
    delta_block = _extract_brace_block(cmd_code, r"\bstruct\s+DocumentDelta\b")
    if not delta_block:
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01: struct DocumentDelta missing in command.rs")
    missing_delta = [
        fld for fld in ("color_styles", "object_styles", "symbols", "custom_icc_profile", "perspective_grid", "assets")
        if not re.search(rf"\b{fld}\s*:", delta_block)
    ]
    if missing_delta:
        return TestOutcome("FAIL", f"[Blocker][VGCore] W1 REQ-01: DocumentDelta missing fields: {missing_delta}")
    if not re.search(r"\bstruct\s+DocumentStateSnapshot\b", cmd_code):
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01: struct DocumentStateSnapshot missing in command.rs")

    doc_struct = _extract_brace_block(doc_code, r"\bpub\s+struct\s+Document\s*\{")
    if not doc_struct:
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01/REM-10: pub struct Document block not found in document.rs")
    pub_doc_fields = re.findall(r"\bpub(?:\s*\([^)]*\))?\s+([A-Za-z_][A-Za-z0-9_]*)\s*:", doc_struct)
    if pub_doc_fields:
        return TestOutcome("FAIL", f"[Blocker][VGCore] W1 REQ-01/REM-10: Document still has public fields {pub_doc_fields} (breaks encapsulation)")

    bfq_body = _extract_brace_block(prod_code, r"\bfn\s+build_find_query\b")
    if not bfq_body:
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01: fn build_find_query not found in production.rs")
    stub_fields = re.findall(r"\b(fill_color|outline_color|layer_id|layer_name)\s*:\s*(?:Option::)?None\b", bfq_body)
    stub_shorthand = re.findall(
        r"\blet\s+(?:mut\s+)?(fill_color|outline_color|layer_id|layer_name)(?:\s*:[^=;]+)?\s*=\s*(?:Option::)?None\s*;",
        bfq_body,
    )
    has_default_stub = bool(re.search(r"\b(?:FindQuery::)?default\s*\(\s*\)|\bDefault::default\s*\(\s*\)", bfq_body))
    if stub_fields or stub_shorthand or has_default_stub:
        return TestOutcome(
            "FAIL",
            f"[Blocker][VGCore] W1 REQ-01: build_find_query still hardcodes None/Default for "
            f"fields={stub_fields}, shorthand={stub_shorthand}, default={has_default_stub}",
        )

    rf_body = _extract_brace_block(prod_code, r"\bfn\s+run_find_and_replace_action\b")
    if not rf_body:
        return TestOutcome("FAIL", "[Blocker][VGCore] W1 REQ-01: fn run_find_and_replace_action not found in production.rs")
    if "CommandOp::FindAndReplace" not in rf_body or re.search(r"\bfind_and_replace\s*\(\s*&mut\s+tab\.document\b", rf_body):
        return TestOutcome(
            "FAIL",
            "[Blocker][VGCore] W1 REQ-01: run_find_and_replace_action bypasses CommandOp::FindAndReplace (direct &mut tab.document mutation)",
        )

    fail = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "command"], "[Blocker][VGCore] W1 REQ-01")
    if fail is not None:
        return fail
    return TestOutcome("PASS", "VGCore CommandOp + DocumentDelta + encapsulation + cargo test verified")


def _test_w1_live_vector_clipboard(ctx: RunContext) -> TestOutcome:
    cb = ctx.repo_root / "crates" / "coree-io" / "src" / "clipboard_codec.rs"
    if not cb.is_file():
        return TestOutcome("FAIL", "W1 REQ-09: crates/coree-io/src/clipboard_codec.rs not yet implemented")
    code = strip_rust_comments_and_strings(cb.read_text(encoding="utf-8"))
    for fmt in ("svg", "pdf", "emf"):
        if not re.search(rf"\b[A-Za-z_]*{fmt}[A-Za-z_]*\b", code, re.I):
            return TestOutcome("FAIL", f"W1 REQ-09: clipboard_codec.rs missing {fmt.upper()} symbol/codec")
    fail = _run_cargo_tests(ctx, ["-p", "coree-io", "--", "clipboard"], "W1 REQ-09")
    if fail is not None:
        return fail
    return TestOutcome("PASS", "Live Vector Clipboard codec (SVG/PDF/EMF) cargo test passed")


def _test_w1_ipc_uds_and_modularize(ctx: RunContext) -> TestOutcome:
    ipc_code = _read_rust_stripped(ctx, "crates/coree-app/src/ipc.rs")
    if not re.search(r"\b(?:UnixListener|UnixStream|NamedPipe)\b", ipc_code):
        return TestOutcome("FAIL", "W1 REQ-10: ipc.rs still uses loopback TCP instead of Unix Domain Socket (0600) / NamedPipe")
    if re.search(r"\b(?:TcpListener|TcpStream|std::net)\b", ipc_code):
        return TestOutcome("FAIL", "W1 REQ-10: ipc.rs still references std::net / TcpListener / TcpStream")
    verify_script = ctx.repo_root / "scripts" / "verify_invariants.py"
    if verify_script.is_file():
        vr = run_cmd(
            [sys.executable, str(verify_script), "--strict-no-std-net"],
            cwd=ctx.repo_root,
            timeout=30.0,
            timeout_scale=ctx.timeout_scale,
        )
        if vr.returncode != 0:
            return TestOutcome("FAIL", f"W1 REQ-10: verify_invariants.py --strict-no-std-net failed:\n{vr.stderr or vr.stdout}")
    fail = _run_cargo_tests(ctx, ["-p", "coree-app", "--", "ipc"], "W1 REQ-10")
    if fail is not None:
        return fail
    return TestOutcome("PASS", "ipc.rs UDS/NamedPipe verified (--strict-no-std-net + cargo test)")


def _test_w1_autosave_wal_and_quarantine(ctx: RunContext) -> TestOutcome:
    db_code = _read_rust_stripped(ctx, "crates/coree-storage/src/db.rs")
    auto_code = _read_rust_stripped(ctx, "crates/coree-storage/src/autosave.rs")
    if not re.search(r"\bquarantine\b", db_code + "\n" + auto_code, re.I):
        return TestOutcome("FAIL", "W1 IMP-07: coree-storage db.rs/autosave.rs missing corrupt SQLite quarantine")
    if not re.search(r"\bdifferential\b", auto_code, re.I):
        return TestOutcome("FAIL", "W1 IMP-07: autosave.rs missing async differential autosave")
    fail_q = _run_cargo_tests(ctx, ["-p", "coree-storage", "--", "quarantine"], "W1 IMP-07 quarantine")
    if fail_q is not None:
        return fail_q
    fail_d = _run_cargo_tests(ctx, ["-p", "coree-storage", "--", "differential"], "W1 IMP-07 differential")
    if fail_d is not None:
        return fail_d
    return TestOutcome("PASS", "SQLite WAL + async differential autosave + corrupt DB quarantine + cargo tests verified")


def _test_w1_modularize_and_arch(ctx: RunContext) -> TestOutcome:
    arch_txt = _read_crates_text(ctx, "replica/architecture.md")
    if "coree-tools" not in arch_txt or "crates/coree-tools" not in arch_txt:
        return TestOutcome("FAIL", "[Major] W1 IMP-11: replica/architecture.md not yet reconciled with crates/coree-tools")
    for rel_monolith in (
        "crates/coree-app/src/state/production.rs",
        "crates/coree-app/src/ui/canvas.rs",
        "crates/coree-app/src/ui/dockers.rs",
        "crates/coree-app/src/ui/toolbar.rs",
        "crates/coree-app/src/ui/dialogs.rs",
        "crates/coree-cdr/src/cdr_riff.rs",
    ):
        p = ctx.repo_root / rel_monolith
        if p.is_file():
            n_lines = len(p.read_text(encoding="utf-8", errors="replace").splitlines())
            if n_lines > 1800:
                return TestOutcome("FAIL", f"[Major] W1 IMP-11: {rel_monolith} ({n_lines} lines > 1800) not yet modularized")
    return TestOutcome("PASS", "replica/architecture.md reconciled & bloated state/UI/CDR modules split")


def _test_w1_benches_init(ctx: RunContext) -> TestOutcome:
    benches_dir = ctx.repo_root / "benches"
    bench_files = list(benches_dir.glob("*.rs")) if benches_dir.is_dir() else []
    cargo_toml = _read_crates_text(ctx, "Cargo.toml")
    if not bench_files or "[[bench]]" not in cargo_toml:
        return TestOutcome("FAIL", "W1 IMP-12: benches/*.rs or [[bench]] entries in Cargo.toml not yet initialized")
    bc = _run_cargo(ctx, ["check", "--benches"], timeout=120.0)
    if bc.returncode != 0:
        return TestOutcome("FAIL", f"W1 IMP-12: cargo check --benches failed:\n{bc.stderr[-500:]}")
    return TestOutcome("PASS", f"Compiled {len(bench_files)} offline Criterion bench target(s)")


def _test_w2_cdr_writer_authentic(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W2-CDR-WRITER")
    src = _f(ctx, "fixture.coree")
    out_riff = work / "out_riff.cdr"
    out_x6 = work / "out_x6.cdr"
    for ver, out_p in (("riff", out_riff), ("x6", out_x6)):
        r = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_p), "--cdr-version", ver], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r.returncode != 0:
            return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] convert to {ver} failed: {r.stderr}")

    fix_x6 = dissect_cdr_zcf(_f(ctx, "fixture_x6.cdr").read_bytes())
    if (
        fix_x6["doc_palette_color_count"] < 2
        or fix_x6["cdss_key_count"] < 1
        or fix_x6["unindirected_loda_count"] != 0
    ):
        return TestOutcome("ERROR", f"Internal fixture_x6.cdr ZCF dissector invariant failed: {fix_x6}")

    try:
        riff = dissect_cdr_riff(out_riff.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: RIFF .cdr output malformed: {exc}")
    if not riff["has_20b_loda"]:
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: RIFF .cdr writer emitted synthetic 12B loda instead of authentic 20B reversed loda: {riff['loda_headers']}")
    try:
        zcf = dissect_cdr_zcf(out_x6.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: X6 .cdr ZCF output malformed: {exc}")
    if (
        not zcf["has_cdro_root"]
        or not zcf["has_valid_indirect_pointers"]
        or zcf["external_pointer_count"] < 1
        or zcf["invalid_pointer_count"] != 0
        or zcf["unindirected_loda_count"] != 0
        or not zcf["has_20b_deref_loda"]
        or zcf["deref_12b_loda_count"] != 0
        or not zcf["has_cdss"]
        or zcf["cdss_key_count"] < 1
        or not zcf["has_metadata_xml"]
        or not zcf["has_doc_palette_xml"]
        or zcf["doc_palette_color_count"] < 2
    ):
        return TestOutcome(
            "FAIL",
            f"[Blocker][RIFF/ZCF] W2 MND-03: X6 .cdr writer missing authentic CDRO + valid external 16B indirect pointers to 20B loda (0 unindirected / 0 12B deref loda) + non-empty cdss/docPalette.xml/metadata.xml: {zcf}",
        )

    # Verify non-flattening export + re-import round-trip for Table, Dimension, Connector, SymbolInstance, DropShadow, TwoColorPattern
    _, nf_doc = inspect_coree_zip(src)
    base_outline = nf_doc["objects"]["3"]["outline"]
    base_text_style = nf_doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"]
    sym_uuid = "44444444-4444-4444-8444-444444444444"
    sym_rect = dict(nf_doc["objects"]["3"])
    sym_rect["id"] = 20
    sym_rect["name"] = "Symbol Child Rect"
    sym_rect["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]}
    nf_doc["symbols"] = {
        sym_uuid: {
            "id": sym_uuid,
            "name": "BadgeSymbol",
            "objects": [sym_rect],
        }
    }
    nf_doc["objects"]["3"]["effects"] = [
        {
            "DropShadow": {
                "dx_mm": 2.0,
                "dy_mm": 2.0,
                "blur_mm": 3.0,
                "opacity": 0.6,
                "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}},
                "feather_direction": "Outside",
            }
        }
    ]
    nf_doc["objects"]["4"]["fill"] = {
        "TwoColorPattern": {
            "fg": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}},
            "bg": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}},
            "pattern_name": "DiagonalStripe",
            "tile_w": 10.0,
            "tile_h": 10.0,
        }
    }
    nf_doc["objects"]["8"] = {
        "id": 8,
        "name": "Spec Table",
        "payload": {
            "Table": {
                "rows": 2,
                "cols": 2,
                "col_widths_mm": [25.0, 25.0],
                "row_heights_mm": [10.0, 10.0],
                "cells": [
                    {
                        "row": r_i,
                        "col": c_i,
                        "row_span": 1,
                        "col_span": 1,
                        "text": f"R{r_i}C{c_i}",
                        "text_style": base_text_style,
                        "fill": "None",
                        "padding_mm": 2.0,
                        "merged_into": None,
                    }
                    for r_i in range(2)
                    for c_i in range(2)
                ],
                "border_style": base_outline,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 120.0]},
        "fill": "None",
        "outline": base_outline,
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
    nf_doc["objects"]["9"] = {
        "id": 9,
        "name": "Width Dimension",
        "payload": {
            "Dimension": {
                "kind": "Horizontal",
                "p1": {"x": 20.0, "y": 20.0},
                "p2": {"x": 110.0, "y": 20.0},
                "p3": None,
                "offset_mm": 10.0,
                "precision": 2,
                "prefix": "",
                "suffix": " mm",
                "label_override": None,
                "text_style": base_text_style,
                "unit_override": None,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": base_outline,
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
    nf_doc["objects"]["10"] = {
        "id": 10,
        "name": "Box Connector",
        "payload": {
            "Connector": {
                "from_obj": 3,
                "from_anchor": "Right",
                "from_fallback": {"x": 110.0, "y": 47.5},
                "to_obj": 4,
                "to_anchor": "Left",
                "to_fallback": {"x": 108.0, "y": 60.0},
                "style": "RoundedRightAngle",
                "corner_radius_mm": 3.0,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": base_outline,
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
    nf_doc["objects"]["11"] = {
        "id": 11,
        "name": "Symbol Instance",
        "payload": {"SymbolInstance": sym_uuid},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 140.0, 140.0]},
        "fill": "None",
        "outline": base_outline,
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
    nf_doc["pages"][0]["layers"][0]["objects"].extend([8, 9, 10, 11])
    nf_doc["next_id"] = 12

    nf_in = work / "nonflat_in.coree"
    nf_cdr = work / "nonflat.cdr"
    nf_rt = work / "nonflat_rt.coree"
    _write_coree_from_doc(nf_in, "Non-Flattening CDR Test", nf_doc)
    r_nf1 = run_cmd([str(ctx.cli_bin), "convert", str(nf_in), str(nf_cdr), "--cdr-version", "x6"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_nf1.returncode != 0 or not nf_cdr.is_file():
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: non-flattening .coree -> .cdr failed: {r_nf1.stderr}")
    r_nf2 = run_cmd([str(ctx.cli_bin), "convert", str(nf_cdr), str(nf_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_nf2.returncode != 0 or not nf_rt.is_file():
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: .cdr -> .coree re-import failed: {r_nf2.stderr}")
    try:
        _, rt_doc = inspect_coree_zip(nf_rt)
        rt_objs = _require_dict(rt_doc.get("objects"), "non-flattening .cdr round-trip objects")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"[Blocker][RIFF/ZCF] W2 MND-03: .cdr -> .coree re-import output malformed: {exc}")
    payload_kinds = {
        next(iter(obj.get("payload", {}).keys()), "")
        for obj in rt_objs.values()
        if isinstance(obj, dict) and isinstance(obj.get("payload"), dict)
    }
    missing_structured = [k for k in ("Table", "Dimension", "Connector", "SymbolInstance") if k not in payload_kinds]
    if missing_structured:
        return TestOutcome(
            "FAIL",
            f"[Blocker][RIFF/ZCF] W2 MND-03: .cdr round-trip flattened structured objects {missing_structured} (found={sorted(payload_kinds)})",
        )
    if not rt_doc.get("symbols"):
        return TestOutcome("FAIL", "[Blocker][RIFF/ZCF] W2 MND-03: .cdr round-trip dropped symbols definitions")
    has_drop_shadow = any(
        isinstance(eff, dict) and "DropShadow" in eff
        for obj in rt_objs.values()
        if isinstance(obj, dict) and isinstance(obj.get("effects"), list)
        for eff in obj["effects"]
    )
    if not has_drop_shadow:
        return TestOutcome("FAIL", "[Blocker][RIFF/ZCF] W2 MND-03: .cdr round-trip dropped Effect::DropShadow")
    has_two_color_pat = any(
        isinstance(obj, dict) and isinstance(obj.get("fill"), dict) and "TwoColorPattern" in obj["fill"]
        for obj in rt_objs.values()
    )
    if not has_two_color_pat:
        return TestOutcome("FAIL", "[Blocker][RIFF/ZCF] W2 MND-03: .cdr round-trip flattened Fill::TwoColorPattern")

    return TestOutcome(
        "PASS",
        f"Authentic 20B loda + CDRO ZCF ({zcf['external_pointer_count']} ext ptrs, {zcf['deref_20b_loda_count']} 20B deref) + docPalette.xml + non-flattened Table/Dimension/Connector/Symbol/DropShadow/TwoColorPattern verified",
    )


def _test_w2_cdr_v3_wi_zcf_xml(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W2-CDR-LEGACY-XML")
    # 1. Runtime CLI test: X6 ZCF with Spot color palette must NOT fall back to black [0,0,0,1]
    # and XML parser must ignore commented-out nodes & decode XML entities cleanly.
    zcf_spot = work / "spot_zcf.cdr"
    src_x6 = _f(ctx, "fixture_x6.cdr")
    with zipfile.ZipFile(src_x6, "r") as zin, zipfile.ZipFile(zcf_spot, "w") as zout:
        zinfo_map = _validate_zip_infolist(zin)
        zbudget = [_ZIP_MAX_TOTAL_BYTES]
        for item in zin.infolist():
            if item.filename not in ("styles/document.cdss", "color/docPalette.xml"):
                zout.writestr(
                    item,
                    _bounded_zip_read_info(zin, zinfo_map[item.filename], remaining_budget=zbudget),
                )
        zout.writestr(
            "styles/document.cdss",
            json.dumps({
                "version": 16,
                "style": {
                    "~graphic~": {"fill": {"type": 1, "primaryColor": "SPOT,PantoneMagenta,100,0,100,0,0"}}
                },
            }),
        )
        zout.writestr(
            "color/docPalette.xml",
            (
                '<?xml version="1.0"?>\n'
                '<palette name="Doc">\n'
                '  <!-- <color cs="SPOT" name="CommentedOut" tints="0,0,0,1"/> -->\n'
                '  <colors>\n'
                '    <color cs="SPOT" name="PantoneMagenta" tints="0,100,0,0"/>\n'
                '    <color cs="CMYK" name="Teal &amp; Cyan" tints="100,0,25,0"/>\n'
                '    <color cs="CMYK" name="A > B" tints="0,50,100,0"/>\n'
                '  </colors>\n'
                '</palette>\n'
            ),
        )
    out_coree = work / "spot_out.coree"
    r_zcf = run_cmd([str(ctx.cli_bin), "convert", str(zcf_spot), str(out_coree)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_zcf.returncode != 0 or not out_coree.is_file():
        return TestOutcome("FAIL", f"W2 REQ-06: convert ZCF spot fixture failed: {r_zcf.stderr}")

    try:
        _, spot_doc = inspect_coree_zip(out_coree)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REQ-06: converted ZCF spot .coree malformed: {exc}")
    doc_pal = spot_doc.get("document_palette")
    swatches = doc_pal.get("swatches", []) if isinstance(doc_pal, dict) else []
    if not isinstance(swatches, list):
        swatches = []
    swatch_by_name = {
        sw.get("name"): (sw.get("color") if isinstance(sw.get("color"), dict) else {})
        for sw in swatches
        if isinstance(sw, dict)
    }
    if "CommentedOut" in swatch_by_name:
        return TestOutcome(
            "FAIL",
            "W2 REQ-06 / REM-06: docPalette.xml parser parsed commented-out <color> inside <!-- ... --> (brittle str::find)",
        )
    if "Teal & Cyan" not in swatch_by_name:
        return TestOutcome(
            "FAIL",
            f"W2 REQ-06 / REM-06: docPalette.xml did not decode XML entity swatch 'Teal & Cyan' (got={sorted(swatch_by_name)})",
        )
    if "A > B" not in swatch_by_name:
        return TestOutcome(
            "FAIL",
            f"W2 REQ-06 / REM-06: docPalette.xml broke on '>' inside attribute value 'A > B' (brittle str::find('>'), got={sorted(swatch_by_name)})",
        )
    pm_color = swatch_by_name.get("PantoneMagenta")
    pm_spot = pm_color.get("Spot") if isinstance(pm_color, dict) else None
    if not isinstance(pm_spot, dict):
        return TestOutcome(
            "FAIL",
            f"W2 REM-06: PantoneMagenta Spot color missing from document_palette: {swatch_by_name.get('PantoneMagenta')}",
        )
    fb = pm_spot.get("fallback_cmyk", [])
    if (
        not isinstance(fb, list)
        or len(fb) != 4
        or not all(isinstance(v, (int, float)) and not isinstance(v, bool) for v in fb)
        or all(abs(fb[i] - v) < 1e-4 for i, v in enumerate((0.0, 0.0, 0.0, 1.0)))
    ):
        return TestOutcome("FAIL", f"W2 REM-06: PantoneMagenta Spot fell back to black [0,0,0,1] or invalid fallback: {pm_spot}")
    if not all(abs(fb[i] - v) < 1e-4 for i, v in enumerate((0.0, 1.0, 0.0, 0.0))):
        return TestOutcome("FAIL", f"W2 REM-06: PantoneMagenta fallback_cmyk mismatch (expected [0,1,0,0]): {pm_spot}")

    spot_objs = spot_doc.get("objects")
    if not isinstance(spot_objs, dict):
        return TestOutcome("FAIL", f"W2 REQ-06: converted ZCF spot .coree missing objects dict: {type(spot_objs).__name__}")
    for obj in spot_objs.values():
        if not isinstance(obj, dict):
            continue
        fill = obj.get("fill")
        if isinstance(fill, dict) and isinstance(fill.get("Solid"), dict):
            spot = fill["Solid"].get("Spot")
            if isinstance(spot, dict) and spot.get("fallback_cmyk") == [0.0, 0.0, 0.0, 1.0]:
                return TestOutcome("FAIL", f"W2 REM-06: object Spot fill fell back to black [0,0,0,1]: {spot}")

    # 2. Verify zcf.rs uses real XML parser (comment-stripped) & no [0,0,0,1] Spot fallback
    zcf_no_comments = _read_rust_comments_only(ctx, "crates/coree-cdr/src/zcf.rs")
    zcf_code = _read_rust_stripped(ctx, "crates/coree-cdr/src/zcf.rs")
    if re.search(r"str::find|\.find\(\s*(?:[\"']\s*</?|[\"']>[\"']|&(?:open|close|needle))", zcf_no_comments):
        return TestOutcome("FAIL", "W2 REQ-06 / REM-06: zcf.rs still uses brittle str.find XML tag/attr scanning instead of quick-xml/roxmltree")
    if re.search(r"fallback_cmyk\s*:\s*\[\s*0\.0\s*,\s*0\.0\s*,\s*0\.0\s*,\s*1\.0\s*\]", zcf_code):
        return TestOutcome("FAIL", "W2 REM-06: zcf.rs still contains [0,0,0,1] Spot color black fallback")
    if not re.search(r"\b(?:quick_xml|roxmltree)\b", zcf_code):
        return TestOutcome("FAIL", "W2 REQ-06: zcf.rs does not use quick-xml or roxmltree")

    fail = _run_cargo_tests(ctx, ["-p", "coree-cdr", "--", "zcf"], "W2 REQ-06")
    if fail is not None:
        return fail
    return TestOutcome("PASS", "ZCF real XML parser + non-black Spot color + zcf cargo tests verified")


def _test_w2_unknown_chunks_passthrough(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W2-PASSTHROUGH")
    xdoc_magic = b"COREE_XDOC_PASSTHROUGH_MAGIC_v1"
    xobj_magic = b"COREE_XOBJ_PASSTHROUGH_MAGIC_v1"
    xlay_magic = b"COREE_XLAY_PASSTHROUGH_MAGIC_v1"
    xpag_magic = b"COREE_XPAG_PASSTHROUGH_MAGIC_v1"
    in_cdr = work / "with_unknown_chunks.cdr"
    in_cdr.write_bytes(build_cdr_riff_with_unknown_xdoc(xdoc_magic, xobj_magic, xlay_magic, xpag_magic))

    in_info = dissect_cdr_riff(in_cdr.read_bytes())
    if xdoc_magic not in in_info["payloads_by_fourcc"].get("XDOC", []):
        return TestOutcome("ERROR", "Internal error: synthetic RIFF input missing XDOC chunk")

    out = work / "rt.cdr"
    r = run_cmd([str(ctx.cli_bin), "convert", str(in_cdr), str(out), "--cdr-version", "riff"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0 or not out.is_file():
        return TestOutcome("FAIL", f"convert .cdr -> .cdr failed: {r.stderr}")

    try:
        out_info = dissect_cdr_riff(out.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REM-05: round-tripped RIFF .cdr output malformed: {exc}")
    pbf = out_info["payloads_by_fourcc"]
    checks = {
        "XDOC": xdoc_magic in pbf.get("XDOC", []),
        "XPAG": xpag_magic in pbf.get("XPAG", []),
        "XLAY": xlay_magic in pbf.get("XLAY", []),
        "XOBJ": xobj_magic in pbf.get("XOBJ", []),
    }
    if not all(checks.values()):
        return TestOutcome(
            "FAIL",
            f"W2 REM-05: RIFF unknown chunk passthrough dropped chunks across round-trip: {checks}",
        )

    # Also verify X6 ZCF unknown content/data/*.dat passthrough
    zcf_marker = b"COREE_ZCF_UNKNOWN_DAT_PASSTHROUGH_v1"
    in_x6 = work / "in_x6_unknown.cdr"
    with zipfile.ZipFile(_f(ctx, "fixture_x6.cdr"), "r") as zin, zipfile.ZipFile(in_x6, "w") as zout:
        zinfo_map = _validate_zip_infolist(zin)
        zbudget = [_ZIP_MAX_TOTAL_BYTES]
        for item in zin.infolist():
            zout.writestr(
                item,
                _bounded_zip_read_info(zin, zinfo_map[item.filename], remaining_budget=zbudget),
            )
        zout.writestr("content/data/custom_unknown.dat", zcf_marker)
    out_x6 = work / "rt_x6.cdr"
    r_x6 = run_cmd([str(ctx.cli_bin), "convert", str(in_x6), str(out_x6), "--cdr-version", "x6"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_x6.returncode != 0 or not out_x6.is_file():
        return TestOutcome("FAIL", f"convert X6 .cdr -> .cdr failed: {r_x6.stderr}")
    try:
        with zipfile.ZipFile(out_x6, "r") as zrt:
            zrt_info = _validate_zip_infolist(zrt)
            zrt_budget = [_ZIP_MAX_TOTAL_BYTES]
            names = list(zrt_info.keys())
            if (
                "content/data/custom_unknown.dat" not in zrt_info
                or _bounded_zip_read_info(
                    zrt, zrt_info["content/data/custom_unknown.dat"], remaining_budget=zrt_budget
                )
                != zcf_marker
            ):
                return TestOutcome(
                    "FAIL",
                    f"W2 REM-05: X6 ZCF unknown content/data/custom_unknown.dat dropped across round-trip (entries={names})",
                )
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REM-05: round-tripped X6 .cdr ZIP output malformed: {exc}")

    return TestOutcome("PASS", "Byte-exact UnknownChunkMap passthrough (XDOC/XPAG/XLAY/XOBJ + X6 data/*.dat) verified")


def _test_w2_cmx_cpt_full(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W2-CMX-CPT")
    # 1. Real CLI CMX export + RIFF dissection + disp/ixlr/ixpg + re-import round-trip
    out_cmx = work / "exported.cmx"
    r1 = run_cmd([str(ctx.cli_bin), "convert", str(_f(ctx, "fixture.coree")), str(out_cmx)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r1.returncode != 0 or not out_cmx.is_file():
        return TestOutcome("FAIL", f"W2 REM-07: convert .coree -> .cmx failed: {r1.stderr}")
    try:
        cmx_riff = dissect_cdr_riff(out_cmx.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REM-07: exported CMX malformed RIFF: {exc}")
    missing_cmx_chunks = [ck for ck in ("disp", "ixlr", "ixpg") if ck not in cmx_riff["chunks"]]
    if missing_cmx_chunks:
        return TestOutcome(
            "FAIL",
            f"W2 REM-07: Exported CMX missing {missing_cmx_chunks} chunk(s) (chunks={cmx_riff['chunks']})",
        )

    cmx_rt = work / "cmx_rt.coree"
    r_rt = run_cmd([str(ctx.cli_bin), "convert", str(out_cmx), str(cmx_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_rt.returncode != 0 or not cmx_rt.is_file():
        return TestOutcome("FAIL", f"W2 REM-07: re-import .cmx -> .coree failed: {r_rt.stderr}")
    r_ins = run_cmd([str(ctx.cli_bin), "inspect", str(cmx_rt), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_ins.returncode != 0:
        return TestOutcome("FAIL", f"W2 REM-07: inspect cmx_rt.coree failed: {r_ins.stderr}")
    try:
        cmx_rep = _require_dict(json.loads(r_ins.stdout), "inspect cmx_rt.coree")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REM-07: inspect cmx_rt.coree returned invalid JSON: {exc}")
    cmx_oc = cmx_rep.get("object_count")
    if not isinstance(cmx_oc, int) or isinstance(cmx_oc, bool) or cmx_oc < 3:
        return TestOutcome(
            "FAIL",
            f"W2 REM-07: CMX round-trip lost objects (object_count={cmx_oc}, expected >= 3)",
        )

    # 2. Real CLI CPT -> PNG raster render + active CPTObject check + CPT unit tests
    out_cpt_png = work / "cpt_render.png"
    r2 = run_cmd([str(ctx.cli_bin), "convert", str(_f(ctx, "fixture.cpt")), str(out_cpt_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r2.returncode != 0 or not out_cpt_png.is_file():
        return TestOutcome("FAIL", f"W2 REM-07: convert .cpt -> .png failed: {r2.stderr}")
    try:
        cpt_png = parse_png_info(out_cpt_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 REM-07: CPT converted PNG malformed: {exc}")
    if cpt_png["width"] < 32 or cpt_png["height"] < 32 or cpt_png["non_white_pixels"] < 20:
        return TestOutcome("FAIL", f"W2 REM-07: CPT converted PNG was blank or too small: {cpt_png}")

    cpt_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-cdr/src/cpt.rs"))
    if not re.search(r"\bCPTObject\b", cpt_code):
        return TestOutcome("FAIL", "W2 REM-07: cpt.rs missing CPTObject / 16-bit / mask support")
    cpt_fn_blocks = _extract_all_brace_blocks(cpt_code, r"\bfn\s+(\w+)\b")
    has_active_cpt_obj = any(
        not _is_trivial_stub_body(body)
        and re.search(r"\bCPTObject\s*(?:\{|::\s*(?:new|parse|from|decode|read)[A-Za-z0-9_]*\s*\()", body)
        for _m, body in cpt_fn_blocks
    )
    if not has_active_cpt_obj:
        return TestOutcome(
            "FAIL",
            "W2 REM-07: cpt.rs declares CPTObject without actively constructing/parsing it in a non-stub fn",
        )
    fail_cmx = _run_cargo_tests(ctx, ["-p", "coree-cdr", "--", "cmx"], "W2 REM-07 CMX", min_passed=1)
    if fail_cmx is not None:
        return fail_cmx
    fail_cpt = _run_cargo_tests(ctx, ["-p", "coree-cdr", "--", "cpt"], "W2 REM-07 CPT", min_passed=1)
    if fail_cpt is not None:
        return fail_cpt
    return TestOutcome("PASS", f"CMX disp/ixlr/ixpg round-trip (object_count={cmx_rep.get('object_count')}) + CPT render & CPTObject verified")


def _test_w2_curves_refit_and_continuity(ctx: RunContext) -> TestOutcome:
    geom_code = _read_rust_stripped(ctx, "crates/coree-core/src/geometry.rs")
    bool_code = _read_rust_stripped(ctx, "crates/coree-core/src/boolean.rs")
    if re.search(r"const\s+SEGMENTS\s*:\s*usize\s*=\s*96\b", geom_code):
        return TestOutcome("FAIL", "[Blocker][CDRCRV] W2 REM-01: erase_circle in geometry.rs still uses 96-segment polygon approximation")
    if not re.search(r"\bfn\s+enforce_continuity\b", geom_code):
        return TestOutcome("FAIL", "[Blocker][CDRCRV] W2 REM-10: PathNode::enforce_continuity missing in geometry.rs")

    for fn_name in ("knife_cut", "virtual_segment_delete"):
        fn_body = _extract_brace_block(geom_code, rf"\bfn\s+{fn_name}\b")
        if not fn_body or "fit_cubic_bezier_subpath" not in fn_body:
            return TestOutcome("FAIL", f"[Blocker][CDRCRV] W2 REM-01: {fn_name} in geometry.rs lacks cubic Bezier refit (fit_cubic_bezier_subpath)")

    stcp_body = _extract_brace_block(bool_code, r"\bfn\s+stroke_to_closed_path\b")
    if not stcp_body:
        return TestOutcome("FAIL", "[Blocker][CDRCRV] W2 REM-01/FEAT-35: fn stroke_to_closed_path missing in boolean.rs")
    missing_pen_attrs = [
        attr for attr in ("dash_pattern", "start_arrow", "end_arrow", "calligraphy_angle_deg", "calligraphy_stretch")
        if not re.search(rf"\b{attr}\b", stcp_body)
    ]
    if missing_pen_attrs:
        return TestOutcome(
            "FAIL",
            f"[Blocker][CDRCRV] W2 REM-01/FEAT-35: stroke_to_closed_path in boolean.rs ignores outline attributes {missing_pen_attrs}",
        )

    fail = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "geometry"], "[Blocker][CDRCRV] W2 REM-01")
    if fail is not None:
        return fail
    fail_st = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "stroke_to_closed_path"], "[Blocker][CDRCRV] W2 REM-01/FEAT-35")
    if fail_st is not None:
        return fail_st
    return TestOutcome("PASS", "Analytical erase_circle, knife/VSD cubic refit, stroke_to_closed_path (dash/arrows/calligraphy), PathNode continuity & cargo tests verified")


def _test_w2_convert_to_curves_cli(ctx: RunContext) -> TestOutcome:
    gate = require_cli_subcommand(ctx, "convert-to-curves", "W2")
    if gate is not None:
        return gate
    work = ctx.make_scratch_dir("T1-W2-CURVES-CLI")
    out = work / "outlined.coree"
    res = run_cmd([str(ctx.cli_bin), "convert-to-curves", str(_f(ctx, "fixture.coree")), str(out)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if res.returncode != 0 or not out.is_file():
        return TestOutcome("FAIL", f"convert-to-curves failed: {res.stderr}")
    try:
        _, doc = inspect_coree_zip(out)
        objs = _require_dict(doc.get("objects"), "convert-to-curves objects")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"convert-to-curves output .coree malformed: {exc}")
    curve_nodes_max = 0
    for obj in objs.values():
        if not isinstance(obj, dict):
            return TestOutcome("FAIL", f"convert-to-curves output object entry malformed: {obj!r}")
        payload = obj.get("payload")
        if not isinstance(payload, dict):
            continue
        if "ArtisticText" in payload:
            return TestOutcome("FAIL", "convert-to-curves left ArtisticText unconverted")
        path_val = payload.get("Path")
        if isinstance(path_val, dict) and isinstance(path_val.get("subpaths"), list):
            for sp in path_val["subpaths"]:
                if isinstance(sp, dict) and isinstance(sp.get("nodes"), list):
                    curve_nodes_max = max(curve_nodes_max, len(sp["nodes"]))
    if curve_nodes_max <= 4:
        return TestOutcome("FAIL", f"convert-to-curves produced trivial/AABB outline (max_nodes={curve_nodes_max})")
    return TestOutcome("PASS", f"convert-to-curves converted text into real Bézier glyph paths (max_nodes={curve_nodes_max})")


def _test_w2_bspline_symbol_perspective(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W2-BSPLINE-PERSP")
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    # Render BSpline in isolation (remove pre-existing objects 3, 4, 5, 7 so silent no-op cannot pass)
    bspline_outline = doc["objects"]["3"]["outline"]
    doc["objects"] = {
        "8": {
            "id": 8,
            "name": "BSpline Curve",
            "payload": {
                "BSpline": {
                    "control_points": [
                        {"x": 30.0, "y": 140.0},
                        {"x": 60.0, "y": 110.0},
                        {"x": 100.0, "y": 115.0},
                        {"x": 125.0, "y": 145.0},
                        {"x": 80.0, "y": 165.0},
                    ],
                    "degree": 3,
                    "knots": [0.0, 0.0, 0.0, 0.0, 0.5, 1.0, 1.0, 1.0, 1.0],
                    "closed": True,
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
            "fill": {"Solid": {"Cmyk": {"c": 0.0, "m": 0.5, "y": 1.0, "k": 0.0, "a": 1.0}}},
            "outline": bspline_outline,
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
    }
    doc["pages"][0]["layers"][0]["objects"] = [8]
    doc["pages"][0]["layers"][1]["objects"] = []
    doc["next_id"] = 9
    bs_in = work / "bspline.coree"
    _write_coree_from_doc(bs_in, "BSpline Isolated Test", doc)

    bs_png = work / "bspline.png"
    r_ren = run_cmd([str(ctx.cli_bin), "render", str(bs_in), str(bs_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ren.returncode != 0 or not bs_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: coree-cli render rejected BSpline fixture: {r_ren.stderr.strip()}")
    bs_bytes = bs_png.read_bytes()
    try:
        bs_info = parse_png_info(bs_bytes)
        bs_pw, bs_ph, bs_rgba = _decode_png_rgba_bytes_stdlib(bs_bytes)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-02: BSpline rendered PNG malformed: {exc}")
    if bs_info["non_white_pixels"] < 50 or bs_info["luma_stddev"] < 1.0:
        return TestOutcome("FAIL", f"W2 ADD-02: Isolated BSpline rendered blank: {bs_info}")
    # Control-point AABB is [30, 125] x [110, 165]; all 4 inset AABB corners lie >15.6 mm outside the
    # convex hull [(30,140),(60,110),(100,115),(125,145),(80,165)] (top-left (32,112) is 18.385 mm outside),
    # so a genuine BSpline cannot paint them while an AABB-box stub would.
    bs_pw_mm = float(doc["pages"][0]["width_mm"])
    bs_ph_mm = float(doc["pages"][0]["height_mm"])
    for ext_x_mm, ext_y_mm in ((32.0, 112.0), (123.0, 112.0), (32.0, 163.0), (123.0, 163.0)):
        ext_px = max(0, min(bs_pw - 1, int(round(ext_x_mm * bs_pw / bs_pw_mm))))
        ext_py = max(0, min(bs_ph - 1, int(round(ext_y_mm * bs_ph / bs_ph_mm))))
        ext_off = (ext_py * bs_pw + ext_px) * 4
        er, eg, eb = bs_rgba[ext_off], bs_rgba[ext_off + 1], bs_rgba[ext_off + 2]
        if not (er > 245 and eg > 245 and eb > 245):
            return TestOutcome(
                "FAIL",
                f"W2 ADD-02: BSpline exterior AABB corner ({ext_x_mm}, {ext_y_mm}) mm is non-white {(er, eg, eb)} (AABB-box stub?)",
            )

    doc_only_code = _read_rust_stripped(ctx, "crates/coree-core/src/document.rs")
    cmd_only_code = _read_rust_stripped(ctx, "crates/coree-core/src/command.rs")
    doc_code = doc_only_code + "\n" + cmd_only_code
    for sym in ("FitContents", "CenterContents", "StretchToFill"):
        if not re.search(rf"\b{sym}\b", doc_code):
            return TestOutcome("FAIL", f"W2 ADD-03: PowerClip mode {sym} missing")
    math_code = _read_rust_stripped(ctx, "crates/coree-core/src/math.rs")
    if not re.search(r"\b(?:homography|Perspective3x3)\b", math_code, re.I):
        return TestOutcome("FAIL", "W2 ADD-04: 3x3 perspective homography missing in coree-core/src/math.rs")

    # Isolated runtime CLI pixel-diff checks for SymbolInstance, Effect::Perspective, ArtisticMedia, and live Clone
    _, base_tpl = inspect_coree_zip(_f(ctx, "fixture.coree"))
    base_tpl["pages"][0]["width_mm"] = 120.0
    base_tpl["pages"][0]["height_mm"] = 120.0
    base_tpl["pages"][0]["bleed_mm"] = 0.0
    base_tpl["pages"][0]["layers"][0]["objects"] = []
    base_tpl["pages"][0]["layers"][1]["objects"] = []
    base_tpl["objects"] = {}
    base_tpl["symbols"] = {}
    base_outline = {**bspline_outline, "width_mm": 0.0, "is_hairline": False}

    def _px_at_mm(png_bytes: bytes, x_mm: float, y_mm: float, pw_mm: float = 120.0, ph_mm: float = 120.0) -> tuple[int, int, int, int]:
        pw, ph, buf = _decode_png_rgba_bytes_stdlib(png_bytes)
        px = max(0, min(pw - 1, int(round(x_mm * pw / pw_mm))))
        py = max(0, min(ph - 1, int(round(y_mm * ph / ph_mm))))
        off = (py * pw + px) * 4
        return buf[off], buf[off + 1], buf[off + 2], buf[off + 3]

    # Empty page control
    empty_in = work / "empty_ctrl.coree"
    empty_png = work / "empty_ctrl.png"
    _write_coree_from_doc(empty_in, "Empty Control", base_tpl)
    re_empty = run_cmd([str(ctx.cli_bin), "render", str(empty_in), str(empty_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if re_empty.returncode != 0 or not empty_png.is_file():
        return TestOutcome("FAIL", f"W2: empty page control render failed: {re_empty.stderr}")

    # 1. Runtime SymbolInstance check: renders SymbolDef geometry at instance transform
    sym_uuid = "11111111-1111-4111-8111-111111111111"
    sym_doc = json.loads(json.dumps(base_tpl))
    sym_doc["symbols"] = {
        sym_uuid: {
            "id": sym_uuid,
            "name": "BlueSym",
            "objects": [{
                "id": 99,
                "name": "SymRect",
                "payload": {"Shape": {"Rectangle": {"width": 30.0, "height": 30.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
                "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
                "fill": {"Solid": {"Rgba": {"r": 0.0, "g": 0.2, "b": 1.0, "a": 1.0}}},
                "outline": base_outline,
                "lens": None,
                "effects": [],
                "blend_mode": "Normal",
                "opacity": 1.0,
                "visible": True,
                "locked": False,
                "printable": True,
                "style_id": None,
                "unknown_chunks": [],
            }],
        }
    }
    sym_doc["objects"] = {
        "8": {
            "id": 8,
            "name": "SymInst",
            "payload": {"SymbolInstance": sym_uuid},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 45.0, 45.0]},
            "fill": "None",
            "outline": base_outline,
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
    }
    sym_doc["pages"][0]["layers"][0]["objects"] = [8]
    sym_doc["next_id"] = 9
    sym_in = work / "symbol_inst.coree"
    sym_png = work / "symbol_inst.png"
    _write_coree_from_doc(sym_in, "SymbolInstance Test", sym_doc)
    r_sym = run_cmd([str(ctx.cli_bin), "render", str(sym_in), str(sym_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_sym.returncode != 0 or not sym_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-03: SymbolInstance render failed: {r_sym.stderr}")
    sym_bytes = sym_png.read_bytes()
    try:
        if parse_png_info(sym_bytes)["non_white_pixels"] < 50 or _compare_same_page_pngs(empty_png, sym_png, "W2 ADD-03 SymbolInstance", min_non_white_a=0, min_non_white_b=50)["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", "W2 ADD-03: SymbolInstance rendered blank vs empty page")
        sym_center_px = _px_at_mm(sym_bytes, 60.0, 60.0)
        sym_out_px = _px_at_mm(sym_bytes, 10.0, 10.0)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-03: SymbolInstance PNG malformed: {exc}")
    if sym_center_px[2] < 150 or sym_center_px[0] > 100 or sym_out_px[0] < 248:
        return TestOutcome("FAIL", f"W2 ADD-03: SymbolInstance geometry not positioned at instance transform (center={sym_center_px}, corner={sym_out_px})")

    # 2. Runtime Effect::Perspective check: trapezoidal warp differs from unwarped control
    persp_doc = json.loads(json.dumps(base_tpl))
    persp_doc["objects"] = {
        "8": {
            "id": 8,
            "name": "PerspRect",
            "payload": {"Shape": {"Rectangle": {"width": 40.0, "height": 40.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 30.0]},
            "fill": {"Solid": {"Rgba": {"r": 0.1, "g": 0.7, "b": 0.2, "a": 1.0}}},
            "outline": base_outline,
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
    }
    persp_doc["pages"][0]["layers"][0]["objects"] = [8]
    persp_doc["next_id"] = 9
    persp_ctrl_in = work / "persp_ctrl.coree"
    persp_ctrl_png = work / "persp_ctrl.png"
    _write_coree_from_doc(persp_ctrl_in, "Perspective Control", persp_doc)
    r_pc = run_cmd([str(ctx.cli_bin), "render", str(persp_ctrl_in), str(persp_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_pc.returncode != 0 or not persp_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-04: Perspective control render failed: {r_pc.stderr}")
    persp_doc["objects"]["8"]["effects"] = [{
        "Perspective": {
            "quad": [
                {"x": 14.0, "y": 0.0},
                {"x": 26.0, "y": 0.0},
                {"x": 40.0, "y": 40.0},
                {"x": 0.0, "y": 40.0},
            ]
        }
    }]
    persp_in = work / "persp.coree"
    persp_png = work / "persp.png"
    _write_coree_from_doc(persp_in, "Perspective Warp", persp_doc)
    r_pw = run_cmd([str(ctx.cli_bin), "render", str(persp_in), str(persp_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if r_pw.returncode != 0 or not persp_png.is_file() or _compare_same_page_pngs(persp_ctrl_png, persp_png, "W2 ADD-04 Perspective")["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", "W2 ADD-04: Effect::Perspective produced no visible projective warp")
        p_bytes = persp_png.read_bytes()
        top_corner_px = _px_at_mm(p_bytes, 34.0, 34.0)
        bot_corner_px = _px_at_mm(p_bytes, 34.0, 66.0)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-04: Perspective PNG malformed: {exc}")
    if top_corner_px[0] < 245 or bot_corner_px[1] < 100 or bot_corner_px[0] > 120:
        return TestOutcome("FAIL", f"W2 ADD-04: Perspective trapezoidal taper invalid (top={top_corner_px}, bot={bot_corner_px})")

    # 3. Runtime NodePayload::ArtisticMedia check (must differ from bare spine curve and render non-white > 50)
    am_spine = {
        "subpaths": [{
            "nodes": [
                {"pos": {"x": 20.0, "y": 60.0}, "handle_in": None, "handle_out": {"x": 45.0, "y": 25.0}, "kind": "Smooth"},
                {"pos": {"x": 100.0, "y": 60.0}, "handle_in": {"x": 75.0, "y": 95.0}, "handle_out": None, "kind": "Smooth"},
            ],
            "closed": False,
        }]
    }
    am_ctrl_doc = json.loads(json.dumps(base_tpl))
    am_ctrl_doc["objects"] = {
        "8": {
            "id": 8,
            "name": "Spine Control",
            "payload": {"Path": am_spine},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
            "fill": "None",
            "outline": {**bspline_outline, "width_mm": 0.3, "is_hairline": False},
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
    }
    am_ctrl_doc["pages"][0]["layers"][0]["objects"] = [8]
    am_ctrl_doc["next_id"] = 9
    am_ctrl_in = work / "am_ctrl.coree"
    am_ctrl_png = work / "am_ctrl.png"
    _write_coree_from_doc(am_ctrl_in, "ArtisticMedia Spine Control", am_ctrl_doc)
    r_amc = run_cmd([str(ctx.cli_bin), "render", str(am_ctrl_in), str(am_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_amc.returncode != 0 or not am_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: ArtisticMedia spine control render failed: {r_amc.stderr}")

    am_doc = json.loads(json.dumps(am_ctrl_doc))
    am_doc["objects"]["8"]["name"] = "ArtisticMedia Live"
    am_doc["objects"]["8"]["payload"] = {
        "ArtisticMedia": {
            "spine": am_spine,
            "style": {"Calligraphic": {"width_mm": 8.0, "angle_deg": 45.0}},
        }
    }
    am_doc["objects"]["8"]["fill"] = {"Solid": {"Rgba": {"r": 0.1, "g": 0.2, "b": 0.85, "a": 1.0}}}
    am_doc["objects"]["8"]["outline"] = base_outline
    am_in = work / "artistic_media.coree"
    am_png = work / "artistic_media.png"
    _write_coree_from_doc(am_in, "ArtisticMedia Isolated Test", am_doc)
    r_am = run_cmd([str(ctx.cli_bin), "render", str(am_in), str(am_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_am.returncode != 0 or not am_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: coree-cli render rejected NodePayload::ArtisticMedia: {r_am.stderr.strip()}")
    try:
        am_info = parse_png_info(am_png.read_bytes())
        if am_info["non_white_pixels"] <= 50 or _compare_same_page_pngs(am_ctrl_png, am_png, "W2 ADD-02 ArtisticMedia", min_non_white_b=50)["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", f"W2 ADD-02: ArtisticMedia failed to expand variable-width stroke vs bare spine: {am_info}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-02: ArtisticMedia PNG malformed: {exc}")

    # 4. Runtime live Clone check (renders master geometry at clone transform + preserves Clone across convert round-trip)
    cl_doc = json.loads(json.dumps(base_tpl))
    cl_doc["objects"] = {
        "3": {
            "id": 3,
            "name": "Clone Master",
            "payload": {"Shape": {"Rectangle": {"width": 30.0, "height": 30.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 15.0, 40.0]},
            "fill": {"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}},
            "outline": base_outline,
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
    }
    cl_doc["pages"][0]["layers"][0]["objects"] = [3]
    cl_doc["next_id"] = 4
    cl_ctrl_in = work / "clone_ctrl.coree"
    cl_ctrl_png = work / "clone_ctrl.png"
    _write_coree_from_doc(cl_ctrl_in, "Clone Master Only", cl_doc)
    r_clc = run_cmd([str(ctx.cli_bin), "render", str(cl_ctrl_in), str(cl_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_clc.returncode != 0 or not cl_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: Clone master control render failed: {r_clc.stderr}")

    cl_doc["objects"]["4"] = {
        "id": 4,
        "name": "Live Clone",
        "payload": {"Clone": {"master_id": 3, "link_fill": True, "link_outline": True, "link_shape": True}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 75.0, 40.0]},
        "fill": "None",
        "outline": base_outline,
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
    cl_doc["pages"][0]["layers"][0]["objects"] = [3, 4]
    cl_doc["next_id"] = 5
    cl_in = work / "clone_live.coree"
    cl_png = work / "clone_live.png"
    cl_rt = work / "clone_rt.coree"
    _write_coree_from_doc(cl_in, "Live Clone Test", cl_doc)
    r_cl = run_cmd([str(ctx.cli_bin), "render", str(cl_in), str(cl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cl.returncode != 0 or not cl_png.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: coree-cli render rejected live Clone object: {r_cl.stderr.strip()}")
    try:
        if _compare_same_page_pngs(cl_ctrl_png, cl_png, "W2 ADD-02 Live Clone")["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", "W2 ADD-02: live Clone produced no pixel diff vs master-only control")
        cl_px = _px_at_mm(cl_png.read_bytes(), 90.0, 55.0)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-02: live Clone PNG malformed: {exc}")
    if cl_px[0] < 150 or cl_px[2] > 100:
        return TestOutcome("FAIL", f"W2 ADD-02: live Clone did not render master red fill at clone offset (px={cl_px})")
    r_cl_rt = run_cmd([str(ctx.cli_bin), "convert", str(cl_in), str(cl_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cl_rt.returncode != 0 or not cl_rt.is_file():
        return TestOutcome("FAIL", f"W2 ADD-02: convert round-trip failed on live Clone: {r_cl_rt.stderr}")
    try:
        _, cl_rt_doc = inspect_coree_zip(cl_rt)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-02: live Clone round-trip .coree malformed: {exc}")
    cl_objs = cl_rt_doc.get("objects")
    cl_obj4 = cl_objs.get("4") if isinstance(cl_objs, dict) else None
    cl_payload = cl_obj4.get("payload") if isinstance(cl_obj4, dict) else None
    if not isinstance(cl_payload, dict) or "Clone" not in cl_payload:
        return TestOutcome("FAIL", f"W2 ADD-02: convert flattened/dropped live Clone payload: {cl_obj4}")

    # ADD-02: Spine-linked live ArtisticMedia & live Clone propagation (not static baked geometry)
    node_payload_block = _extract_brace_block(doc_only_code, r"\bpub\s+enum\s+NodePayload\s*\{") or ""
    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
    if not re.search(r"\bArtisticMedia\b", node_payload_block):
        return TestOutcome("FAIL", "W2 ADD-02: NodePayload::ArtisticMedia spine-linked live variant missing in document.rs")
    if not re.search(r"\bArtisticMedia\b", ren_code) or _has_empty_match_arm(ren_code, r"(?:NodePayload::)?ArtisticMedia"):
        return TestOutcome("FAIL", "W2 ADD-02: renderer.rs does not genuinely render live NodePayload::ArtisticMedia")
    if not re.search(r"\bClone\b", node_payload_block):
        return TestOutcome("FAIL", "W2 ADD-02: NodePayload::Clone variant missing in document.rs")
    if not re.search(r"\bClone\b", ren_code) or _has_empty_match_arm(ren_code, r"(?:NodePayload::)?Clone"):
        return TestOutcome("FAIL", "W2 ADD-02: renderer.rs does not genuinely render live NodePayload::Clone objects")

    # ADD-03: Isolated Symbol studio editing (edit_symbol / UnlinkSymbolInstance / UpsertSymbol)
    for sym_op in ("UpsertSymbol", "DeleteSymbol", "InstantiateSymbol", "UnlinkSymbolInstance"):
        if not re.search(rf"\b{sym_op}\b", cmd_only_code):
            return TestOutcome("FAIL", f"W2 ADD-03: CommandOp::{sym_op} missing in command.rs")
    app_sym_code = (
        _read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-app/src/state/mod.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-app/src/state/editing.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs")
    )
    if not re.search(r"\b(?:edit_symbol|EditSymbol|BeginEditSymbol|FinishEditSymbol|editing_symbol|symbol_isolation)\b", app_sym_code):
        return TestOutcome("FAIL", "W2 ADD-03: Isolated Symbol studio editing (edit_symbol) missing in coree-app")

    for pkg, filt, label, min_p in (
        ("coree-core", "artistic_media", "W2 ADD-02 ArtisticMedia", 3),
        ("coree-core", "live_clone", "W2 ADD-02 live Clone", 1),
        ("coree-core", "fit_powerclip", "W2 ADD-03 FitPowerClip", 1),
        ("coree-core", "homography", "W2 ADD-04 Perspective homography", 1),
        ("coree-core", "symbol", "W2 ADD-03 Symbol", 4),
        ("coree-app", "edit_symbol", "W2 ADD-03 Symbol Studio isolation", 1),
    ):
        fail_c = _run_cargo_tests(ctx, ["-p", pkg, "--", filt], label, min_passed=min_p)
        if fail_c is not None:
            return fail_c

    return TestOutcome(
        "PASS",
        f"Isolated BSpline/ArtisticMedia/Clone/SymbolInstance/Perspective CLI render + cargo tests (including edit_symbol) verified",
    )


def _test_w2_smartfill_dcel_and_boolean_meta(ctx: RunContext) -> TestOutcome:
    sf_code = _read_rust_stripped(ctx, "crates/coree-render/src/smart_fill.rs")
    if not re.search(r"\b(?:Dcel|HalfEdge|dcel_smart_fill)\b", sf_code, re.I):
        return TestOutcome("FAIL", "W2 IMP-05: smart_fill.rs lacks planar DCEL structs/functions")
    fail_sf = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "dcel"], "W2 IMP-05 DCEL SmartFill")
    if fail_sf is not None:
        return fail_sf
    fail_bool = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "boolean"], "W2 REM-03")
    if fail_bool is not None:
        return fail_bool
    return TestOutcome("PASS", "Planar DCEL SmartFill & BooleanOp cargo tests verified")


def _test_w3_icc_color_pipeline(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W3-ICC")
    color_code = _read_rust_stripped(ctx, "crates/coree-core/src/color.rs")
    icc_code = _read_rust_stripped(ctx, "crates/coree-core/src/icc.rs")
    if not re.search(r"\bchad\b", icc_code, re.I) or not re.search(r"\b(?:black_point|bpc)\b", icc_code, re.I):
        return TestOutcome("FAIL", "W3 REQ-02: icc.rs missing BPC / chromatic adaptation (chad)")
    if not re.search(r"\bcmyk_to_rgb_icc\b", color_code) or not re.search(r"\bRenderingIntent\b", color_code + "\n" + icc_code):
        return TestOutcome("FAIL", "W3 REQ-02: Color::cmyk_to_rgb / lerp not yet routed through ICC + RenderingIntent")

    # Exercise CLI render with an actual local CMYK /usr/share/color/icc/ profile embedded
    if ctx.icc_dir is None:
        return TestOutcome("FAIL", "W3 REQ-02: system ICC directory /usr/share/color/icc/ missing")
    all_icc = sorted(ctx.icc_dir.rglob("*.icc"))
    if not all_icc:
        return TestOutcome("FAIL", "W3 REQ-02: no .icc profiles found under /usr/share/color/icc/")
    preferred_cmyk = [
        p for p in all_icc
        if p.name in ("IFRA26S_2004_newsprint.icc", "SWOP_TR003_coated_3.icc", "FOGRA39L_coated.icc", "default_cmyk.icc")
    ]
    icc_file = preferred_cmyk[0] if preferred_cmyk else all_icc[0]

    src = _f(ctx, "fixture.coree")
    baseline_png = work / "baseline_render.png"
    rb = run_cmd([str(ctx.cli_bin), "render", str(src), str(baseline_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rb.returncode != 0 or not baseline_png.is_file():
        return TestOutcome("FAIL", f"W3 REQ-02: baseline render failed: {rb.stderr}")

    _, doc = inspect_coree_zip(src)
    icc_b64 = base64.b64encode(icc_file.read_bytes()).decode("ascii")
    doc["custom_icc_profile"] = icc_b64
    doc["custom_icc_name"] = icc_file.name
    in_icc = work / "with_icc.coree"
    _write_coree_from_doc(in_icc, "ICC Embedded", doc)
    out_png = work / "icc_render.png"
    r = run_cmd([str(ctx.cli_bin), "render", str(in_icc), str(out_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if r.returncode != 0 or not out_png.is_file() or parse_png_info(out_png.read_bytes())["non_white_pixels"] < 50:
            return TestOutcome("FAIL", f"W3 REQ-02: CLI render with embedded ICC failed: {r.stderr}")

        icc_diff = _compare_same_page_pngs(baseline_png, out_png, "W3 REQ-02 ICC", min_non_white_b=50)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REQ-02: ICC render PNG malformed: {exc}")
    if icc_diff["mean_abs_diff"] <= 0.1:
        return TestOutcome(
            "FAIL",
            f"W3 REQ-02: embedded CMYK ICC profile ({icc_file.name}) produced no colorimetric pixel change vs default render (mean_abs_diff={icc_diff['mean_abs_diff']:.4f})",
        )

    # FEAT-31: Multi-stop Fountain Fill (LinearGradient) render + ICC interpolation check
    _, grad_doc = inspect_coree_zip(src)
    grad_obj = dict(grad_doc["objects"]["3"])
    grad_obj["fill"] = {
        "LinearGradient": {
            "start": {"x": 0.0, "y": 0.0},
            "end": {"x": 90.0, "y": 55.0},
            "stops": [
                {"offset": 0.0, "color": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}},
                {"offset": 0.5, "color": {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}},
                {"offset": 1.0, "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 1.0, "k": 0.0, "a": 1.0}}},
            ],
        }
    }
    grad_obj["outline"] = {**grad_obj["outline"], "width_mm": 0.0, "is_hairline": False}
    grad_doc["objects"] = {"3": grad_obj}
    grad_doc["pages"][0]["layers"][0]["objects"] = [3]
    grad_doc["pages"][0]["layers"][1]["objects"] = []
    grad_base_in = work / "grad_base.coree"
    grad_base_png = work / "grad_base.png"
    _write_coree_from_doc(grad_base_in, "Fountain Fill Base", grad_doc)
    rgb_grad = run_cmd([str(ctx.cli_bin), "render", str(grad_base_in), str(grad_base_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rgb_grad.returncode != 0 or not grad_base_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-31: LinearGradient render failed: {rgb_grad.stderr}")
    try:
        grad_info = parse_png_info(grad_base_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-31: LinearGradient PNG malformed: {exc}")
    if grad_info["unique_colors_sample"] <= 8 or grad_info["luma_stddev"] <= 1.0:
        return TestOutcome("FAIL", f"W3 FEAT-31: LinearGradient rendered flat/degenerate: {grad_info}")
    grad_doc["custom_icc_profile"] = icc_b64
    grad_doc["custom_icc_name"] = icc_file.name
    grad_icc_in = work / "grad_icc.coree"
    grad_icc_png = work / "grad_icc.png"
    _write_coree_from_doc(grad_icc_in, "Fountain Fill ICC", grad_doc)
    ricc_grad = run_cmd([str(ctx.cli_bin), "render", str(grad_icc_in), str(grad_icc_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if ricc_grad.returncode != 0 or not grad_icc_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-31: LinearGradient ICC render failed: {ricc_grad.stderr}")
    try:
        grad_icc_diff = _compare_same_page_pngs(grad_base_png, grad_icc_png, "W3 FEAT-31 LinearGradient ICC")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-31: LinearGradient ICC PNG malformed: {exc}")
    if grad_icc_diff["mean_abs_diff"] <= 0.1:
        return TestOutcome("FAIL", f"W3 REQ-02/FEAT-31: ICC profile had no effect on multi-stop LinearGradient ({grad_icc_diff})")

    fail = _run_cargo_tests(ctx, ["-p", "coree-core", "--", "icc"], "W3 REQ-02")
    if fail is not None:
        return fail
    return TestOutcome("PASS", f"ICC + BPC + chad + Fountain Fill ICC render ({icc_file.name}, diff={icc_diff['mean_abs_diff']:.2f}) + cargo test verified")


def _test_w3_wgpu_and_world_tilecache(ctx: RunContext) -> TestOutcome:
    backend_code = _read_rust_stripped(ctx, "crates/coree-render/src/backend.rs")
    if not backend_code or not re.search(r"\bstruct\s+WgpuBackend\b", backend_code):
        return TestOutcome("FAIL", "W3 IMP-01: struct WgpuBackend behind CanvasBackend not yet implemented")
    if not re.search(r"impl\s+CanvasBackend\s+for\s+WgpuBackend\b", backend_code):
        return TestOutcome("FAIL", "W3 IMP-01: impl CanvasBackend for WgpuBackend missing")
    tc_code = _read_rust_stripped(ctx, "crates/coree-render/src/tile_cache.rs")
    if not re.search(r"\bstruct\s+WorldTileKey\b", tc_code):
        return TestOutcome("FAIL", "W3 IMP-02: DirtyTileCache lacks continuous struct WorldTileKey (lod, tile_x, tile_y)")
    fail_wg = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "wgpu"], "W3 IMP-01 WgpuBackend")
    if fail_wg is not None:
        return fail_wg
    fail_wt = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "world_tile"], "W3 IMP-02 WorldTileKey")
    if fail_wt is not None:
        return fail_wt
    return TestOutcome("PASS", "WgpuBackend + WorldTileKey DirtyTileCache cargo tests verified")


def _test_w3_effects_coons_and_frozen_lens(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W3-EFFECTS")
    eff_code = _read_rust_stripped(ctx, "crates/coree-core/src/effects.rs")
    if not re.search(r"\bInnerShadow\b", eff_code):
        return TestOutcome("FAIL", "W3 ADD-01: Effect::InnerShadow not yet implemented in effects.rs")

    style_code = _read_rust_stripped(ctx, "crates/coree-core/src/style.rs")
    mesh_node_block = _extract_brace_block(style_code, r"\bpub\s+struct\s+MeshNode\s*\{")
    if not mesh_node_block:
        return TestOutcome("FAIL", "W3 IMP-04: pub struct MeshNode missing in crates/coree-core/src/style.rs")
    has_tangents = (
        all(re.search(rf"\b{h}\b", mesh_node_block) for h in ("handle_n", "handle_s", "handle_e", "handle_w"))
        or bool(re.search(r"\btangents\b|\bhandles\b", mesh_node_block))
    )
    has_node_alpha = bool(re.search(r"\b(?:alpha|opacity)\b", mesh_node_block))
    if not has_tangents or not has_node_alpha:
        return TestOutcome(
            "FAIL",
            f"W3 IMP-04: MeshNode in style.rs lacks Coons tangent handles (N/S/E/W or tangents) or node alpha/opacity (has_tangents={has_tangents}, has_node_alpha={has_node_alpha})",
        )

    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
    if not re.search(r"\bcoons\b", ren_code, re.I) or not re.search(r"\b(?:handle_n|handle_e|tangents)\b", ren_code):
        return TestOutcome("FAIL", "W3 IMP-04: Bicubic Coons MeshFill handle interpolation not yet implemented in renderer.rs")

    lens_code = _read_rust_stripped(ctx, "crates/coree-core/src/lens.rs")
    lens_cfg_block = _extract_brace_block(lens_code, r"\bpub\s+struct\s+LensConfig\s*\{")
    if not lens_cfg_block or not re.search(r"\bfrozen_snapshot\b", lens_cfg_block):
        return TestOutcome("FAIL", "W3 REM-04: LensConfig::frozen_snapshot not yet persisted in Document/lens.rs")

    # 1. Runtime Fill::MeshFill CLI render check (non-flat interior colors + node alpha transparency diff)
    src = _f(ctx, "fixture.coree")
    _, mesh_doc = inspect_coree_zip(src)
    mesh_doc["pages"][0]["width_mm"] = 100.0
    mesh_doc["pages"][0]["height_mm"] = 100.0
    mesh_doc["pages"][0]["bleed_mm"] = 0.0
    mesh_obj = dict(mesh_doc["objects"]["3"])
    mesh_obj["payload"] = {
        "Shape": {"Rectangle": {"width": 60.0, "height": 60.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}
    }
    mesh_obj["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]}
    mesh_obj["fill"] = {
        "MeshFill": {
            "rows": 2,
            "cols": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Rgba": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}},
                {"row": 0, "col": 1, "pos": {"x": 60.0, "y": 0.0}, "color": {"Rgba": {"r": 0.0, "g": 1.0, "b": 0.0, "a": 1.0}}},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 60.0}, "color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0}}},
                {"row": 1, "col": 1, "pos": {"x": 60.0, "y": 60.0}, "color": {"Rgba": {"r": 1.0, "g": 1.0, "b": 0.0, "a": 1.0}}},
            ],
        }
    }
    mesh_obj["outline"] = {**mesh_obj["outline"], "width_mm": 0.0, "is_hairline": False}
    mesh_doc["objects"] = {"3": mesh_obj}
    mesh_doc["pages"][0]["layers"][0]["objects"] = [3]
    mesh_doc["pages"][0]["layers"][1]["objects"] = []
    mesh_in = work / "mesh_in.coree"
    mesh_png = work / "mesh_out.png"
    _write_coree_from_doc(mesh_in, "MeshFill W3", mesh_doc)
    rm = run_cmd([str(ctx.cli_bin), "render", str(mesh_in), str(mesh_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rm.returncode != 0 or not mesh_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill CLI render failed: {rm.stderr}")
    try:
        mesh_info = parse_png_info(mesh_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill PNG malformed: {exc}")
    if mesh_info["non_white_pixels"] < 100 or mesh_info["unique_colors_sample"] <= 8 or mesh_info["luma_stddev"] <= 1.0:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill rendered flat/degenerate interior: {mesh_info}")

    # Verify node alpha transparency produces measurable visual change vs opaque mesh
    mesh_alpha_doc = json.loads(json.dumps(mesh_doc))
    mesh_alpha_doc["objects"]["3"]["fill"]["MeshFill"]["nodes"][0]["alpha"] = 0.0
    mesh_alpha_in = work / "mesh_alpha_in.coree"
    mesh_alpha_png = work / "mesh_alpha_out.png"
    _write_coree_from_doc(mesh_alpha_in, "MeshFill Alpha W3", mesh_alpha_doc)
    rma = run_cmd([str(ctx.cli_bin), "render", str(mesh_alpha_in), str(mesh_alpha_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rma.returncode != 0 or not mesh_alpha_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill alpha render failed: {rma.stderr}")
    try:
        mesh_alpha_diff = _compare_same_page_pngs(mesh_png, mesh_alpha_png, "W3 IMP-04 MeshFill alpha", min_non_white_a=100)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill alpha PNG malformed: {exc}")
    if mesh_alpha_diff["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill node alpha=0.0 produced no visual change vs opaque control: {mesh_alpha_diff}")

    # Verify Coons tangent handles produce measurable visual change vs straight-handle control
    mesh_curved_doc = json.loads(json.dumps(mesh_doc))
    curved_nodes = mesh_curved_doc["objects"]["3"]["fill"]["MeshFill"]["nodes"]
    curved_nodes[0]["handle_e"] = {"x": 20.0, "y": 25.0}
    curved_nodes[0]["handle_s"] = {"x": 25.0, "y": 20.0}
    curved_nodes[1]["handle_w"] = {"x": -20.0, "y": 25.0}
    curved_nodes[1]["handle_s"] = {"x": -25.0, "y": 20.0}
    curved_nodes[2]["handle_n"] = {"x": 25.0, "y": -20.0}
    curved_nodes[2]["handle_e"] = {"x": 20.0, "y": -25.0}
    curved_nodes[3]["handle_n"] = {"x": -25.0, "y": -20.0}
    curved_nodes[3]["handle_w"] = {"x": -20.0, "y": -25.0}
    mesh_curved_in = work / "mesh_curved_in.coree"
    mesh_curved_png = work / "mesh_curved_out.png"
    _write_coree_from_doc(mesh_curved_in, "MeshFill Curved Coons W3", mesh_curved_doc)
    rmc = run_cmd([str(ctx.cli_bin), "render", str(mesh_curved_in), str(mesh_curved_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rmc.returncode != 0 or not mesh_curved_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill Coons curved-handle render failed: {rmc.stderr}")
    try:
        mesh_coons_diff = _compare_same_page_pngs(mesh_png, mesh_curved_png, "W3 IMP-04 MeshFill Coons", min_non_white_a=100)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill Coons PNG malformed: {exc}")
    if mesh_coons_diff["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 IMP-04: MeshFill Coons tangent handles produced no visual change vs straight handles: {mesh_coons_diff}")

    fail_mesh = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "mesh"], "W3 IMP-04 Coons mesh")
    if fail_mesh is not None:
        return fail_mesh

    # 2. Runtime Frozen Lens (REM-04): cargo test freeze capture + populated frozen_snapshot convert/render persistence
    fail_frozen = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "frozen"], "W3 REM-04 frozen lens")
    if fail_frozen is not None:
        return fail_frozen

    _, lens_doc = inspect_coree_zip(src)
    lens_doc["pages"][0]["width_mm"] = 100.0
    lens_doc["pages"][0]["height_mm"] = 100.0
    lens_doc["pages"][0]["bleed_mm"] = 0.0
    lens_doc["objects"] = {"3": dict(lens_doc["objects"]["3"])}
    lens_doc["objects"]["3"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 20.0]}
    lens_doc["pages"][0]["layers"][0]["objects"] = [3]
    lens_doc["pages"][0]["layers"][1]["objects"] = []
    lens_ctrl_in = work / "lens_ctrl.coree"
    lens_ctrl_png = work / "lens_ctrl.png"
    _write_coree_from_doc(lens_ctrl_in, "Lens Control", lens_doc)
    rlc = run_cmd([str(ctx.cli_bin), "render", str(lens_ctrl_in), str(lens_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rlc.returncode != 0 or not lens_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Lens control render failed: {rlc.stderr}")

    frozen_snap_b64 = base64.b64encode(
        _encode_png_rgba_stdlib(60, 45, bytes([0, 180, 255, 255] * (60 * 45)))
    ).decode("ascii")
    lens_doc["objects"]["8"] = {
        "id": 8,
        "name": "Frozen Invert Lens",
        "payload": {
            "Shape": {"Rectangle": {"width": 60.0, "height": 45.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 25.0, 25.0]},
        "fill": "None",
        "outline": {**lens_doc["objects"]["3"]["outline"], "width_mm": 0.2},
        "lens": {
            "kind": "Invert",
            "frozen": True,
            "viewpoint": None,
            "remove_face": False,
            "frozen_snapshot": frozen_snap_b64,
        },
        "effects": [],
        "blend_mode": "Normal",
        "opacity": 1.0,
        "visible": True,
        "locked": False,
        "printable": True,
        "style_id": None,
        "unknown_chunks": [],
    }
    lens_doc["pages"][0]["layers"][0]["objects"] = [3, 8]
    lens_doc["next_id"] = 9
    lens_in = work / "lens_frozen_in.coree"
    lens_rt = work / "lens_frozen_rt.coree"
    lens_png = work / "lens_frozen_out.png"
    _write_coree_from_doc(lens_in, "Frozen Lens W3", lens_doc)
    r_lrt = run_cmd([str(ctx.cli_bin), "convert", str(lens_in), str(lens_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_lrt.returncode != 0 or not lens_rt.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens .coree -> .coree convert failed: {r_lrt.stderr}")
    try:
        _, lens_rt_doc = inspect_coree_zip(lens_rt)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens converted .coree malformed: {exc}")
    lens_objs = lens_rt_doc.get("objects")
    lens_obj8 = lens_objs.get("8") if isinstance(lens_objs, dict) else None
    rt_lens_cfg = lens_obj8.get("lens") if isinstance(lens_obj8, dict) else None
    if not isinstance(rt_lens_cfg, dict) or not rt_lens_cfg.get("frozen") or rt_lens_cfg.get("frozen_snapshot") != frozen_snap_b64:
        return TestOutcome("FAIL", f"W3 REM-04: Lens frozen_snapshot did not persist across convert: {rt_lens_cfg}")
    r_lren = run_cmd([str(ctx.cli_bin), "render", str(lens_rt), str(lens_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_lren.returncode != 0 or not lens_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens render failed: {r_lren.stderr}")
    try:
        lens_diff = _compare_same_page_pngs(lens_ctrl_png, lens_png, "W3 REM-04 Frozen Lens")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens PNG malformed: {exc}")
    if lens_diff["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Invert lens produced no pixel difference vs control: {lens_diff}")

    # Delete underlying object 3 and verify frozen_snapshot STILL renders inverted artwork (vs unfrozen empty control)
    rt_pages = lens_rt_doc.get("pages")
    if not isinstance(rt_pages, list) or not rt_pages or not isinstance(rt_pages[0], dict):
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens converted .coree missing valid pages[0]: {rt_pages!r}")
    rt_layers = rt_pages[0].get("layers")
    if not isinstance(rt_layers, list) or not rt_layers or not isinstance(rt_layers[0], dict):
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens converted .coree missing valid pages[0].layers[0]: {rt_layers!r}")
    deleted_doc = json.loads(json.dumps(lens_rt_doc))
    deleted_doc["objects"].pop("3", None)
    deleted_doc["pages"][0]["layers"][0]["objects"] = [8]
    lens_del_in = work / "lens_frozen_deleted.coree"
    lens_del_png = work / "lens_frozen_deleted.png"
    _write_coree_from_doc(lens_del_in, "Frozen Lens Deleted Underlying", deleted_doc)
    r_ldel = run_cmd([str(ctx.cli_bin), "render", str(lens_del_in), str(lens_del_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ldel.returncode != 0 or not lens_del_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Frozen Lens render after deleting underlying object failed: {r_ldel.stderr}")

    unfrozen_doc = json.loads(json.dumps(deleted_doc))
    unfrozen_doc["objects"]["8"]["lens"] = {"kind": "Invert", "frozen": False, "viewpoint": None, "remove_face": False}
    lens_unfrozen_in = work / "lens_unfrozen_deleted.coree"
    lens_unfrozen_png = work / "lens_unfrozen_deleted.png"
    _write_coree_from_doc(lens_unfrozen_in, "Unfrozen Lens Deleted Underlying", unfrozen_doc)
    r_lu = run_cmd([str(ctx.cli_bin), "render", str(lens_unfrozen_in), str(lens_unfrozen_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_lu.returncode != 0 or not lens_unfrozen_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Unfrozen control render failed: {r_lu.stderr}")
    try:
        ctrl_info = parse_png_info(lens_ctrl_png.read_bytes())
        del_info = parse_png_info(lens_del_png.read_bytes())
        unfrozen_info = parse_png_info(lens_unfrozen_png.read_bytes())
        if (
            ctrl_info["width"] < 32
            or ctrl_info["height"] < 32
            or (del_info["width"], del_info["height"]) != (ctrl_info["width"], ctrl_info["height"])
            or (unfrozen_info["width"], unfrozen_info["height"]) != (ctrl_info["width"], ctrl_info["height"])
        ):
            return TestOutcome(
                "FAIL",
                f"W3 REM-04: Frozen/unfrozen PNG dimensions {del_info['width']}x{del_info['height']} / {unfrozen_info['width']}x{unfrozen_info['height']} do not match control {ctrl_info['width']}x{ctrl_info['height']} (>= 32x32)",
            )
        frozen_persist_diff = _compare_same_page_pngs(
            lens_unfrozen_png,
            lens_del_png,
            "W3 REM-04 Frozen Lens deleted",
            min_non_white_a=0,
        )
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04: Frozen/unfrozen PNG malformed: {exc}")
    if frozen_persist_diff["mean_abs_diff"] <= 5.0:
        return TestOutcome(
            "FAIL",
            f"W3 REM-04: Frozen Lens failed to render persisted snapshot after removing underlying object 3: {frozen_persist_diff}",
        )

    # 3. Runtime remove_face (REM-04): Invert lens with remove_face=True over ArtisticText("L") must NOT invert white bg inside AABB
    _, rf_doc = inspect_coree_zip(src)
    rf_doc["pages"][0]["width_mm"] = 100.0
    rf_doc["pages"][0]["height_mm"] = 100.0
    rf_doc["pages"][0]["bleed_mm"] = 0.0
    base_style = dict(rf_doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    base_outline = dict(rf_doc["objects"]["3"]["outline"])
    rf_doc["objects"] = {
        "5": {
            "id": 5,
            "name": "L Text",
            "payload": {
                "ArtisticText": {
                    "runs": [{"text": "L", "style": {**base_style, "font_size_pt": 72.0}}],
                    "direction": "Ltr",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 25.0, 35.0]},
            "fill": "None",
            "outline": {**base_outline, "width_mm": 0.0},
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
    }
    rf_doc["pages"][0]["layers"][0]["objects"] = [5]
    rf_doc["pages"][0]["layers"][1]["objects"] = []
    rf_doc["next_id"] = 6
    rf_ctrl_in = work / "rf_ctrl.coree"
    rf_ctrl_png = work / "rf_ctrl.png"
    _write_coree_from_doc(rf_ctrl_in, "RemoveFace Text Control", rf_doc)
    r_rfc = run_cmd([str(ctx.cli_bin), "render", str(rf_ctrl_in), str(rf_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_rfc.returncode != 0 or not rf_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: remove_face text control render failed: {r_rfc.stderr}")

    rf_doc["objects"]["9"] = {
        "id": 9,
        "name": "RemoveFace Invert Lens",
        "payload": {
            "Shape": {"Rectangle": {"width": 70.0, "height": 60.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 15.0, 20.0]},
        "fill": "None",
        "outline": {**base_outline, "width_mm": 0.0},
        "lens": {"kind": "Invert", "frozen": False, "viewpoint": None, "remove_face": True},
        "effects": [],
        "blend_mode": "Normal",
        "opacity": 1.0,
        "visible": True,
        "locked": False,
        "printable": True,
        "style_id": None,
        "unknown_chunks": [],
    }
    rf_doc["pages"][0]["layers"][0]["objects"] = [5, 9]
    rf_doc["next_id"] = 10
    rf_lens_in = work / "rf_lens.coree"
    rf_lens_png = work / "rf_lens.png"
    _write_coree_from_doc(rf_lens_in, "RemoveFace Invert Over L", rf_doc)
    r_rfl = run_cmd([str(ctx.cli_bin), "render", str(rf_lens_in), str(rf_lens_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_rfl.returncode != 0 or not rf_lens_png.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: remove_face lens render failed: {r_rfl.stderr}")

    try:
        rf_diff = _compare_same_page_pngs(rf_ctrl_png, rf_lens_png, "W3 REM-04 remove_face")
        if rf_diff["mean_abs_diff"] <= 0.05:
            return TestOutcome("FAIL", f"W3 REM-04: remove_face Invert lens had no effect on ArtisticText('L'): {rf_diff}")
        rf_w_a, rf_h_a, rf_rgba_a = _decode_png_rgba_bytes_stdlib(rf_ctrl_png.read_bytes())
        rf_w_b, rf_h_b, rf_rgba_b = _decode_png_rgba_bytes_stdlib(rf_lens_png.read_bytes())
        if (rf_w_a, rf_h_a) != (rf_w_b, rf_h_b) or len(rf_rgba_a) != len(rf_rgba_b):
            return TestOutcome(
                "FAIL",
                f"W3 REM-04: remove_face PNG dimension mismatch: {(rf_w_a, rf_h_a)} vs {(rf_w_b, rf_h_b)}",
            )
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04: remove_face PNG malformed: {exc}")
    false_inverted_bg = 0
    for idx in range(0, min(len(rf_rgba_a), len(rf_rgba_b)), 4):
        if rf_rgba_a[idx] >= 250 and rf_rgba_a[idx + 1] >= 250 and rf_rgba_a[idx + 2] >= 250:
            if rf_rgba_b[idx] < 200 or rf_rgba_b[idx + 1] < 200 or rf_rgba_b[idx + 2] < 200:
                false_inverted_bg += 1
    if false_inverted_bg > 150:
        return TestOutcome(
            "FAIL",
            f"W3 REM-04: remove_face=True falsely inverted {false_inverted_bg} white background pixels inside ArtisticText AABB",
        )

    # 4. Round-trip & render a .coree exercising InnerShadow + compare pixels vs baseline
    baseline_png = work / "baseline.png"
    rb = run_cmd([str(ctx.cli_bin), "render", str(src), str(baseline_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rb.returncode != 0 or not baseline_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01: baseline render failed: {rb.stderr}")

    _, doc = inspect_coree_zip(src)
    doc["objects"]["3"]["effects"] = [
        {
            "InnerShadow": {
                "dx_mm": 1.5,
                "dy_mm": 1.5,
                "blur_mm": 2.0,
                "opacity": 0.6,
                "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}},
                "feather_direction": "Inside",
            }
        }
    ]
    in_eff = work / "effects_in.coree"
    _write_coree_from_doc(in_eff, "Effects W3", doc)
    out_png = work / "effects_out.png"
    r = run_cmd([str(ctx.cli_bin), "render", str(in_eff), str(out_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if r.returncode != 0 or not out_png.is_file() or parse_png_info(out_png.read_bytes())["non_white_pixels"] < 50:
            return TestOutcome("FAIL", f"W3 ADD-01: InnerShadow .coree render failed: {r.stderr}")
        eff_diff = _compare_same_page_pngs(baseline_png, out_png, "W3 ADD-01 InnerShadow", min_non_white_b=50)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01: InnerShadow PNG malformed: {exc}")
    if eff_diff["mean_abs_diff"] <= 0.5:
        return TestOutcome(
            "FAIL",
            f"W3 ADD-01: InnerShadow produced no visible pixel change vs baseline (mean_abs_diff={eff_diff['mean_abs_diff']:.4f})",
        )

    # 5. ADD-01 & IMP-04: Standalone Bevel, 3D multi-light/rotation Extrude, Transparency modes, 4-curve cubic Envelope, Blend tangent rotation
    eff_enum_block = _extract_brace_block(eff_code, r"\bpub\s+enum\s+Effect\s*\{") or ""
    eff_ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/effects_render.rs")
    all_ren_eff = eff_ren_code + "\n" + ren_code
    if not re.search(r"\bBevel\b", eff_enum_block) or not re.search(r"\bEffect::Bevel\b|\bBevel\b", eff_ren_code):
        return TestOutcome("FAIL", "W3 ADD-01: standalone Effect::Bevel missing in effects.rs / effects_render.rs")
    if _has_empty_match_arm(all_ren_eff, r"(?:Effect::)?Bevel"):
        return TestOutcome("FAIL", "W3 ADD-01: Effect::Bevel is an empty no-op match stub")
    if not re.search(r"\bExtrude3D\b", eff_enum_block) or not re.search(r"\brotation_3d\b", eff_enum_block) or not re.search(r"\blights\b", eff_enum_block):
        return TestOutcome("FAIL", "W3 ADD-01: Effect::Extrude3D { ..., rotation_3d, lights } missing in Effect enum (only 2.5D single-light)")
    if not re.search(r"\bExtrude3D\b", all_ren_eff) or _has_empty_match_arm(all_ren_eff, r"(?:Effect::)?Extrude3D"):
        return TestOutcome("FAIL", "W3 ADD-01: Effect::Extrude3D missing or empty no-op match stub in renderer")
    tmode_block = _extract_brace_block(eff_code, r"\bpub\s+enum\s+TransparencyMode\s*\{") or ""
    missing_tmodes = [
        tm for tm in ("Pattern", "Texture", "Conical", "Square")
        if not re.search(rf"\b{tm}\b", tmode_block) or not re.search(rf"\b{tm}\b", eff_ren_code)
    ]
    if missing_tmodes:
        return TestOutcome("FAIL", f"W3 ADD-01: TransparencyMode missing modes {missing_tmodes} in effects.rs / effects_render.rs")
    if _has_empty_match_arm(all_ren_eff, r"(?:TransparencyMode::)?(?:Pattern|Texture|Conical|Square)"):
        return TestOutcome("FAIL", "W3 ADD-01: TransparencyMode W3 arm is an empty no-op match stub")
    env_fn = _extract_brace_block(eff_ren_code, r"\bfn\s+apply_envelope_to_path\b") or ""
    if not re.search(r"\beval_cubic\b|\bcubic\b|\bBoundaryCurve", env_fn, re.I) or not re.search(r"cage\.len\(\)\s*>=\s*(?:12|16)", env_fn):
        return TestOutcome("FAIL", "W3 IMP-04: apply_envelope_to_path lacks 4-curve cubic Bézier boundary cage (>=12 pts)")
    blend_fn = _extract_brace_block(eff_ren_code, r"\bfn\s+compute_blend_steps\b") or ""
    if (
        re.search(r"\|\(\s*pt\s*,\s*_\s*\)\|", blend_fn)
        or re.search(r"\blet\s+_\s*=\s*tangent\b", blend_fn)
        or re.search(r"\(\s*pt\s*,\s*_tangent\b", blend_fn)
        or not re.search(r"\b(?:atan2|tangent)\b", blend_fn)
    ):
        return TestOutcome("FAIL", "W3 IMP-04: compute_blend_steps discards path tangent instead of rotating blend steps along path tangent")

    # Runtime CLI render diffs for the actual W3 features (Bevel, Extrude3D rotation/multi-light, Transparency modes, 4-curve cubic Envelope, Blend tangent rotation)
    _, fx_base_doc = inspect_coree_zip(src)
    fx_base_doc["pages"][0]["width_mm"] = 120.0
    fx_base_doc["pages"][0]["height_mm"] = 120.0
    fx_base_doc["pages"][0]["bleed_mm"] = 0.0
    rect_obj = dict(fx_base_doc["objects"]["3"])
    rect_obj["payload"] = {
        "Shape": {"Rectangle": {"width": 40.0, "height": 30.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}
    }
    rect_obj["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 40.0]}
    rect_obj["outline"] = {**rect_obj["outline"], "width_mm": 0.0, "is_hairline": False}
    fx_base_doc["objects"] = {"3": rect_obj}
    fx_base_doc["pages"][0]["layers"][0]["objects"] = [3]
    fx_base_doc["pages"][0]["layers"][1]["objects"] = []
    fx_ctrl_in = work / "fx_ctrl.coree"
    fx_ctrl_png = work / "fx_ctrl.png"
    _write_coree_from_doc(fx_ctrl_in, "Effects Base Control", fx_base_doc)
    rfxc = run_cmd([str(ctx.cli_bin), "render", str(fx_ctrl_in), str(fx_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rfxc.returncode != 0 or not fx_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01: effects base control render failed: {rfxc.stderr}")

    def _w3_px_at_mm(png_bytes: bytes, x_mm: float, y_mm: float) -> tuple[int, int, int, int]:
        pw, ph, buf = _decode_png_rgba_bytes_stdlib(png_bytes)
        px = max(0, min(pw - 1, int(round(x_mm * pw / 120.0))))
        py = max(0, min(ph - 1, int(round(y_mm * ph / 120.0))))
        off = (py * pw + px) * 4
        return buf[off], buf[off + 1], buf[off + 2], buf[off + 3]

    # 5a. Standalone Effect::Bevel CLI render diff
    bev_doc = json.loads(json.dumps(fx_base_doc))
    bev_doc["objects"]["3"]["effects"] = [{
        "Bevel": {
            "style": "SoftEdge",
            "distance_mm": 3.0,
            "softness_mm": 1.0,
            "light_angle_deg": 45.0,
            "altitude_deg": 35.0,
            "highlight_color": {"Rgba": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}},
            "shadow_color": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.2, "a": 1.0}},
            "intensity": 0.8,
        }
    }]
    bev_in = work / "bevel_in.coree"
    bev_png = work / "bevel_out.png"
    _write_coree_from_doc(bev_in, "Standalone Bevel Test", bev_doc)
    rbev = run_cmd([str(ctx.cli_bin), "render", str(bev_in), str(bev_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if rbev.returncode != 0 or not bev_png.is_file() or _compare_same_page_pngs(fx_ctrl_png, bev_png, "W3 ADD-01 Bevel")["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", f"W3 ADD-01: standalone Effect::Bevel CLI render failed or produced no visible diff: {rbev.stderr.strip()}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01: Bevel PNG malformed: {exc}")

    # 5b. 3D Extrude with 3D rotation & multi-light vs zero-rotation single-light control
    ext_ctrl_doc = json.loads(json.dumps(fx_base_doc))
    ext_ctrl_doc["objects"]["3"]["effects"] = [{
        "Extrude3D": {
            "vanishing_point": {"x": 95.0, "y": 15.0},
            "depth": 18.0,
            "bevel_depth": 1.5,
            "shade_color": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.2, "a": 1.0}},
            "rotation_3d": [0.0, 0.0, 0.0],
            "lights": [{"dir": [0.5, -0.5, 0.7], "intensity": 0.8}],
        }
    }]
    ext_ctrl_in = work / "ext3_ctrl.coree"
    ext_ctrl_png = work / "ext3_ctrl.png"
    _write_coree_from_doc(ext_ctrl_in, "Extrude3D Zero-Rot Control", ext_ctrl_doc)
    rextc = run_cmd([str(ctx.cli_bin), "render", str(ext_ctrl_in), str(ext_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if rextc.returncode != 0 or not ext_ctrl_png.is_file() or _compare_same_page_pngs(fx_ctrl_png, ext_ctrl_png, "W3 ADD-01 Extrude3D ctrl")["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", f"W3 ADD-01: Extrude3D control render failed or produced no diff: {rextc.stderr.strip()}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01: Extrude3D control PNG malformed: {exc}")

    ext_doc = json.loads(json.dumps(ext_ctrl_doc))
    ext_doc["objects"]["3"]["effects"][0]["Extrude3D"]["rotation_3d"] = [20.0, -25.0, 10.0]
    ext_doc["objects"]["3"]["effects"][0]["Extrude3D"]["lights"] = [
        {"dir": [0.5, -0.5, 0.7], "intensity": 0.8},
        {"dir": [-0.6, 0.4, 0.7], "intensity": 0.5},
    ]
    ext_in = work / "ext3_rot_in.coree"
    ext_png = work / "ext3_rot_out.png"
    _write_coree_from_doc(ext_in, "Extrude3D Rotated Multi-Light Test", ext_doc)
    rext = run_cmd([str(ctx.cli_bin), "render", str(ext_in), str(ext_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if rext.returncode != 0 or not ext_png.is_file() or _compare_same_page_pngs(ext_ctrl_png, ext_png, "W3 ADD-01 Extrude3D rot")["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", "W3 ADD-01: Extrude3D 3D rotation / multi-light produced no visible diff vs zero-rotation control")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01: Extrude3D PNG malformed: {exc}")

    # 5c. TransparencyMode::{Pattern, Texture, Conical, Square} rendered over colored background vs opaque and Uniform
    bg_obj = json.loads(json.dumps(rect_obj))
    bg_obj["id"] = 2
    bg_obj["name"] = "Blue Background"
    bg_obj["payload"] = {"Shape": {"Rectangle": {"width": 100.0, "height": 100.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}}
    bg_obj["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 10.0, 10.0]}
    bg_obj["fill"] = {"Solid": {"Rgba": {"r": 0.0, "g": 0.25, "b": 0.9, "a": 1.0}}}

    fg_obj = json.loads(json.dumps(rect_obj))
    fg_obj["id"] = 3
    fg_obj["name"] = "Red Foreground"
    fg_obj["payload"] = {"Shape": {"Rectangle": {"width": 60.0, "height": 60.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}}
    fg_obj["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 30.0]}
    fg_obj["fill"] = {"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}}

    tr_base_doc = json.loads(json.dumps(fx_base_doc))
    tr_base_doc["objects"] = {"2": bg_obj, "3": fg_obj}
    tr_base_doc["pages"][0]["layers"][0]["objects"] = [2, 3]
    tr_base_doc["next_id"] = 4

    tr_opaque_in = work / "tr_opaque.coree"
    tr_opaque_png = work / "tr_opaque.png"
    _write_coree_from_doc(tr_opaque_in, "Transparency Opaque Control", tr_base_doc)
    r_tro = run_cmd([str(ctx.cli_bin), "render", str(tr_opaque_in), str(tr_opaque_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_tro.returncode != 0 or not tr_opaque_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01: transparency opaque control failed: {r_tro.stderr}")

    tr_uni_doc = json.loads(json.dumps(tr_base_doc))
    tr_uni_doc["objects"]["3"]["effects"] = [{"Transparency": {"mode": {"Uniform": 0.5}, "blend_mode": "Normal"}}]
    tr_uni_in = work / "tr_uni.coree"
    tr_uni_png = work / "tr_uni.png"
    _write_coree_from_doc(tr_uni_in, "Transparency Uniform Control", tr_uni_doc)
    r_tru = run_cmd([str(ctx.cli_bin), "render", str(tr_uni_in), str(tr_uni_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_tru.returncode != 0 or not tr_uni_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-01: transparency uniform control failed: {r_tru.stderr}")

    tmode_payloads = {
        "Pattern": {"Pattern": {"pattern_name": "checker", "tile_w": 8.0, "tile_h": 8.0, "fg_opacity": 1.0, "bg_opacity": 0.1}},
        "Texture": {"Texture": {"preset": "marble", "scale": 6.0, "fg_opacity": 1.0, "bg_opacity": 0.1}},
        "Conical": {"Conical": {"center": {"x": 60.0, "y": 60.0}, "angle_deg": 0.0, "start_opacity": 1.0, "end_opacity": 0.1}},
        "Square": {"Square": {"center": {"x": 60.0, "y": 60.0}, "size": 50.0, "angle_deg": 0.0, "start_opacity": 1.0, "end_opacity": 0.1}},
    }
    for tm_name, tm_payload in tmode_payloads.items():
        tm_doc = json.loads(json.dumps(tr_base_doc))
        tm_doc["objects"]["3"]["effects"] = [{"Transparency": {"mode": tm_payload, "blend_mode": "Normal"}}]
        tm_in = work / f"tr_{tm_name.lower()}.coree"
        tm_png = work / f"tr_{tm_name.lower()}.png"
        _write_coree_from_doc(tm_in, f"Transparency {tm_name}", tm_doc)
        r_tm = run_cmd([str(ctx.cli_bin), "render", str(tm_in), str(tm_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r_tm.returncode != 0 or not tm_png.is_file():
            return TestOutcome("FAIL", f"W3 ADD-01: TransparencyMode::{tm_name} render failed: {r_tm.stderr.strip()}")
        try:
            d_opq = _compare_same_page_pngs(tr_opaque_png, tm_png, f"W3 ADD-01 TransparencyMode::{tm_name} vs opaque")["mean_abs_diff"]
            d_uni = _compare_same_page_pngs(tr_uni_png, tm_png, f"W3 ADD-01 TransparencyMode::{tm_name} vs uniform")["mean_abs_diff"]
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W3 ADD-01: TransparencyMode::{tm_name} PNG malformed: {exc}")
        if d_opq <= 0.5 or d_uni <= 0.5:
            return TestOutcome("FAIL", f"W3 ADD-01: TransparencyMode::{tm_name} indistinguishable from opaque/Uniform (d_opq={d_opq:.3f}, d_uni={d_uni:.3f})")

    # 5d. 4-curve cubic Envelope (12-point S-curve cubic cage vs 4-corner straight/bilinear control)
    env_ctrl_doc = json.loads(json.dumps(fx_base_doc))
    env_ctrl_doc["objects"]["3"]["effects"] = [{
        "Envelope": {
            "mode": "Straight",
            "cage": [{"x": 0.0, "y": 0.0}, {"x": 40.0, "y": 0.0}, {"x": 40.0, "y": 30.0}, {"x": 0.0, "y": 30.0}],
        }
    }]
    env_ctrl_in = work / "env_ctrl.coree"
    env_ctrl_png = work / "env_ctrl.png"
    _write_coree_from_doc(env_ctrl_in, "Envelope Straight Control", env_ctrl_doc)
    re_ctrl = run_cmd([str(ctx.cli_bin), "render", str(env_ctrl_in), str(env_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if re_ctrl.returncode != 0 or not env_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: Envelope straight control render failed: {re_ctrl.stderr}")

    env_doc = json.loads(json.dumps(fx_base_doc))
    env_doc["objects"]["3"]["effects"] = [{
        "Envelope": {
            "mode": "Unconstrained",
            "cage": [
                {"x": 0.0, "y": 0.0}, {"x": 13.0, "y": -18.0}, {"x": 27.0, "y": 18.0}, {"x": 40.0, "y": 0.0},
                {"x": 40.0, "y": 10.0}, {"x": 40.0, "y": 20.0}, {"x": 40.0, "y": 30.0},
                {"x": 27.0, "y": 30.0}, {"x": 13.0, "y": 30.0}, {"x": 0.0, "y": 30.0},
                {"x": 0.0, "y": 20.0}, {"x": 0.0, "y": 10.0},
            ],
        }
    }]
    env_in = work / "env_cubic.coree"
    env_png = work / "env_cubic.png"
    _write_coree_from_doc(env_in, "Envelope 4-Curve Cubic Test", env_doc)
    renv = run_cmd([str(ctx.cli_bin), "render", str(env_in), str(env_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if renv.returncode != 0 or not env_png.is_file() or _compare_same_page_pngs(env_ctrl_png, env_png, "W3 IMP-04 Envelope")["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", "W3 IMP-04: 4-curve cubic Envelope produced no visible warp vs 4-corner control")
        env_bytes = env_png.read_bytes()
        bulge_px = _w3_px_at_mm(env_bytes, 38.5, 37.0)
        dip_px = _w3_px_at_mm(env_bytes, 57.0, 43.0)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: Envelope PNG malformed: {exc}")
    if min(bulge_px[:3]) >= 245 or min(dip_px[:3]) < 245:
        return TestOutcome("FAIL", f"W3 IMP-04: 4-curve cubic S-envelope failed cubic inflection test (bulge={bulge_px}, dip={dip_px})")

    # 5e. Blend tangent rotation: elongated 30x4 mm horizontal bar along S-curve (exact vertical tangent B'(0.5)=(0,90) at t=0.5)
    bl_doc = json.loads(json.dumps(fx_base_doc))
    bar_shape = {"Shape": {"Rectangle": {"width": 30.0, "height": 4.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}}
    obj_start = json.loads(json.dumps(rect_obj))
    obj_start["id"] = 3
    obj_start["name"] = "Blend Start Bar"
    obj_start["payload"] = bar_shape
    obj_start["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 15.0, 28.0]}
    obj_start["fill"] = {"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}}

    obj_end = json.loads(json.dumps(obj_start))
    obj_end["id"] = 4
    obj_end["name"] = "Blend End Bar"
    obj_end["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 75.0, 88.0]}

    obj_host = {
        "id": 5,
        "name": "Blend S-Curve Host",
        "payload": {
            "Path": {
                "subpaths": [{
                    "nodes": [
                        {"pos": {"x": 30.0, "y": 30.0}, "handle_in": None, "handle_out": {"x": 90.0, "y": 30.0}, "kind": "Smooth"},
                        {"pos": {"x": 90.0, "y": 90.0}, "handle_in": {"x": 30.0, "y": 90.0}, "handle_out": None, "kind": "Smooth"},
                    ],
                    "closed": False,
                }]
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": {**rect_obj["outline"], "width_mm": 0.0, "is_hairline": False},
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
    bl_doc["objects"] = {"3": obj_start, "4": obj_end, "5": obj_host}
    bl_doc["pages"][0]["layers"][0]["objects"] = [3, 4, 5]
    bl_doc["next_id"] = 6
    bl_ctrl_in = work / "bl_ctrl.coree"
    bl_ctrl_png = work / "bl_ctrl.png"
    _write_coree_from_doc(bl_ctrl_in, "Blend Control", bl_doc)
    rblc = run_cmd([str(ctx.cli_bin), "render", str(bl_ctrl_in), str(bl_ctrl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rblc.returncode != 0 or not bl_ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 IMP-04: Blend control render failed: {rblc.stderr}")
    bl_doc["objects"]["3"]["effects"] = [{
        "Blend": {"target_id": 4, "steps": 1, "rotation_deg": 0.0, "along_path": 5}
    }]
    bl_in = work / "bl_in.coree"
    bl_png = work / "bl_out.png"
    _write_coree_from_doc(bl_in, "Blend Tangent Rotation", bl_doc)
    rbl = run_cmd([str(ctx.cli_bin), "render", str(bl_in), str(bl_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    try:
        if rbl.returncode != 0 or not bl_png.is_file() or _compare_same_page_pngs(bl_ctrl_png, bl_png, "W3 IMP-04 Blend")["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", "W3 IMP-04: Blend along path CLI render produced no visible intermediate shapes")
        bl_bytes = bl_png.read_bytes()
        rot_top_px = _w3_px_at_mm(bl_bytes, 60.0, 48.0)
        rot_bot_px = _w3_px_at_mm(bl_bytes, 60.0, 72.0)
        unrot_side_px = _w3_px_at_mm(bl_bytes, 48.0, 60.0)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04: Blend PNG malformed: {exc}")
    if rot_top_px[0] < 150 or rot_top_px[2] > 100 or rot_bot_px[0] < 150 or unrot_side_px[0] < 245:
        return TestOutcome(
            "FAIL",
            f"W3 IMP-04: Blend along S-curve did not rotate step to vertical path tangent at midpoint (top={rot_top_px}, bot={rot_bot_px}, side={unrot_side_px})",
        )

    for filt, label, min_p in (
        ("bevel", "W3 ADD-01 Bevel", 2),
        ("extrude", "W3 ADD-01 Extrude3D", 1),
        ("blend", "W3 IMP-04 Blend", 3),
        ("envelope", "W3 IMP-04 Envelope", 2),
        ("transparency", "W3 ADD-01 Transparency", 2),
    ):
        fail_fx = _run_cargo_tests(ctx, ["-p", "coree-render", "--", filt], label, min_passed=min_p)
        if fail_fx is not None:
            return fail_fx

    return TestOutcome(
        "PASS",
        f"InnerShadow/Bevel/Extrude3D/Transparency modes/4-curve Envelope/Blend tangent CLI+cargo + MeshFill ({mesh_info['unique_colors_sample']} colors) + Frozen Lens persistence verified",
    )


def _test_w3_typography_and_stylus(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W3-TYPO")
    tl_code = _read_rust_stripped(ctx, "crates/coree-render/src/text_layout.rs")
    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
    render_tl = tl_code + "\n" + ren_code

    if _has_empty_match_arm(tl_code, r"(?:SwashContent::)?SubpixelMask"):
        return TestOutcome("FAIL", "W3 ADD-10: SwashContent::SubpixelMask in text_layout.rs is still an empty no-op stub")
    subpixel_arms = _extract_match_arm_bodies(tl_code, r"(?:SwashContent::)?SubpixelMask")
    subpixel_write_rx = re.compile(
        r"\brgba\s*\[[^\]]+\]\s*(?:[+\-*/&|^]?=(?![=>]))"
        r"|\brgba\s*(?:\[[^\]]*\]\s*)?\.\s*(?:copy_from_slice|iter_mut|chunks_mut|chunks_exact_mut|fill|push|extend|resize)\s*\("
        r"|&\s*mut\s+rgba(?:\s*\[[^\]]*\])?\b"
    )
    if not subpixel_arms or not all(subpixel_write_rx.search(arm) for arm in subpixel_arms):
        return TestOutcome("FAIL", "W3 ADD-10: SwashContent::SubpixelMask arm(s) in text_layout.rs do not mutate rgba pixel data")
    if not re.search(r"\bSubpixelMask\b", tl_code):
        return TestOutcome("FAIL", "W3 ADD-10: SwashContent::SubpixelMask missing in text_layout.rs")
    if not re.search(r"\bdrop_cap\b|\bDropCap\b", render_tl):
        return TestOutcome("FAIL", "W3 ADD-10: coree-render does not implement ParagraphText.drop_cap rendering")
    if not re.search(r"\blinked_next_frame\b", render_tl) or not re.search(r"\bTextWrapMode\b|\bwrap_mode\b", render_tl):
        return TestOutcome("FAIL", "W3 ADD-10: coree-render does not lay out ParagraphText.linked_next_frame or TextWrapMode")
    if not re.search(r"\bfvar\b", tl_code, re.I):
        return TestOutcome("FAIL", "W3 ADD-10: OpenType variable font (fvar) axes missing in text_layout.rs")
    if not re.search(r"\blayout_text_on_path_pieces\b", tl_code) or not re.search(r"\bapply_kashida_elongation\b", tl_code):
        return TestOutcome("FAIL", "W3 ADD-10/IMP-10: TextOnPath or Kashida elongation missing in text_layout.rs")
    top_fn = _extract_brace_block(tl_code, r"\bfn\s+layout_text_on_path_pieces\b")
    if not top_fn or re.search(r"\.layout_runs\s*\(\s*\)\s*\.take\s*\(\s*1\s*\)", top_fn):
        return TestOutcome("FAIL", "W3 ADD-10: layout_text_on_path_pieces still caps layout_runs() with .take(1) (blocks multi-line TextOnPath)")

    stylus_code = _read_rust_stripped(ctx, "crates/coree-app/src/stylus.rs")
    if not stylus_code or not re.search(r"\bStylusProvider\b", stylus_code):
        return TestOutcome("FAIL", "W3 IMP-03: trait/struct StylusProvider missing in crates/coree-app/src/stylus.rs")
    display_icc_code = _read_rust_stripped(ctx, "crates/coree-app/src/display_icc.rs")
    if not display_icc_code:
        app_src = ctx.repo_root / "crates" / "coree-app" / "src"
        display_icc_code = "\n".join(
            strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace"))
            for p in sorted(app_src.rglob("*.rs"))
        )
    if not re.search(r"\bDisplayIccProvider\b", display_icc_code):
        return TestOutcome("FAIL", "W3 IMP-03: DisplayIccProvider missing in crates/coree-app/src/display_icc.rs (or coree-app/src)")

    # Runtime CLI render checks isolating TextOnPath (multi-line), ParagraphText, linked_next_frame overflow, and TextWrapMode
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    host_path_obj = {
        "id": 10,
        "name": "Text Host Curve",
        "payload": {
            "Path": {
                "subpaths": [
                    {
                        "nodes": [
                            {"pos": {"x": 20.0, "y": 80.0}, "handle_in": None, "handle_out": {"x": 55.0, "y": 35.0}, "kind": "Cusp"},
                            {"pos": {"x": 160.0, "y": 80.0}, "handle_in": {"x": 125.0, "y": 35.0}, "handle_out": None, "kind": "Cusp"},
                        ],
                        "closed": False,
                    }
                ]
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": {**base_outline, "width_mm": 0.4},
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
    doc["objects"] = {"10": host_path_obj}
    doc["pages"][0]["layers"][0]["objects"] = [10]
    doc["pages"][0]["layers"][1]["objects"] = []
    doc["next_id"] = 11

    ctrl_in = work / "typo_path_only.coree"
    ctrl_png = work / "typo_path_only.png"
    _write_coree_from_doc(ctrl_in, "Path Only Control", doc)
    rc_ctrl = run_cmd([str(ctx.cli_bin), "render", str(ctrl_in), str(ctrl_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if rc_ctrl.returncode != 0 or not ctrl_png.is_file():
        return TestOutcome("FAIL", f"W3 typography control render failed: {rc_ctrl.stderr}")
    try:
        ctrl_info = parse_png_info(ctrl_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 typography control PNG malformed: {exc}")

    # 1) Isolated 1-line vs 2-line multi-line TextOnPath render ([10, 11] vs [10] & 1-line vs 2-line)
    doc["objects"]["11"] = {
        "id": 11,
        "name": "Text On Path",
        "payload": {
            "TextOnPath": {
                "text": {
                    "runs": [{"text": "coree Fit Path", "style": {**base_style, "font_size_pt": 16.0}}],
                    "direction": "Auto",
                    "align": "Left",
                },
                "path_id": 10,
                "offset_mm": 5.0,
                "distance_mm": 2.0,
                "flip_vertical": False,
                "flip_horizontal": False,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": {**base_outline, "width_mm": 0.0},
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
    doc["pages"][0]["layers"][0]["objects"] = [10, 11]
    doc["next_id"] = 12
    top_1line_in = work / "typo_top_1line.coree"
    top_1line_png = work / "typo_top_1line.png"
    _write_coree_from_doc(top_1line_in, "TextOnPath 1-Line Control", doc)
    r_top1 = run_cmd([str(ctx.cli_bin), "render", str(top_1line_in), str(top_1line_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_top1.returncode != 0 or not top_1line_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-39: TextOnPath 1-line control render failed: {r_top1.stderr}")
    try:
        top_1line_info = parse_png_info(top_1line_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-39: TextOnPath 1-line PNG malformed: {exc}")

    doc["objects"]["11"]["payload"]["TextOnPath"]["text"]["runs"][0]["text"] = "coree Fit Path\nSecond Line كوري"
    top_in = work / "typo_top_only.coree"
    top_png = work / "typo_top_only.png"
    _write_coree_from_doc(top_in, "TextOnPath Isolated W3", doc)
    r_top = run_cmd([str(ctx.cli_bin), "render", str(top_in), str(top_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_top.returncode != 0 or not top_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-39: TextOnPath isolated render failed: {r_top.stderr}")
    try:
        top_info = parse_png_info(top_png.read_bytes())
        top_diff = _compare_same_page_pngs(ctrl_png, top_png, "W3 FEAT-39 TextOnPath")
        top_2line_diff = _compare_same_page_pngs(top_1line_png, top_png, "W3 ADD-10 TextOnPath 2-line")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-39: TextOnPath PNG malformed: {exc}")
    if top_diff["mean_abs_diff"] <= 0.15 or top_info["non_white_pixels"] <= ctrl_info["non_white_pixels"]:
        return TestOutcome(
            "FAIL",
            f"W3 FEAT-39: Isolated TextOnPath produced insufficient visual output vs path control: diff={top_diff}, top_px={top_info['non_white_pixels']}, ctrl_px={ctrl_info['non_white_pixels']}",
        )
    if top_2line_diff["mean_abs_diff"] <= 0.05 or top_info["non_white_pixels"] <= top_1line_info["non_white_pixels"]:
        return TestOutcome(
            "FAIL",
            f"W3 ADD-10: Multi-line TextOnPath produced no extra rendered pixels over 1-line control: diff={top_2line_diff}, px_1line={top_1line_info['non_white_pixels']}, px_2line={top_info['non_white_pixels']}",
        )

    # 2) Isolated ParagraphText frame 12 ([10, 11, 12] vs [10, 11]) + linked overflow frame 13 ([10, 11, 12, 13])
    overflow_text = " ".join(["مرحبا بكم في استوديو التصميم المتجهي كوري"] * 6)
    kashida_style = {**base_style, "font_size_pt": 18.0, "direction": "Rtl", "align": "ForceJustifyKashida", "kashida_stretch": 1.0}
    doc["objects"]["12"] = {
        "id": 12,
        "name": "Primary Paragraph Frame",
        "payload": {
            "ParagraphText": {
                "runs": [{"text": overflow_text, "style": kashida_style}],
                "frame_size": {"x": 80.0, "y": 25.0},
                "columns": 1,
                "column_gutter_mm": 5.0,
                "linked_next_frame": None,
                "wrap_mode": "None",
                "drop_cap": None,
                "direction": "Rtl",
                "align": "ForceJustifyKashida",
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 110.0]},
        "fill": "None",
        "outline": {**base_outline, "width_mm": 0.0},
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
    doc["pages"][0]["layers"][0]["objects"] = [10, 11, 12]
    doc["next_id"] = 13
    no_link_in = work / "typo_no_link.coree"
    no_link_png = work / "typo_no_link.png"
    _write_coree_from_doc(no_link_in, "Paragraph Isolated W3", doc)
    r_nl = run_cmd([str(ctx.cli_bin), "render", str(no_link_in), str(no_link_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_nl.returncode != 0 or not no_link_png.is_file():
        return TestOutcome("FAIL", f"W3 FEAT-38: ParagraphText render failed: {r_nl.stderr}")
    try:
        para_diff = _compare_same_page_pngs(top_png, no_link_png, "W3 FEAT-38 ParagraphText")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 FEAT-38: ParagraphText PNG malformed: {exc}")
    if para_diff["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W3 FEAT-38: ParagraphText produced no independent visual output over TextOnPath: {para_diff}")

    # Link frame 12 -> empty frame 13 positioned below frame 12 (y=148..193mm)
    doc["objects"]["12"]["payload"]["ParagraphText"]["linked_next_frame"] = 13
    doc["objects"]["13"] = {
        "id": 13,
        "name": "Overflow Paragraph Frame",
        "payload": {
            "ParagraphText": {
                "runs": [{"text": "", "style": kashida_style}],
                "frame_size": {"x": 80.0, "y": 45.0},
                "columns": 1,
                "column_gutter_mm": 5.0,
                "linked_next_frame": None,
                "wrap_mode": "None",
                "drop_cap": None,
                "direction": "Rtl",
                "align": "ForceJustifyKashida",
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 148.0]},
        "fill": "None",
        "outline": {**base_outline, "width_mm": 0.0},
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
    doc["pages"][0]["layers"][0]["objects"] = [10, 11, 12, 13]
    doc["next_id"] = 14
    linked_in = work / "typo_linked.coree"
    linked_png = work / "typo_linked.png"
    _write_coree_from_doc(linked_in, "Linked Paragraph W3", doc)
    r_lk = run_cmd([str(ctx.cli_bin), "render", str(linked_in), str(linked_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_lk.returncode != 0 or not linked_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: linked_next_frame render failed: {r_lk.stderr}")

    try:
        pw_nl, ph_nl, rgba_nl = _decode_png_rgba_bytes_stdlib(no_link_png.read_bytes())
        pw_lk, ph_lk, rgba_lk = _decode_png_rgba_bytes_stdlib(linked_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-10: linked_next_frame PNG malformed: {exc}")
    if (pw_nl, ph_nl) != (pw_lk, ph_lk) or pw_lk < 32 or ph_lk < 32:
        return TestOutcome(
            "FAIL",
            f"W3 ADD-10: linked_next_frame PNG dimensions mismatch or too small: {pw_nl}x{ph_nl} vs {pw_lk}x{ph_lk}",
        )
    x0 = int((20.0 / 210.0) * pw_lk)
    x1 = int((105.0 / 210.0) * pw_lk)
    y0 = int((148.0 / 297.0) * ph_lk)
    y1 = int((193.0 / 297.0) * ph_lk)
    nl_f13_px = sum(
        1
        for yy in range(y0, min(y1, ph_nl))
        for xx in range(x0, min(x1, pw_nl))
        if (
            rgba_nl[(yy * pw_nl + xx) * 4] < 245
            or rgba_nl[(yy * pw_nl + xx) * 4 + 1] < 245
            or rgba_nl[(yy * pw_nl + xx) * 4 + 2] < 245
        )
    )
    lk_f13_px = sum(
        1
        for yy in range(y0, min(y1, ph_lk))
        for xx in range(x0, min(x1, pw_lk))
        if (
            rgba_lk[(yy * pw_lk + xx) * 4] < 245
            or rgba_lk[(yy * pw_lk + xx) * 4 + 1] < 245
            or rgba_lk[(yy * pw_lk + xx) * 4 + 2] < 245
        )
    )
    if lk_f13_px - nl_f13_px < 30:
        return TestOutcome(
            "FAIL",
            f"W3 ADD-10: linked_next_frame=13 failed to render overflow text into frame 13 region (before={nl_f13_px}, after={lk_f13_px})",
        )

    # 3) TextWrapMode::SquareBoth around an overlapping opaque obstacle vs wrap_mode=None
    _, wrap_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    wrap_doc["objects"] = {
        "12": json.loads(json.dumps(doc["objects"]["12"])),
        "14": {
            "id": 14,
            "name": "Wrap Obstacle",
            "payload": {
                "Shape": {"Rectangle": {"width": 25.0, "height": 20.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 45.0, 112.0]},
            "fill": {"Solid": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}},
            "outline": {**base_outline, "width_mm": 0.0},
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
    wrap_doc["objects"]["12"]["payload"]["ParagraphText"]["linked_next_frame"] = None
    wrap_doc["objects"]["12"]["payload"]["ParagraphText"]["wrap_mode"] = "None"
    wrap_doc["pages"][0]["layers"][0]["objects"] = [12, 14]
    wrap_doc["pages"][0]["layers"][1]["objects"] = []
    wrap_doc["next_id"] = 15
    wrap_none_in = work / "typo_wrap_none.coree"
    wrap_none_png = work / "typo_wrap_none.png"
    _write_coree_from_doc(wrap_none_in, "Wrap None", wrap_doc)
    r_wn = run_cmd([str(ctx.cli_bin), "render", str(wrap_none_in), str(wrap_none_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_wn.returncode != 0 or not wrap_none_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: TextWrapMode=None render failed: {r_wn.stderr}")

    wrap_doc["objects"]["12"]["payload"]["ParagraphText"]["wrap_mode"] = "SquareBoth"
    wrap_sq_in = work / "typo_wrap_sq.coree"
    wrap_sq_png = work / "typo_wrap_sq.png"
    _write_coree_from_doc(wrap_sq_in, "Wrap SquareBoth", wrap_doc)
    r_ws = run_cmd([str(ctx.cli_bin), "render", str(wrap_sq_in), str(wrap_sq_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ws.returncode != 0 or not wrap_sq_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: TextWrapMode=SquareBoth render failed: {r_ws.stderr}")
    try:
        wrap_diff = _compare_same_page_pngs(wrap_none_png, wrap_sq_png, "W3 ADD-10 TextWrapMode")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-10: TextWrapMode PNG malformed: {exc}")
    if wrap_diff["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W3 ADD-10: TextWrapMode::SquareBoth did not wrap text around overlapping obstacle: {wrap_diff}")

    # 4) DropCap runtime render diff (drop_cap=None vs DropCapConfig enabled=True)
    _, dc_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    dc_style = {**base_style, "font_size_pt": 12.0, "direction": "Ltr", "align": "Left"}
    dc_para = {
        "runs": [
            {
                "text": "CorelDRAW clean-room studio paragraph drop cap layout test across multiple wrapped lines inside frame.",
                "style": dc_style,
            }
        ],
        "frame_size": {"x": 85.0, "y": 45.0},
        "columns": 1,
        "column_gutter_mm": 5.0,
        "linked_next_frame": None,
        "wrap_mode": "None",
        "drop_cap": None,
        "direction": "Ltr",
        "align": "Left",
    }
    dc_doc["objects"] = {
        "15": {
            "id": 15,
            "name": "DropCap Frame",
            "payload": {"ParagraphText": dc_para},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": "None",
            "outline": {**base_outline, "width_mm": 0.0},
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
    }
    dc_doc["pages"][0]["layers"][0]["objects"] = [15]
    dc_doc["pages"][0]["layers"][1]["objects"] = []
    dc_doc["next_id"] = 16
    dc_none_in = work / "typo_dropcap_none.coree"
    dc_none_png = work / "typo_dropcap_none.png"
    _write_coree_from_doc(dc_none_in, "DropCap None", dc_doc)
    r_dcn = run_cmd([str(ctx.cli_bin), "render", str(dc_none_in), str(dc_none_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dcn.returncode != 0 or not dc_none_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: DropCap=None render failed: {r_dcn.stderr}")

    dc_doc["objects"]["15"]["payload"]["ParagraphText"]["drop_cap"] = {"enabled": True, "lines": 3, "gap_mm": 1.5}
    dc_on_in = work / "typo_dropcap_on.coree"
    dc_on_png = work / "typo_dropcap_on.png"
    _write_coree_from_doc(dc_on_in, "DropCap Enabled", dc_doc)
    r_dco = run_cmd([str(ctx.cli_bin), "render", str(dc_on_in), str(dc_on_png), "--dpi", "96"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dco.returncode != 0 or not dc_on_png.is_file():
        return TestOutcome("FAIL", f"W3 ADD-10: DropCap=enabled render failed: {r_dco.stderr}")
    try:
        dc_diff = _compare_same_page_pngs(dc_none_png, dc_on_png, "W3 ADD-10 DropCap", min_non_white_b=51)
        if dc_diff["mean_abs_diff"] <= 0.2 or parse_png_info(dc_on_png.read_bytes())["non_white_pixels"] <= 50:
            return TestOutcome("FAIL", f"W3 ADD-10: DropCap enabled=True produced no visual change vs drop_cap=None: {dc_diff}")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-10: DropCap PNG malformed: {exc}")

    fail = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "kashida"], "W3 ADD-10/IMP-10")
    if fail is not None:
        return fail
    fail_sub = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "subpixel"], "W3 ADD-10 SubpixelMask", min_passed=1)
    if fail_sub is not None:
        return fail_sub
    return TestOutcome(
        "PASS",
        f"TextOnPath (diff={top_diff['mean_abs_diff']:.2f}) + ParagraphText (diff={para_diff['mean_abs_diff']:.2f}) + linked_next_frame (+{lk_f13_px - nl_f13_px}px) + TextWrapMode (diff={wrap_diff['mean_abs_diff']:.2f}) + DropCap (diff={dc_diff['mean_abs_diff']:.2f}) + Stylus/DisplayICC verified",
    )


def _test_w4_pdfx_and_font_subsetting(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-PDFX")
    src = _f(ctx, "fixture.coree")
    out_pdf = work / "pdfx_bilingual.pdf"
    r = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_pdf), "--cmyk", "--crop-marks"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0 or not out_pdf.is_file():
        return TestOutcome("FAIL", f"convert -> PDF failed: {r.stderr}")

    try:
        pdf_s = parse_pdf_structure(out_pdf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REQ-03: Exported PDF malformed: {exc}")
    if not (pdf_s["has_pdfx_version"] and pdf_s["has_output_intents"] and pdf_s["has_trapped"]):
        return TestOutcome("FAIL", f"W4 REQ-03: Exported PDF lacks PDF/X-1a/4 markers (/GTS_PDFXVersion, /OutputIntents, /Trapped): {pdf_s}")

    # External Ghostscript & Poppler pdfinfo/pdffonts/pdftotext oracles
    pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
    fields = pinfo.get("fields", {})
    if (
        not pinfo["ok"]
        or fields.get("Pages") != "1"
        or "MediaBox" not in fields
        or "TrimBox" not in fields
        or "BleedBox" not in fields
        or fields["TrimBox"] == fields["MediaBox"]
    ):
        return TestOutcome(
            "FAIL",
            f"W4 REQ-03: Poppler pdfinfo -box missing MediaBox/TrimBox/BleedBox or TrimBox == MediaBox under --crop-marks: {fields}",
        )

    gs_res = run_ghostscript_render(out_pdf, work / "gs_pdfx.png", dpi=96, device="png16m", timeout_scale=ctx.timeout_scale)
    if not gs_res["ok"] or gs_res["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"W4 REQ-03: Ghostscript failed to rasterize PDF/X cleanly: {gs_res}")

    fonts_rep = run_poppler_fonts(out_pdf, timeout_scale=ctx.timeout_scale)
    if not fonts_rep["ok"]:
        return TestOutcome("FAIL", f"W4 REQ-03: pdffonts failed: {fonts_rep}")
    cid_types = {
        "CID TrueType",
        "CID TrueType (OT)",
        "Type 0",
        "CID Type 0",
        "CID Type 0 (OT)",
        "CID Type 0C",
        "CID Type 0C (OT)",
        "TrueType (CID)",
    }
    has_cid_subset = any(
        f["type"] in cid_types and f["emb"] == "yes" and f["sub"] == "yes" and f["uni"] == "yes"
        for f in fonts_rep.get("fonts", [])
    )
    if not has_cid_subset:
        return TestOutcome("FAIL", f"W4 REQ-03: pdffonts shows no subsetted CIDFontType2+ToUnicode row: {fonts_rep}")

    txt_rep = run_poppler_text(out_pdf, timeout_scale=ctx.timeout_scale)
    if not txt_rep["ok"] or "coree Tier1" not in txt_rep.get("text", "") or "كوري" not in txt_rep.get("text", ""):
        return TestOutcome("FAIL", f"W4 REQ-03: pdftotext failed to extract bilingual Latin+Arabic text: {txt_rep}")

    return TestOutcome("PASS", "PDF/X + Ghostscript + pdfinfo + subsetted CIDFontType2 + pdftotext Arabic/Latin verified")


_PASSTHROUGH_RUST_METHODS = frozenset({
    "unwrap",
    "expect",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "ok",
    "clone",
    "as_ref",
    "as_mut",
    "borrow",
    "borrow_mut",
    "deref",
    "deref_mut",
})

_RUST_NON_CALL_KEYWORDS = frozenset({
    "if",
    "while",
    "for",
    "match",
    "return",
    "let",
    "fn",
    "where",
    "else",
    "loop",
})


def _find_matching_close_forward(s: str, open_idx: int, open_ch: str, close_ch: str) -> Optional[int]:
    n = len(s)
    depth = 1
    i = open_idx + 1
    while i < n:
        ch = s[i]
        if ch == open_ch:
            depth += 1
        elif ch == close_ch:
            depth -= 1
            if depth == 0:
                return i
        i += 1
    return None


def _find_matching_open_backward(s: str, close_idx: int, open_ch: str, close_ch: str) -> Optional[int]:
    depth = 1
    i = close_idx - 1
    while i >= 0:
        ch = s[i]
        if ch == close_ch:
            depth += 1
        elif ch == open_ch:
            depth -= 1
            if depth == 0:
                return i
        i -= 1
    return None


def _skip_balanced_angle_forward(s: str, open_idx: int) -> Optional[int]:
    n = len(s)
    depth = 1
    i = open_idx + 1
    while i < n:
        ch = s[i]
        if ch == "(":
            cl = _find_matching_close_forward(s, i, "(", ")")
            if cl is None:
                return None
            i = cl + 1
            continue
        if ch == "[":
            cl = _find_matching_close_forward(s, i, "[", "]")
            if cl is None:
                return None
            i = cl + 1
            continue
        if ch == "<":
            depth += 1
        elif ch == ">":
            if i > 0 and s[i - 1] in ("-", "="):
                i += 1
                continue
            depth -= 1
            if depth == 0:
                return i + 1
        elif ch in ";{}":
            return None
        i += 1
    return None


def _find_matching_angle_backward(s: str, close_idx: int) -> Optional[int]:
    depth = 1
    i = close_idx - 1
    while i >= 0:
        ch = s[i]
        if ch == ")":
            op = _find_matching_open_backward(s, i, "(", ")")
            if op is None:
                return None
            i = op - 1
            continue
        if ch == "]":
            op = _find_matching_open_backward(s, i, "[", "]")
            if op is None:
                return None
            i = op - 1
            continue
        if ch == ">":
            if i > 0 and s[i - 1] in ("-", "="):
                i -= 2
                continue
            depth += 1
        elif ch == "<":
            depth -= 1
            if depth == 0:
                return i
        elif ch in ";{}":
            return None
        i -= 1
    return None


def _scan_rust_path_backward(s: str, cur: int) -> Optional[int]:
    if cur < 0 or not (s[cur].isalnum() or s[cur] == "_"):
        return None
    pos = cur
    while True:
        while pos >= 0 and (s[pos].isalnum() or s[pos] == "_"):
            pos -= 1
        ident_start = pos + 1
        if s[ident_start].isdigit():
            return None
        k = pos
        while k >= 0 and s[k].isspace():
            k -= 1
        if k >= 1 and s[k - 1 : k + 1] == "::":
            j = k - 2
            while j >= 0 and s[j].isspace():
                j -= 1
            if j >= 0 and s[j] == ">":
                open_a = _find_matching_angle_backward(s, j)
                if open_a is not None:
                    m = open_a - 1
                    while m >= 0 and s[m].isspace():
                        m -= 1
                    if m >= 1 and s[m - 1 : m + 1] == "::":
                        n_j = m - 2
                        while n_j >= 0 and s[n_j].isspace():
                            n_j -= 1
                        if n_j >= 0 and (s[n_j].isalnum() or s[n_j] == "_"):
                            pos = n_j
                            continue
            if j >= 0 and (s[j].isalnum() or s[j] == "_"):
                pos = j
                continue
            return k - 1
        return ident_start


def _strip_rust_turbofish_from_path(path: str) -> str:
    out: list[str] = []
    i = 0
    n = len(path)
    while i < n:
        if path[i : i + 2] == "::":
            j = i + 2
            while j < n and path[j].isspace():
                j += 1
            if j < n and path[j] == "<":
                after_lt = _skip_balanced_angle_forward(path, j)
                if after_lt is not None:
                    i = after_lt
                    continue
        out.append(path[i])
        i += 1
    return re.sub(r"\s*::\s*", "::", "".join(out).strip())


_RUST_PRIMARY_TYPE_RX = re.compile(
    r"^\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?"
    r"(?:(?:[A-Za-z0-9_]+::)*(?:Result|Option)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?"
    r"(?:(?:[A-Za-z0-9_]+::)*(?:Box|Arc|Rc)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?"
    r"(?:[A-Za-z0-9_]+::)*([A-Z][A-Za-z0-9_]*)"
)


def _extract_impl_spans(mod_no_use: str) -> list[tuple[int, int, str]]:
    """Extract `(body_start, body_end, target_type)` for all `impl` blocks including nested generics."""
    n = len(mod_no_use)
    spans: list[tuple[int, int, str]] = []
    for m in re.finditer(r"\bimpl\b", mod_no_use):
        prev = m.start() - 1
        while prev >= 0 and mod_no_use[prev].isspace():
            prev -= 1
        if prev >= 0 and mod_no_use[prev] not in (";", "}", "{", "]"):
            if not re.search(r"\bunsafe\s*$", mod_no_use[: prev + 1]):
                continue
        pos = m.end()
        while pos < n and mod_no_use[pos].isspace():
            pos += 1
        if pos < n and mod_no_use[pos] == "<":
            after_lt = _skip_balanced_angle_forward(mod_no_use, pos)
            if after_lt is None:
                continue
            pos = after_lt
        hdr_start = pos
        paren = 0
        bracket = 0
        angle = 0
        ibs = -1
        i = pos
        while i < n:
            ch = mod_no_use[i]
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren -= 1
                if paren < 0:
                    break
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket -= 1
                if bracket < 0:
                    break
            elif ch == "<":
                angle += 1
            elif ch == ">":
                if not (i > 0 and mod_no_use[i - 1] in ("-", "=")):
                    angle = max(0, angle - 1)
            elif ch == ";" and paren == 0 and bracket == 0 and angle == 0:
                break
            elif ch == "{" and paren == 0 and bracket == 0 and angle == 0:
                ibs = i
                break
            i += 1
        if ibs < 0:
            continue
        depth = 1
        j = ibs + 1
        while j < n and depth > 0:
            if mod_no_use[j] == "{":
                depth += 1
            elif mod_no_use[j] == "}":
                depth -= 1
            j += 1
        if depth != 0:
            continue
        ibe = j
        header = mod_no_use[hdr_start:ibs]
        # Strip top-level `where ...` clause and split top-level ` for ` if trait impl
        h_n = len(header)
        h_p = 0
        h_b = 0
        h_a = 0
        where_idx: Optional[int] = None
        for_end: Optional[int] = None
        k = 0
        while k < h_n:
            ch = header[k]
            if ch == "(":
                h_p += 1
            elif ch == ")":
                h_p = max(0, h_p - 1)
            elif ch == "[":
                h_b += 1
            elif ch == "]":
                h_b = max(0, h_b - 1)
            elif ch == "<":
                h_a += 1
            elif ch == ">":
                if not (k > 0 and header[k - 1] in ("-", "=")):
                    h_a = max(0, h_a - 1)
            elif h_p == 0 and h_b == 0 and h_a == 0:
                if (
                    where_idx is None
                    and header[k : k + 5] == "where"
                    and (k == 0 or not (header[k - 1].isalnum() or header[k - 1] == "_"))
                    and (k + 5 >= h_n or not (header[k + 5].isalnum() or header[k + 5] == "_"))
                ):
                    where_idx = k
                    break
                if (
                    for_end is None
                    and header[k : k + 3] == "for"
                    and (k == 0 or not (header[k - 1].isalnum() or header[k - 1] == "_"))
                    and (k + 3 >= h_n or not (header[k + 3].isalnum() or header[k + 3] == "_"))
                ):
                    for_end = k + 3
            k += 1
        if where_idx is not None:
            header_main = header[:where_idx]
        else:
            header_main = header
        type_expr = header_main[for_end:].strip() if for_end is not None else header_main.strip()
        while type_expr.endswith(">"):
            open_a = _find_matching_angle_backward(type_expr, len(type_expr) - 1)
            if open_a is None:
                break
            type_expr = type_expr[:open_a].rstrip()
        m_ty = re.search(r"(?:[A-Za-z_][A-Za-z0-9_]*::)*([A-Za-z_][A-Za-z0-9_]*)\s*$", type_expr)
        if m_ty:
            spans.append((ibs, ibe, m_ty.group(1)))
    return spans


def _extract_match_arm_pattern_spans(match_body: str) -> list[tuple[list[str], int, int]]:
    """Extract `(vars_found, scope_start, scope_end)` per top-level match arm in `Ok(var)` / `Some(var)` patterns."""
    arms: list[tuple[list[str], int, int]] = []
    n = len(match_body)
    brace_d = 0
    paren_d = 0
    bracket_d = 0
    arm_pat_start = 0
    in_rhs = False
    cur_vars: list[str] = []
    cur_scope_start = 0
    i = 0
    pat_rx = re.compile(
        r"(?:[A-Za-z0-9_]+::)*(?:Ok|Some)\s*\(\s*(?:ref\s+)?(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*\)"
    )
    while i < n:
        ch = match_body[i]
        if not in_rhs and brace_d == 0 and paren_d == 0 and bracket_d == 0 and match_body[i : i + 2] == "=>":
            pat_slice = match_body[arm_pat_start:i]
            pms = list(pat_rx.finditer(pat_slice))
            cur_vars = [pm.group(1) for pm in pms]
            cur_scope_start = arm_pat_start + pms[-1].end() if pms else i + 2
            in_rhs = True
            i += 2
            continue
        if ch == "{":
            brace_d += 1
        elif ch == "}":
            if brace_d > 0:
                brace_d -= 1
                if in_rhs and brace_d == 0 and paren_d == 0 and bracket_d == 0:
                    k = i + 1
                    while k < n and match_body[k].isspace():
                        k += 1
                    continues_expr = (
                        k < n
                        and (
                            match_body[k] == "."
                            or (
                                match_body[k : k + 4] == "else"
                                and (k + 4 >= n or not (match_body[k + 4].isalnum() or match_body[k + 4] == "_"))
                            )
                        )
                    )
                    if not continues_expr:
                        if cur_vars:
                            arms.append((cur_vars, cur_scope_start, i + 1))
                        in_rhs = False
                        cur_vars = []
                        arm_pat_start = i + 1
        elif ch == "(":
            paren_d += 1
        elif ch == ")":
            paren_d = max(0, paren_d - 1)
        elif ch == "[":
            bracket_d += 1
        elif ch == "]":
            bracket_d = max(0, bracket_d - 1)
        elif ch == "," and brace_d == 0 and paren_d == 0 and bracket_d == 0:
            if in_rhs and cur_vars:
                arms.append((cur_vars, cur_scope_start, i))
            in_rhs = False
            cur_vars = []
            arm_pat_start = i + 1
        i += 1
    if in_rhs and cur_vars:
        arms.append((cur_vars, cur_scope_start, n))
    return arms


def _extract_match_arm_pattern_vars(match_body: str) -> list[str]:
    """Extract variable names bound in `Ok(var)` / `Some(var)` top-level match arm patterns."""
    vars_found: list[str] = []
    for arm_vars, _s, _e in _extract_match_arm_pattern_spans(match_body):
        vars_found.extend(arm_vars)
    return vars_found


def _split_fn_sig_params_and_ret(sig: str) -> tuple[str, str]:
    m_fn = re.search(r"\bfn\s+[A-Za-z_][A-Za-z0-9_]*\s*", sig)
    if not m_fn:
        return sig, sig
    pos = m_fn.end()
    n = len(sig)
    if pos < n and sig[pos] == "<":
        after_lt = _skip_balanced_angle_forward(sig, pos)
        if after_lt is None:
            return sig, sig
        pos = after_lt
        while pos < n and sig[pos].isspace():
            pos += 1
    if pos < n and sig[pos] == "(":
        close_p = _find_matching_close_forward(sig, pos, "(", ")")
        if close_p is not None:
            return sig[pos + 1 : close_p], sig[close_p + 1 :]
    return sig, sig


def _split_rust_expr_chain(s: str, end_pos: int) -> list[str]:
    segments: list[str] = []
    cur = end_pos - 1
    while cur >= 0:
        while cur >= 0:
            while cur >= 0 and s[cur].isspace():
                cur -= 1
            if cur >= 0 and s[cur] == "?":
                cur -= 1
                continue
            if (
                cur >= 4
                and s[cur - 4 : cur + 1] == "await"
                and (cur == 4 or not (s[cur - 5].isalnum() or s[cur - 5] == "_"))
            ):
                k = cur - 5
                while k >= 0 and s[k].isspace():
                    k -= 1
                if k >= 0 and s[k] == "." and not (k > 0 and s[k - 1] == "."):
                    cur = k - 1
                    continue
            break
        if cur < 0:
            break
        seg_end = cur + 1
        if s[cur] == ")":
            open_p = _find_matching_open_backward(s, cur, "(", ")")
            if open_p is None:
                break
            cur = open_p - 1
            while cur >= 0 and s[cur].isspace():
                cur -= 1
            if cur >= 0 and s[cur] == ">":
                open_a = _find_matching_angle_backward(s, cur)
                if open_a is not None:
                    k = open_a - 1
                    while k >= 0 and s[k].isspace():
                        k -= 1
                    if k >= 1 and s[k - 1 : k + 1] == "::":
                        cur = k - 2
                        while cur >= 0 and s[cur].isspace():
                            cur -= 1
            p_start = _scan_rust_path_backward(s, cur)
            if p_start is None:
                # Parenthesized root expression `(expr)`
                segments.append(s[open_p:seg_end])
                break
            segments.append(s[p_start:seg_end])
            cur = p_start - 1
        elif s[cur] == "]":
            open_b = _find_matching_open_backward(s, cur, "[", "]")
            if open_b is None:
                break
            cur = open_b - 1
            while cur >= 0 and s[cur].isspace():
                cur -= 1
            p_start = _scan_rust_path_backward(s, cur)
            if p_start is None:
                break
            segments.append(s[p_start:seg_end])
            cur = p_start - 1
        elif s[cur] == "}":
            open_c = _find_matching_open_backward(s, cur, "{", "}")
            if open_c is None:
                break
            k = open_c - 1
            while k >= 0 and s[k].isspace():
                k -= 1
            p_start = _scan_rust_path_backward(s, k)
            if p_start is None:
                break
            path_str = s[p_start : k + 1]
            last_ident = path_str.rsplit("::", 1)[-1].strip()
            if not (last_ident and last_ident[0].isupper()):
                break
            segments.append(s[p_start:seg_end])
            break
        else:
            p_start = _scan_rust_path_backward(s, cur)
            if p_start is None:
                break
            segments.append(s[p_start:seg_end])
            cur = p_start - 1
        while cur >= 0 and s[cur].isspace():
            cur -= 1
        if cur >= 0 and s[cur] == ".":
            if cur > 0 and s[cur - 1] == ".":
                break
            cur -= 1
            continue
        break
    segments.reverse()
    return segments


def _resolve_rust_expr_chain_types(
    segments: list[str],
    cur_scope: str,
    fn_ret_types: dict[tuple[str, str], str],
    fn_entries: dict[tuple[str, str], Any],
    var_types: dict[str, set[str]],
    _depth: int = 0,
) -> set[str]:
    if not segments or _depth > 8:
        return set()
    root = segments[0].strip()
    cand: set[str] = set()
    if root == "self":
        if cur_scope:
            cand.add(cur_scope)
    elif re.fullmatch(r"[a-z_][A-Za-z0-9_]*", root):
        cand.update(var_types.get(root, ()))
    elif re.fullmatch(r"[A-Z][A-Za-z0-9_]*", root):
        cand.add(cur_scope if root == "Self" and cur_scope else root)
    elif root.endswith("}"):
        open_c = _find_matching_open_backward(root, len(root) - 1, "{", "}")
        if open_c is not None:
            prefix = _strip_rust_turbofish_from_path(root[:open_c])
            m_st = re.fullmatch(r"(?:[a-z_][A-Za-z0-9_]*::)*([A-Z][A-Za-z0-9_]*)", prefix)
            if m_st:
                ty = cur_scope if m_st.group(1) == "Self" and cur_scope else m_st.group(1)
                if ty and ty != "Self":
                    cand.add(ty)
    elif root.endswith(")"):
        open_p = _find_matching_open_backward(root, len(root) - 1, "(", ")")
        if open_p is not None:
            prefix = root[:open_p].rstrip()
            if not prefix:
                inner = root[open_p + 1 : len(root) - 1].strip()
                inner_segs = _split_rust_expr_chain(inner, len(inner))
                cand.update(
                    _resolve_rust_expr_chain_types(
                        inner_segs, cur_scope, fn_ret_types, fn_entries, var_types, _depth + 1
                    )
                )
            else:
                prefix = _strip_rust_turbofish_from_path(prefix)
                m_assoc = re.fullmatch(
                    r"(?:[a-z_][A-Za-z0-9_]*::)*([A-Z][A-Za-z0-9_]*)::([A-Za-z_][A-Za-z0-9_]*)",
                    prefix,
                )
                if m_assoc:
                    ty_raw, fn_name = m_assoc.group(1), m_assoc.group(2)
                    ty = cur_scope if ty_raw == "Self" and cur_scope else ty_raw
                    if ty and ty != "Self":
                        ret = fn_ret_types.get((ty, fn_name))
                        if ret:
                            cand.add(ret)
                        elif (ty, fn_name) not in fn_entries:
                            cand.add(ty)
                else:
                    m_free = re.fullmatch(r"(?:(?:crate|self)::)?([a-z_][A-Za-z0-9_]*)", prefix)
                    if m_free:
                        ret = fn_ret_types.get(("", m_free.group(1)))
                        if ret:
                            cand.add(ret)

    for seg in segments[1:]:
        if not cand:
            return set()
        seg_s = seg.strip()
        if not seg_s.endswith(")"):
            return set()
        open_p = _find_matching_open_backward(seg_s, len(seg_s) - 1, "(", ")")
        if open_p is None:
            return set()
        m_prefix = seg_s[:open_p].rstrip()
        if m_prefix.endswith(">"):
            open_a = _find_matching_angle_backward(m_prefix, len(m_prefix) - 1)
            if open_a is not None and m_prefix[:open_a].rstrip().endswith("::"):
                m_prefix = m_prefix[:open_a].rstrip()[:-2].strip()
        if not re.fullmatch(r"[A-Za-z_][A-Za-z0-9_]*", m_prefix):
            return set()
        next_cand: set[str] = set()
        for ty in cand:
            ret = fn_ret_types.get((ty, m_prefix))
            if ret:
                next_cand.add(ret)
            elif m_prefix in _PASSTHROUGH_RUST_METHODS and (ty, m_prefix) not in fn_entries:
                next_cand.add(ty)
        cand = next_cand
    return cand


def _test_w4_display_list_and_separations(ctx: RunContext) -> TestOutcome:
    dl_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-io/src/display_list.rs"))
    if not dl_code or not re.search(r"\bExportDisplayList\b", dl_code):
        return TestOutcome("FAIL", "W4 REQ-04: ExportDisplayList (crates/coree-io/src/display_list.rs) not yet implemented")
    for rel_mod, entrypoints in (
        ("crates/coree-io/src/pdf.rs", ("export_pdf_to_bytes", "export_pdf")),
        ("crates/coree-io/src/eps.rs", ("export_eps_to_string", "export_eps")),
        ("crates/coree-io/src/svg.rs", ("export_svg_to_string", "export_svg")),
    ):
        mod_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, rel_mod))
        mod_no_use = re.sub(r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?use\b[^;]*;", "", mod_code)
        if not mod_no_use or not re.search(r"\b(?:ExportDisplayList|display_list)\b", mod_no_use):
            return TestOutcome(
                "FAIL",
                f"W4 REQ-04: {rel_mod} does not use ExportDisplayList / display_list outside import lines and #[cfg(test)]",
            )
        impl_spans = _extract_impl_spans(mod_no_use)
        fn_blocks = _extract_all_brace_blocks_with_spans(mod_no_use, r"\bfn\s+(\w+)\b")
        fn_entries: dict[tuple[str, str], list[tuple[str, str]]] = {}
        fn_ret_types: dict[tuple[str, str], str] = {}
        for m_fn, fn_bs, _fn_be, body in fn_blocks:
            scope = ""
            best_span_len: Optional[int] = None
            for ibs, ibe, ty_name in impl_spans:
                if ibs < m_fn.start() < ibe:
                    slen = ibe - ibs
                    if best_span_len is None or slen < best_span_len:
                        best_span_len = slen
                        scope = ty_name
            sig = mod_no_use[m_fn.start():fn_bs]
            params_inner, ret_tail = _split_fn_sig_params_and_ret(sig)
            fn_entries.setdefault((scope, m_fn.group(1)), []).append((params_inner, body))
            m_ret = re.search(
                r"->\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?(?:(?:[A-Za-z0-9_]+::)*(?:Result|Option)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?(?:(?:[A-Za-z0-9_]+::)*(?:Box|Arc|Rc)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?(?:[A-Za-z0-9_]+::)*([A-Z][A-Za-z0-9_]*)",
                ret_tail,
            )
            if m_ret and (scope, m_fn.group(1)) not in fn_ret_types:
                ret_ty = m_ret.group(1)
                if ret_ty == "Self" and scope:
                    ret_ty = scope
                fn_ret_types[(scope, m_fn.group(1))] = ret_ty
        present_eps = [ep for ep in entrypoints if ("", ep) in fn_entries]
        if not present_eps:
            return TestOutcome(
                "FAIL",
                f"W4 REQ-04: {rel_mod} missing export entrypoint(s) {entrypoints}",
            )
        # BFS over scoped non-test helper/method call targets reachable from the export entrypoints
        visited_fns: set[tuple[str, str]] = set()
        queue: list[tuple[str, str]] = [("", ep) for ep in present_eps]
        used_from_entrypoint = False
        while queue and not used_from_entrypoint:
            cur_key = queue.pop(0)
            if cur_key in visited_fns:
                continue
            visited_fns.add(cur_key)
            cur_scope, _cur_fn = cur_key
            for params_inner, body in fn_entries.get(cur_key, ()):
                if _is_trivial_stub_body(body):
                    continue
                if re.search(r"\b(?:ExportDisplayList|display_list)\b", body):
                    used_from_entrypoint = True
                    break
                var_types: dict[str, set[str]] = {}
                for pm in re.finditer(
                    r"\b([a-z_][A-Za-z0-9_]*)\s*:\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?(?:(?:[A-Za-z0-9_]+::)*(?:Result|Option)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?(?:(?:[A-Za-z0-9_]+::)*(?:Box|Arc|Rc)\s*<\s*(?:&(?:'[A-Za-z_][A-Za-z0-9_]*\s+)?(?:mut\s+)?)?)?(?:[A-Za-z0-9_]+::)*([A-Z][A-Za-z0-9_]*)",
                    params_inner,
                ):
                    p_ty = cur_scope if pm.group(2) == "Self" and cur_scope else pm.group(2)
                    var_types.setdefault(pm.group(1), set()).add(p_ty)

                b_n = len(body)
                events: list[tuple[int, int, str, Any]] = []
                for idx in range(1, max(1, b_n - 1)):
                    ch = body[idx]
                    if ch == "{":
                        events.append((idx, 0, "push_scope", None))
                    elif ch == "}":
                        events.append((idx, 9, "pop_scope", None))

                for lm in re.finditer(
                    r"\blet\s+(?:ref\s+)?(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*(?::\s*([^=;{}]+?))?\s*=",
                    body,
                ):
                    if lm.end() < b_n and body[lm.end()] == "=":
                        continue
                    i_pos = lm.end()
                    p_d = 0
                    br_d = 0
                    b_d = 0
                    semi_pos = -1
                    while i_pos < b_n:
                        c = body[i_pos]
                        if c == "(":
                            p_d += 1
                        elif c == ")":
                            p_d -= 1
                            if p_d < 0:
                                break
                        elif c == "[":
                            br_d += 1
                        elif c == "]":
                            br_d -= 1
                            if br_d < 0:
                                break
                        elif c == "{":
                            b_d += 1
                        elif c == "}":
                            b_d -= 1
                            if b_d < 0:
                                break
                        elif c == ";" and p_d == 0 and br_d == 0 and b_d == 0:
                            semi_pos = i_pos
                            break
                        i_pos += 1
                    if semi_pos > lm.end():
                        rhs_expr = body[lm.end() : semi_pos].strip()
                        events.append((semi_pos + 1, 1, "let", (lm.group(1), lm.group(2), rhs_expr)))

                for lem in re.finditer(
                    r"\blet\s+(?:[A-Za-z0-9_]+::)*(?:Ok|Some)\s*\(\s*(?:ref\s+)?(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*\)\s*=\s*([^;{\n]+?)\s+else\b",
                    body,
                ):
                    e_pos = lem.end()
                    while e_pos < b_n and body[e_pos].isspace():
                        e_pos += 1
                    if e_pos < b_n and body[e_pos] == "{":
                        else_be = _find_matching_close_forward(body, e_pos, "{", "}")
                        if else_be is not None:
                            events.append((else_be + 1, 1, "let_else", (lem.group(1), lem.group(2).strip())))

                for ilm in re.finditer(
                    r"\b(?:if|while)\s+let\s+(?:[A-Za-z0-9_]+::)*(?:Some|Ok)\s*\(\s*(?:ref\s+)?(?:mut\s+)?([a-z_][A-Za-z0-9_]*)\s*\)\s*=",
                    body,
                ):
                    vname = ilm.group(1)
                    i_pos = ilm.end()
                    p_d = 0
                    br_d = 0
                    scrut_end = -1
                    while i_pos < b_n:
                        c = body[i_pos]
                        if c == "(":
                            p_d += 1
                        elif c == ")":
                            p_d -= 1
                            if p_d < 0:
                                break
                        elif c == "[":
                            br_d += 1
                        elif c == "]":
                            br_d -= 1
                            if br_d < 0:
                                break
                        elif c == ";" and p_d == 0 and br_d == 0:
                            break
                        elif c == "{" and p_d == 0 and br_d == 0:
                            scrut_end = i_pos
                            break
                        i_pos += 1
                    if scrut_end > ilm.end():
                        scrut_expr = body[ilm.end() : scrut_end].strip()
                        if scrut_expr:
                            events.append((scrut_end, 1, "if_let", (vname, scrut_expr)))

                for mm in re.finditer(r"\bmatch\b", body):
                    if mm.start() >= 2 and body[mm.start() - 2 : mm.start()] == "r#":
                        continue
                    i_pos = mm.end()
                    p_d = 0
                    br_d = 0
                    m_bs = -1
                    while i_pos < b_n:
                        c = body[i_pos]
                        if c == "(":
                            p_d += 1
                        elif c == ")":
                            p_d -= 1
                            if p_d < 0:
                                break
                        elif c == "[":
                            br_d += 1
                        elif c == "]":
                            br_d -= 1
                            if br_d < 0:
                                break
                        elif c == ";" and p_d == 0 and br_d == 0:
                            break
                        elif c == "{" and p_d == 0 and br_d == 0:
                            m_bs = i_pos
                            break
                        i_pos += 1
                    if m_bs > mm.end():
                        m_be = _find_matching_close_forward(body, m_bs, "{", "}")
                        if m_be is not None:
                            scrut_expr = body[mm.end() : m_bs].strip()
                            arm_spans = _extract_match_arm_pattern_spans(body[m_bs + 1 : m_be])
                            if scrut_expr and arm_spans:
                                events.append((mm.start(), 2, "match_eval", (mm.start(), scrut_expr)))
                                for arm_vars, rel_s, rel_e in arm_spans:
                                    events.append((m_bs + 1 + rel_s, 2, "match_arm_push", (mm.start(), arm_vars)))
                                    events.append((m_bs + 1 + rel_e, 8, "match_arm_pop", None))

                for m_id in re.finditer(r"\b([A-Za-z_][A-Za-z0-9_]*)\b", body):
                    events.append((m_id.start(), 5, "call", m_id))

                events.sort(key=lambda ev: (ev[0], ev[1]))
                scope_stack: list[dict[str, set[str]]] = []
                match_scrut_types: dict[int, set[str]] = {}
                for _pos, _prio, kind, payload in events:
                    if kind == "push_scope":
                        scope_stack.append({k: set(v) for k, v in var_types.items()})
                    elif kind == "pop_scope":
                        if scope_stack:
                            var_types = scope_stack.pop()
                    elif kind == "let":
                        vname, ty_ann, rhs_expr = payload
                        new_tys: set[str] = set()
                        if ty_ann:
                            m_ty = _RUST_PRIMARY_TYPE_RX.match(ty_ann)
                            if m_ty:
                                t = m_ty.group(1)
                                p_ty = cur_scope if t == "Self" and cur_scope else t
                                if p_ty and p_ty != "Self":
                                    new_tys.add(p_ty)
                        else:
                            rhs_segs = _split_rust_expr_chain(rhs_expr, len(rhs_expr))
                            new_tys = _resolve_rust_expr_chain_types(
                                rhs_segs, cur_scope, fn_ret_types, fn_entries, var_types
                            )
                        if new_tys:
                            var_types[vname] = set(new_tys)
                        else:
                            var_types.pop(vname, None)
                    elif kind == "let_else":
                        vname, scrut_expr = payload
                        scrut_segs = _split_rust_expr_chain(scrut_expr, len(scrut_expr))
                        scrut_tys = _resolve_rust_expr_chain_types(
                            scrut_segs, cur_scope, fn_ret_types, fn_entries, var_types
                        )
                        if scrut_tys:
                            var_types[vname] = set(scrut_tys)
                        else:
                            var_types.pop(vname, None)
                    elif kind == "if_let":
                        vname, scrut_expr = payload
                        scrut_segs = _split_rust_expr_chain(scrut_expr, len(scrut_expr))
                        scrut_tys = _resolve_rust_expr_chain_types(
                            scrut_segs, cur_scope, fn_ret_types, fn_entries, var_types
                        )
                        if scrut_tys:
                            var_types[vname] = set(scrut_tys)
                        else:
                            var_types.pop(vname, None)
                    elif kind == "match_eval":
                        m_off, scrut_expr = payload
                        scrut_segs = _split_rust_expr_chain(scrut_expr, len(scrut_expr))
                        match_scrut_types[m_off] = _resolve_rust_expr_chain_types(
                            scrut_segs, cur_scope, fn_ret_types, fn_entries, var_types
                        )
                    elif kind == "match_arm_push":
                        m_off, arm_vars = payload
                        scope_stack.append({k: set(v) for k, v in var_types.items()})
                        scrut_tys = match_scrut_types.get(m_off, set())
                        for av in arm_vars:
                            if scrut_tys:
                                var_types[av] = set(scrut_tys)
                            else:
                                var_types.pop(av, None)
                    elif kind == "match_arm_pop":
                        if scope_stack:
                            var_types = scope_stack.pop()
                    elif kind == "call":
                        m_id = payload
                        ident = m_id.group(1)
                        if ident in _RUST_NON_CALL_KEYWORDS:
                            continue
                        pos = m_id.end()
                        while pos < b_n and body[pos].isspace():
                            pos += 1
                        if pos + 1 < b_n and body[pos : pos + 2] == "::":
                            pos += 2
                            while pos < b_n and body[pos].isspace():
                                pos += 1
                            if pos < b_n and body[pos] == "<":
                                after_lt = _skip_balanced_angle_forward(body, pos)
                                if after_lt is None:
                                    continue
                                pos = after_lt
                                while pos < b_n and body[pos].isspace():
                                    pos += 1
                            else:
                                continue
                        if pos >= b_n or body[pos] != "(":
                            continue
                        prev = m_id.start() - 1
                        while prev >= 0 and body[prev].isspace():
                            prev -= 1
                        if prev >= 0 and body[prev] == "." and not (prev > 0 and body[prev - 1] == "."):
                            if ident == "self":
                                continue
                            recv_segs = _split_rust_expr_chain(body, prev)
                            cand_types = _resolve_rust_expr_chain_types(
                                recv_segs, cur_scope, fn_ret_types, fn_entries, var_types
                            )
                            for t_name in cand_types:
                                nxt = (t_name, ident)
                                if nxt in fn_entries and nxt not in visited_fns:
                                    queue.append(nxt)
                        elif prev >= 1 and body[prev - 1 : prev + 1] == "::":
                            p_k = prev - 2
                            while p_k >= 0 and body[p_k].isspace():
                                p_k -= 1
                            if p_k >= 0 and body[p_k] == ">":
                                open_a = _find_matching_angle_backward(body, p_k)
                                if open_a is not None:
                                    k2 = open_a - 1
                                    while k2 >= 0 and body[k2].isspace():
                                        k2 -= 1
                                    if k2 >= 1 and body[k2 - 1 : k2 + 1] == "::":
                                        p_k = k2 - 2
                                        while p_k >= 0 and body[p_k].isspace():
                                            p_k -= 1
                            p_start = _scan_rust_path_backward(body, p_k)
                            if p_start is None:
                                continue
                            prefix_path = _strip_rust_turbofish_from_path(body[p_start : p_k + 1])
                            last_seg = prefix_path.rsplit("::", 1)[-1]
                            if last_seg == "Self" or (last_seg and last_seg[0].isupper()):
                                t_name = cur_scope if last_seg == "Self" else last_seg
                                nxt = (t_name, ident)
                                if t_name and nxt in fn_entries and nxt not in visited_fns:
                                    queue.append(nxt)
                            elif prefix_path in ("crate", "self"):
                                nxt = ("", ident)
                                if nxt in fn_entries and nxt not in visited_fns:
                                    queue.append(nxt)
                        else:
                            nxt = ("", ident)
                            if nxt in fn_entries and nxt not in visited_fns:
                                queue.append(nxt)
        if not used_from_entrypoint:
            return TestOutcome(
                "FAIL",
                f"W4 REQ-04: {rel_mod} export entrypoint(s) {present_eps} (and reachable helpers) do not invoke ExportDisplayList / display_list in a non-stub body",
            )
    prep_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/prepress.rs"))
    for sym in ("PerfectBound", "Trapping", "Halftone"):
        if not re.search(rf"\b{sym}\b", prep_code):
            return TestOutcome("FAIL", f"W4 REQ-05: prepress.rs missing {sym}")
    vec_sep_fn_rx = r"\bfn\s+(\w*(?:vector\w*(?:separat|plate)|(?:separat|plate)\w*vector)\w*)\b"
    vec_sep_blocks = _extract_all_brace_blocks(prep_code, vec_sep_fn_rx)
    if not vec_sep_blocks or any(_is_trivial_stub_body(body) for _m, body in vec_sep_blocks):
        return TestOutcome(
            "FAIL",
            "W4 REQ-05: prepress.rs missing genuine non-stub vector separation implementation",
        )
    rust_4096_rx = (
        r"(?<![0-9A-Za-z_])"
        r"(?:"
        r"4_?096(?:_[A-Za-z0-9_]+|[ui](?:8|16|32|64|128|size)|f(?:32|64)|\.0+)?"
        r"|0[xX](?:0+_?)*1_?0_?0_?0(?:_[ui](?:8|16|32|64|128|size)|[ui](?:8|16|32|64|128|size))?"
        r")"
        r"(?![0-9A-Za-z_])"
    )
    const_4096_names = set(
        re.findall(
            rf"\b(?:const|static)\s+([A-Za-z_][A-Za-z0-9_]*)\b[^=;]*=\s*[^;]*{rust_4096_rx}[^;]*;",
            prep_code,
        )
    )
    all_sep_blocks = _extract_all_brace_blocks(prep_code, r"\bfn\s+(\w*(?:separat|plate)\w*)\b")
    for m_fn, body in vec_sep_blocks + all_sep_blocks:
        fn_name = m_fn.group(1)
        if fn_name == "compute_separations_with_text_outliner":
            continue
        if re.search(rust_4096_rx, body) or any(
            re.search(rf"\b{re.escape(cname)}\b", body) for cname in const_4096_names
        ):
            return TestOutcome(
                "FAIL",
                f"W4 REQ-05: prepress.rs separation function {fn_name} must not enforce a 4096 raster cap",
            )
    fail_io = _run_cargo_tests(ctx, ["-p", "coree-io", "--", "display_list"], "W4 REQ-04")
    if fail_io is not None:
        return fail_io
    for filt, label in (
        ("separat", "W4 REQ-05 Vector Separations"),
        ("perfect_bound", "W4 REQ-05 PerfectBound"),
        ("trapping", "W4 REQ-05 Trapping"),
        ("halftone", "W4 REQ-05 Halftone"),
    ):
        fail_pr = _run_cargo_tests(ctx, ["-p", "coree-prod", "--", filt], label, min_passed=1)
        if fail_pr is not None:
            return fail_pr
    return TestOutcome("PASS", "ExportDisplayList + Vector Separations + Trapping + Halftone + PerfectBound cargo tests verified")


def _test_w4_dwg_emf_acb_psd_export(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-FORMATS")
    for probe_name, dissector, fmt_tok in (
        ("probe_w4.dwg", dissect_dwg, "dwg"),
        ("probe_w4.emf", dissect_emf, "emf"),
        ("probe_w4.acb", dissect_acb, "acb"),
    ):
        p = _f(ctx, probe_name)
        dissector(p.read_bytes())
        r = run_cmd([str(ctx.cli_bin), "inspect", str(p), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        if r.returncode != 0:
            return TestOutcome("FAIL", f"W4 ADD-06: coree-cli inspect rejected {probe_name}: {r.stderr.strip()}")
        try:
            rep = _require_dict(json.loads(r.stdout), f"inspect {probe_name}")
        except _CLI_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W4 ADD-06: coree-cli inspect {probe_name} returned invalid JSON: {exc}")
        det = json.dumps(rep.get("detected_format", "")).lower()
        if fmt_tok not in det:
            return TestOutcome("FAIL", f"W4 ADD-06: inspect {probe_name} detected_format={det!r}, expected {fmt_tok!r}")

    src = _f(ctx, "fixture.coree")

    out_emf = work / "exported.emf"
    r_emf = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_emf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_emf.returncode != 0 or not out_emf.is_file():
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli convert -> .emf export failed: {r_emf.stderr.strip()}")
    try:
        emf_info = dissect_emf(out_emf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported EMF failed independent binary dissector: {exc}")
    if emf_info["records"] < 3:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported EMF has no drawing records: {emf_info}")

    out_wmf = work / "exported.wmf"
    r_wmf = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_wmf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_wmf.returncode != 0 or not out_wmf.is_file():
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli convert -> .wmf export failed: {r_wmf.stderr.strip()}")
    try:
        wmf_info = dissect_wmf(out_wmf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported WMF failed independent binary dissector: {exc}")
    if wmf_info["records"] < 3 or not wmf_info["has_eof"]:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported WMF lacks drawing records or META_EOF: {wmf_info}")

    out_dwg = work / "exported.dwg"
    r_dwg = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_dwg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dwg.returncode != 0 or not out_dwg.is_file():
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli convert -> .dwg export failed: {r_dwg.stderr.strip()}")
    dwg_bytes = out_dwg.read_bytes()
    try:
        dwg_info = dissect_dwg(dwg_bytes)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported DWG failed independent binary dissector: {exc}")
    dwg_ins = run_cmd([str(ctx.cli_bin), "inspect", str(out_dwg), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if dwg_ins.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli inspect rejected exported DWG: {dwg_ins.stderr.strip()}")
    try:
        dwg_rep = _require_dict(json.loads(dwg_ins.stdout), "inspect exported DWG")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli inspect exported DWG returned invalid JSON: {exc}")
    dwg_oc = dwg_rep.get("object_count")
    if (
        "dwg" not in json.dumps(dwg_rep.get("detected_format", "")).lower()
        or not isinstance(dwg_oc, int)
        or isinstance(dwg_oc, bool)
        or dwg_oc < 1
    ):
        return TestOutcome("FAIL", f"W4 ADD-06: Exported DWG round-trip inspect failed: {dwg_rep}")

    out_psd = work / "exported.psd"
    r_psd = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_psd)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_psd.returncode != 0 or not out_psd.is_file():
        return TestOutcome("FAIL", f"W4 ADD-06: coree-cli convert -> .psd export failed: {r_psd.stderr.strip()}")
    try:
        psd = dissect_psd(out_psd.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-06: Exported PSD failed independent binary dissector: {exc}")
    if psd["width"] <= 0 or psd["height"] <= 0 or psd["channels"] < 3 or psd["depth"] not in (8, 16):
        return TestOutcome("FAIL", f"W4 ADD-06: Invalid exported PSD dimensions/channels/depth: {psd}")
    return TestOutcome(
        "PASS",
        f".dwg/.emf/.acb inspect + .emf ({emf_info['records']} recs) + .wmf ({wmf_info['file_size_words']}w) + .dwg ({dwg_info['magic']}, {dwg_info['byte_len']}B) + .psd ({psd['width']}x{psd['height']} ch={psd['channels']} {psd['depth']}b) verified",
    )


def _test_w4_fefco_2d_barcodes_and_kerf(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-FEFCO-KERF")
    prod_src_dir = ctx.repo_root / "crates" / "coree-prod" / "src"
    prod_stripped = "\n".join(
        _strip_cfg_test_modules(
            strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace"))
        )
        for p in sorted(prod_src_dir.rglob("*.rs"))
    )
    prod_comments = "\n".join(
        strip_rust_comments_only(p.read_text(encoding="utf-8", errors="replace"))
        for p in sorted(prod_src_dir.rglob("*.rs"))
    )
    if not re.search(r"\bfefco\b", prod_stripped, re.I):
        return TestOutcome("FAIL", "W4 ADD-05: FEFCO packaging templates missing in crates/coree-prod/src/")
    fefco_enum_counts = [
        _count_enum_variants(blk)
        for rx in (r"\bpub\s+enum\s+DielineKind\s*\{", r"\bpub\s+enum\s+Fefco[A-Za-z0-9_]*\s*\{", r"\benum\s+Fefco[A-Za-z0-9_]*\s*\{")
        for blk in [_extract_brace_block(prod_stripped, rx)]
        if blk
    ]
    fefco_ident_codes = set(re.findall(r"\b(?:Fefco_?|F)(0\d{3})\b", prod_stripped))
    fefco_str_codes = set(re.findall(r'"(?:FEFCO[\s_-]*)?(0\d{3})"', prod_comments, re.I))
    fefco_count = max(fefco_enum_counts + [len(fefco_ident_codes), len(fefco_str_codes), 0])
    if fefco_count < 30:
        return TestOutcome("FAIL", f"W4 ADD-05: expected >= 30 FEFCO packaging templates, found {fefco_count}")

    cp_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/cut_packaging.rs"))
    gen_dieline_body = _extract_brace_block(cp_code, r"\bfn\s+generate_dieline\b")
    if not gen_dieline_body or _is_trivial_stub_body(gen_dieline_body):
        return TestOutcome("FAIL", "W4 ADD-05: generate_dieline in cut_packaging.rs missing or trivial stub")
    if _has_empty_match_arm(prod_stripped, r"(?:DielineKind|Fefco[A-Za-z0-9_]*)::(?:Fefco_?|F)?0\d{3}"):
        return TestOutcome("FAIL", "W4 ADD-05: FEFCO match arm in crates/coree-prod/src/ is an empty no-op stub")
    fefco_fn_blocks = _extract_all_brace_blocks(prod_stripped, r"\bfn\s+(\w+)\b")
    has_non_stub_fefco_fn = any(
        not _is_trivial_stub_body(body) and re.search(r"\bfefco\b", m.group(1) + " " + body, re.I)
        for m, body in fefco_fn_blocks
    )
    if not has_non_stub_fefco_fn:
        return TestOutcome("FAIL", "W4 ADD-05: no non-stub FEFCO dieline builder/generator function found")

    bc_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/barcode.rs"))
    bc_fn_blocks = _extract_all_brace_blocks(bc_code, r"\bfn\s+(\w+)\b")
    gen_bc_body = _extract_brace_block(bc_code, r"\bfn\s+generate_barcode\b")
    if not gen_bc_body or _is_trivial_stub_body(gen_bc_body):
        return TestOutcome("FAIL", "W4 ADD-05: generate_barcode in barcode.rs missing or trivial stub")
    for sym in ("DataMatrix", "Pdf417", "Aztec", "Gs1128"):
        if not re.search(rf"\b{sym}\b", bc_code):
            return TestOutcome("FAIL", f"W4 ADD-05: barcode.rs missing 2D/GS1 symbology {sym}")
        sym_rx = rf"(?:Barcode[A-Za-z0-9_]*::)?{sym}"
        if _has_empty_match_arm(bc_code, sym_rx):
            return TestOutcome("FAIL", f"W4 ADD-05: {sym} match arm in barcode.rs is an empty no-op stub")
        sym_arms = _extract_match_arm_bodies(bc_code, sym_rx)
        has_real_sym_fn = any(
            not _is_trivial_stub_body(body) and re.search(rf"\b{sym}\b", body)
            for _m, body in bc_fn_blocks
        )
        if not sym_arms or not any(not _is_trivial_stub_body(arm) for arm in sym_arms) or not has_real_sym_fn:
            return TestOutcome(
                "FAIL",
                f"W4 ADD-05: {sym} in barcode.rs lacks non-stub encoding match arm / implementation",
            )

    lp_code = _strip_cfg_test_modules(_read_rust_stripped(ctx, "crates/coree-prod/src/laser_plugin.rs"))
    if not re.search(r"\b(?:i_overlay|self_intersection|boolean_offset)\b", lp_code, re.I):
        return TestOutcome("FAIL", "W4 REM-09: laser_plugin.rs lacks Boolean self-intersection cleanup for Kerf")
    lp_fn_blocks = _extract_all_brace_blocks(lp_code, r"\bfn\s+(\w+)\b")
    kerf_fn_ok = any(
        not _is_trivial_stub_body(body)
        and re.search(r"\b(?:i_overlay|Overlay|self_intersection|boolean_offset)\b", body, re.I)
        for m, body in lp_fn_blocks
        if re.search(r"kerf|boolean_offset|self_intersection", m.group(1), re.I)
    )
    if not kerf_fn_ok:
        return TestOutcome(
            "FAIL",
            "W4 REM-09: laser_plugin.rs kerf/boolean_offset function is empty/stub or lacks non-trivial Boolean cleanup",
        )

    # Execute coree-cli laser --kerf on an isolated acute 5-point star and verify outward expansion + zero inward self-intersections
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    star_obj = dict(doc["objects"]["3"])
    star_obj["id"] = 3
    star_obj["name"] = "Acute Star"
    star_obj["payload"] = {
        "Shape": {"Star": {"points": 5, "outer_radius": 20.0, "inner_ratio": 0.25, "complex": False}}
    }
    star_obj["fill"] = "None"
    star_obj["outline"] = {
        **star_obj["outline"],
        "color": {"Rgba": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}},
        "width_mm": 0.2,
        "is_hairline": True,
    }
    doc["objects"] = {"3": star_obj}
    doc["pages"][0]["layers"][0]["objects"] = [3]
    doc["pages"][0]["layers"][1]["objects"] = []
    doc["next_id"] = 4
    star_in = work / "acute_star.coree"
    _write_coree_from_doc(star_in, "Acute Kerf Star", doc)

    def _extract_cut_polys(svg_bytes: bytes) -> list[list[tuple[float, float]]]:
        root = ET.fromstring(svg_bytes.decode("utf-8"))
        polys: list[list[tuple[float, float]]] = []
        for g in root.iter():
            if g.tag.split("}", 1)[-1] == "g" and g.attrib.get("id") == "CUT":
                for p in g.iter():
                    if p.tag.split("}", 1)[-1] == "path":
                        d_attr = p.attrib.get("d", "")
                        polys.extend(_parse_svg_path_subpaths(d_attr))
        return polys

    def _cross(a: tuple[float, float], b: tuple[float, float], c: tuple[float, float]) -> float:
        return (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])

    def _pt_seg_dist(p: tuple[float, float], a: tuple[float, float], b: tuple[float, float]) -> float:
        dx, dy = b[0] - a[0], b[1] - a[1]
        len_sq = dx * dx + dy * dy
        if len_sq <= 1e-12:
            return math.hypot(p[0] - a[0], p[1] - a[1])
        t = max(0.0, min(1.0, ((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len_sq))
        return math.hypot(p[0] - (a[0] + t * dx), p[1] - (a[1] + t * dy))

    def _segments_intersect_or_overlap(
        p1: tuple[float, float],
        p2: tuple[float, float],
        p3: tuple[float, float],
        p4: tuple[float, float],
    ) -> bool:
        len12 = math.hypot(p2[0] - p1[0], p2[1] - p1[1])
        len34 = math.hypot(p4[0] - p3[0], p4[1] - p3[1])
        if len12 > 1e-12 and len34 > 1e-12:
            s1 = _cross(p3, p4, p1) / len34
            s2 = _cross(p3, p4, p2) / len34
            s3 = _cross(p1, p2, p3) / len12
            s4 = _cross(p1, p2, p4) / len12
            tol = 1e-7
            if ((s1 > tol and s2 < -tol) or (s1 < -tol and s2 > tol)) and (
                (s3 > tol and s4 < -tol) or (s3 < -tol and s4 > tol)
            ):
                return True
        if len12 > 1e-9:
            ux, uy = (p2[0] - p1[0]) / len12, (p2[1] - p1[1]) / len12
            dist3 = abs((p3[0] - p1[0]) * uy - (p3[1] - p1[1]) * ux)
            dist4 = abs((p4[0] - p1[0]) * uy - (p4[1] - p1[1]) * ux)
            if dist3 < 1e-4 and dist4 < 1e-4:
                t3 = (p3[0] - p1[0]) * ux + (p3[1] - p1[1]) * uy
                t4 = (p4[0] - p1[0]) * ux + (p4[1] - p1[1]) * uy
                lo, hi = min(t3, t4), max(t3, t4)
                if min(len12, hi) - max(0.0, lo) > 1e-4:
                    return True
        return False

    def _count_self_intersections(pts: list[tuple[float, float]]) -> int:
        if len(pts) >= 2 and abs(pts[0][0] - pts[-1][0]) < 1e-6 and abs(pts[0][1] - pts[-1][1]) < 1e-6:
            pts = pts[:-1]
        n = len(pts)
        if n < 4:
            return 0
        hits = 0
        # 1. Duplicate/touching non-adjacent vertices
        for i in range(n):
            for j in range(i + 2, n):
                if i == 0 and j == n - 1:
                    continue
                if math.hypot(pts[i][0] - pts[j][0], pts[i][1] - pts[j][1]) < 1e-4:
                    hits += 1
        # 2. Adjacent collinear reversing edges (foldback spikes)
        for i in range(n):
            a, b, c = pts[i], pts[(i + 1) % n], pts[(i + 2) % n]
            ux, uy = b[0] - a[0], b[1] - a[1]
            vx, vy = c[0] - b[0], c[1] - b[1]
            lu, lv = math.hypot(ux, uy), math.hypot(vx, vy)
            if lu > 1e-9 and lv > 1e-9:
                sin_ang = abs(ux * vy - uy * vx) / (lu * lv)
                dot_uv = ux * vx + uy * vy
                if sin_ang < 1e-4 and dot_uv < 0.0:
                    hits += 1
        # 3. Point-on-non-adjacent-segment touches (dist < 1e-4)
        for i in range(n):
            a, b = pts[i], pts[(i + 1) % n]
            i_next = (i + 1) % n
            for k in range(n):
                if k == i or k == i_next:
                    continue
                if _pt_seg_dist(pts[k], a, b) < 1e-4:
                    hits += 1
        # 4. Proper segment crossings (normalized by edge length) + non-adjacent collinear overlapping edges
        for i in range(n):
            p1, p2 = pts[i], pts[(i + 1) % n]
            for j in range(i + 2, n):
                if i == 0 and j == n - 1:
                    continue
                p3, p4 = pts[j], pts[(j + 1) % n]
                if _segments_intersect_or_overlap(p1, p2, p3, p4):
                    hits += 1
        return hits

    def _count_cross_subpath_intersections(
        poly_a: list[tuple[float, float]],
        poly_b: list[tuple[float, float]],
    ) -> int:
        if len(poly_a) >= 2 and abs(poly_a[0][0] - poly_a[-1][0]) < 1e-6 and abs(poly_a[0][1] - poly_a[-1][1]) < 1e-6:
            poly_a = poly_a[:-1]
        if len(poly_b) >= 2 and abs(poly_b[0][0] - poly_b[-1][0]) < 1e-6 and abs(poly_b[0][1] - poly_b[-1][1]) < 1e-6:
            poly_b = poly_b[:-1]
        na, nb = len(poly_a), len(poly_b)
        if na < 2 or nb < 2:
            return 0
        limit_a = na if na >= 3 else 1
        limit_b = nb if nb >= 3 else 1
        hits = 0
        for i in range(limit_a):
            p1, p2 = poly_a[i], poly_a[(i + 1) % na]
            for j in range(limit_b):
                p3, p4 = poly_b[j], poly_b[(j + 1) % nb]
                if (
                    _pt_seg_dist(p1, p3, p4) < 1e-4
                    or _pt_seg_dist(p2, p3, p4) < 1e-4
                    or _pt_seg_dist(p3, p1, p2) < 1e-4
                    or _pt_seg_dist(p4, p1, p2) < 1e-4
                    or _segments_intersect_or_overlap(p1, p2, p3, p4)
                ):
                    hits += 1
        return hits

    kerf0_svg = work / "kerf_star_0.svg"
    kerf_pos_svg = work / "kerf_star_pos.svg"
    kerf_neg_svg = work / "kerf_star_neg.svg"
    for k_arg, out_p in (("0.0", kerf0_svg), ("2.0", kerf_pos_svg), ("-6.0", kerf_neg_svg)):
        rk = run_cmd([str(ctx.cli_bin), "laser", str(star_in), str(out_p), "--kerf", k_arg, "--format", "svg"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if rk.returncode != 0 or not out_p.is_file():
            return TestOutcome("FAIL", f"W4 REM-09: laser --kerf {k_arg} failed: {rk.stderr}")

    try:
        polys0 = _extract_cut_polys(kerf0_svg.read_bytes())
        polys_pos = _extract_cut_polys(kerf_pos_svg.read_bytes())
        polys_neg = _extract_cut_polys(kerf_neg_svg.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-09: malformed CUT SVG path data: {exc}")
    if not polys0 or not polys_pos or not polys_neg:
        return TestOutcome("FAIL", f"W4 REM-09: laser --kerf omitted CUT polygons (0={len(polys0)}, pos={len(polys_pos)}, neg={len(polys_neg)})")
    w0 = max(p[0] for pt_list in polys0 for p in pt_list) - min(p[0] for pt_list in polys0 for p in pt_list)
    w_pos = max(p[0] for pt_list in polys_pos for p in pt_list) - min(p[0] for pt_list in polys_pos for p in pt_list)
    w_neg = max(p[0] for pt_list in polys_neg for p in pt_list) - min(p[0] for pt_list in polys_neg for p in pt_list)
    if w_pos <= w0 + 0.5:
        return TestOutcome("FAIL", f"W4 REM-09: outward --kerf 2.0 did not expand star bounds (w0={w0:.2f}, w_pos={w_pos:.2f})")
    if w_neg >= w0 - 1.0:
        return TestOutcome("FAIL", f"W4 REM-09: inward --kerf -6.0 did not shrink star bounds (w0={w0:.2f}, w_neg={w_neg:.2f})")
    neg_si = sum(_count_self_intersections(pl) for pl in polys_neg)
    for idx_a in range(len(polys_neg)):
        for idx_b in range(idx_a + 1, len(polys_neg)):
            neg_si += _count_cross_subpath_intersections(polys_neg[idx_a], polys_neg[idx_b])
    if neg_si > 0:
        return TestOutcome("FAIL", f"W4 REM-09: inward --kerf -6.0 on acute star left {neg_si} uncleaned self-intersections")

    res = _run_cargo(ctx, ["test", "-p", "coree-prod", "--lib"], timeout=120.0)
    if res.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-05/REM-09 cargo test -p coree-prod --lib failed:\n{(res.stdout + res.stderr)[-800:]}")
    passed_tests = re.findall(r"(?m)^test\s+(\S+)\s+\.\.\.\s+ok\s*$", res.stdout)
    failed_counts = [int(f) for _p, f in re.findall(r"(\d+)\s+passed;\s+(\d+)\s+failed", res.stdout)]
    if not passed_tests or any(f > 0 for f in failed_counts):
        return TestOutcome("FAIL", f"W4 ADD-05/REM-09 coree-prod unit test summary invalid: {res.stdout[-500:]}")
    for target_label, target_rx in (
        ("FEFCO", r"fefco"),
        ("DataMatrix", r"data_?matrix"),
        ("PDF417", r"pdf_?417"),
        ("Aztec", r"aztec"),
        ("GS1-128", r"gs1"),
        ("Kerf", r"kerf"),
    ):
        if not any(re.search(target_rx, tname, re.I) for tname in passed_tests):
            return TestOutcome(
                "FAIL",
                f"W4 ADD-05/REM-09: no passing coree-prod unit test matched targeted capability {target_label!r} (/{target_rx}/)",
            )
    return TestOutcome("PASS", f"30+ FEFCO + 2D/GS1 barcodes + Boolean Kerf cleanup (0 self-intersections) + CLI laser --kerf + {len(passed_tests)} targeted coree-prod tests verified")


def _test_w4_rhai_dom_cql_and_plugin_host(ctx: RunContext) -> TestOutcome:
    gate = require_cli_subcommand(ctx, "cql", "W4")
    if gate is not None:
        return gate
    work = ctx.make_scratch_dir("T1-W4-RHAI-CQL")
    cql_help = run_cmd([str(ctx.cli_bin), "cql", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if cql_help.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql --help failed: {cql_help.stderr}")

    src = _f(ctx, "fixture.coree")
    _, orig_doc = inspect_coree_zip(src)
    use_query_flag = "--query" in cql_help.stdout
    zero_match_rx = re.compile(
        r"\b(?:0\s+(?:objects?\s+)?match(?:ed|es)?|match(?:ed|es)?\s*[:=]?\s*0\b|found\s+0\b|did not match|no\s+(?:objects?\s+)?match(?:ed|es)?)\b"
        r"|(?:^|[\s,{])[\"']?(?:count|total|matched|matches)[\"']?\s*[:=]\s*0\b"
        r"|(?:^|[\s,{])[\"']?(?:matches|results|objects)[\"']?\s*[:=]\s*\[\s*\]",
        re.I,
    )
    pos_match_rx = re.compile(
        r"\b[1-9]\d*\s+(?:objects?\s+)?match(?:ed|es)?\b"
        r"|(?:^|[\s,{])[\"']?(?:count|total|matched|matches)[\"']?\s*[:=]?\s*[1-9]\d*\b",
        re.I,
    )

    def _parse_cql_json(stdout: str) -> Optional[tuple[Optional[int], Optional[list[str]], bool]]:
        raw = stdout.strip()
        if not raw or raw[0] not in ("{", "["):
            return None
        try:
            parsed = json.loads(raw)
        except json.JSONDecodeError:
            return None
        if not isinstance(parsed, (dict, list)):
            return None

        def _extract_names(items: list[Any]) -> list[str]:
            out: list[str] = []
            for it in items:
                if isinstance(it, dict):
                    if it.get("name") is not None:
                        out.append(str(it["name"]))
                    elif it.get("title") is not None:
                        out.append(str(it["title"]))
                    else:
                        out.append(json.dumps(it, ensure_ascii=False))
                elif isinstance(it, str):
                    out.append(it)
                else:
                    out.append(str(it))
            return out

        if isinstance(parsed, list):
            return len(parsed), _extract_names(parsed), False

        explicit_counts: list[int] = []
        for ck in ("count", "total", "matched", "match_count", "matches"):
            val = parsed.get(ck)
            if isinstance(val, int) and not isinstance(val, bool):
                explicit_counts.append(val)
        list_val: Optional[list[Any]] = None
        for lk in ("matches", "results", "objects", "items"):
            val = parsed.get(lk)
            if isinstance(val, list):
                list_val = val
                break
        if not explicit_counts and list_val is None:
            return None, None, True
        if explicit_counts and any(c != explicit_counts[0] for c in explicit_counts):
            return None, None, True
        exp_count = explicit_counts[0] if explicit_counts else None
        if exp_count is not None and list_val is not None and exp_count != len(list_val):
            return None, None, True
        eff_count = exp_count if exp_count is not None else (len(list_val) if list_val is not None else None)
        names = _extract_names(list_val) if list_val is not None else None
        return eff_count, names, False

    def _check_positive_cql(
        cql_res: Any, query_label: str, expected_name: str, forbidden_names: tuple[str, ...]
    ) -> Optional[TestOutcome]:
        j = _parse_cql_json(cql_res.stdout)
        if j is not None:
            eff_count, names, malformed = j
            if malformed or eff_count is None or eff_count <= 0:
                return TestOutcome(
                    "FAIL",
                    f"W4 ADD-11: coree-cli cql {query_label} reported zero/invalid matches: {cql_res.stdout[:300]}",
                )
            if (
                names is None
                or not any(expected_name in n for n in names)
                or any(bad in n for n in names for bad in forbidden_names)
            ):
                return TestOutcome(
                    "FAIL",
                    f"W4 ADD-11: coree-cli cql {query_label} returned unexpected JSON matches: {cql_res.stdout[:300]}",
                )
            return None
        if zero_match_rx.search(cql_res.stdout):
            return TestOutcome(
                "FAIL",
                f"W4 ADD-11: coree-cli cql {query_label} reported zero/no matches: {cql_res.stdout[:300]}",
            )
        if expected_name not in cql_res.stdout or any(bad in cql_res.stdout for bad in forbidden_names):
            return TestOutcome(
                "FAIL",
                f"W4 ADD-11: coree-cli cql {query_label} returned unexpected matches: {cql_res.stdout[:300]}",
            )
        return None

    q1_args = [str(ctx.cli_bin), "cql", str(src), *(["--query"] if use_query_flag else []), "@color == 'CutContour'"]
    cql_q1 = run_cmd(q1_args, cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if cql_q1.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql query failed (rc={cql_q1.returncode}): {cql_q1.stderr or cql_q1.stdout}")
    fail_q1 = _check_positive_cql(cql_q1, "'@color == CutContour'", "Die Cut Box", ("Primary Rect", "Badge Ellipse"))
    if fail_q1 is not None:
        return fail_q1

    q2_args = [str(ctx.cli_bin), "cql", str(src), *(["--query"] if use_query_flag else []), "@name == 'Primary Rect'"]
    cql_q2 = run_cmd(q2_args, cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if cql_q2.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql control query failed (rc={cql_q2.returncode}): {cql_q2.stderr or cql_q2.stdout}")
    fail_q2 = _check_positive_cql(cql_q2, "'@name == Primary Rect'", "Primary Rect", ("Die Cut Box", "Badge Ellipse"))
    if fail_q2 is not None:
        return fail_q2

    q3_args = [str(ctx.cli_bin), "cql", str(src), *(["--query"] if use_query_flag else []), "@name == 'NonExistent_Coree_XYZ'"]
    cql_q3 = run_cmd(q3_args, cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if cql_q3.returncode != 0:
        return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql negative query failed (rc={cql_q3.returncode}): {cql_q3.stderr or cql_q3.stdout}")
    j3 = _parse_cql_json(cql_q3.stdout)
    if j3 is not None:
        eff_count3, names3, malformed3 = j3
        if (
            malformed3
            or eff_count3 != 0
            or (
                names3 is not None
                and (
                    len(names3) > 0
                    or any(
                        bad in n
                        for n in names3
                        for bad in ("Die Cut Box", "Primary Rect", "Badge Ellipse", "Bilingual Title")
                    )
                )
            )
        ):
            return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql negative query '@name == NonExistent_Coree_XYZ' did not report 0 JSON matches: {cql_q3.stdout[:300]}")
    else:
        q3_out = cql_q3.stdout.strip()
        if (
            any(name in cql_q3.stdout for name in ("Die Cut Box", "Primary Rect", "Badge Ellipse", "Bilingual Title"))
            or pos_match_rx.search(cql_q3.stdout)
            or (q3_out and q3_out != "[]" and not zero_match_rx.search(q3_out))
        ):
            return TestOutcome("FAIL", f"W4 ADD-11: coree-cli cql negative query '@name == NonExistent_Coree_XYZ' did not report 0 matches: {cql_q3.stdout[:300]}")

    dom_script = work / "scene_dom_probe.rhai"
    dom_script.write_text(
        'let p0 = doc.pages[0];\n'
        'let l0 = p0.layers[0];\n'
        'let shapes = l0.shapes;\n'
        'if shapes.len() == 0 { throw "empty shapes"; }\n'
        'log("DOM_SHAPE=" + shapes[0].name);\n'
        'set_fill_hex([shapes[0].id], "#112233");\n',
        encoding="utf-8",
    )
    dom_out = work / "scene_dom_out.coree"
    rm = run_cmd([str(ctx.cli_bin), "run-macro", str(src), str(dom_script), str(dom_out)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if rm.returncode != 0 or "DOM_SHAPE=Primary Rect" not in rm.stdout or not dom_out.is_file():
        return TestOutcome("FAIL", f"W4 ADD-11: Rhai Scene DOM macro (doc.pages[0].layers[0].shapes) failed: rc={rm.returncode} out={rm.stdout[:200]} err={rm.stderr[:200]}")
    try:
        _, mutated_doc = inspect_coree_zip(dom_out)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-11: Rhai Scene DOM output .coree malformed: {exc}")
    orig_objs = orig_doc.get("objects")
    orig_obj3 = orig_objs.get("3") if isinstance(orig_objs, dict) else None
    orig_fill = orig_obj3.get("fill") if isinstance(orig_obj3, dict) else None
    mut_objs = mutated_doc.get("objects")
    mut_obj3 = mut_objs.get("3") if isinstance(mut_objs, dict) else None
    mut_fill = mut_obj3.get("fill") if isinstance(mut_obj3, dict) else None
    mut_solid = mut_fill.get("Solid") if isinstance(mut_fill, dict) else None
    mut_rgba = mut_solid.get("Rgba") if isinstance(mut_solid, dict) else None
    try:
        mut_r = float(mut_rgba.get("r", -1.0)) if isinstance(mut_rgba, dict) else -1.0
        mut_g = float(mut_rgba.get("g", -1.0)) if isinstance(mut_rgba, dict) else -1.0
        mut_b = float(mut_rgba.get("b", -1.0)) if isinstance(mut_rgba, dict) else -1.0
    except (TypeError, ValueError):
        mut_r = mut_g = mut_b = -1.0
    if (
        mut_fill == orig_fill
        or not isinstance(mut_rgba, dict)
        or abs(mut_r - (0x11 / 255.0)) > 0.02
        or abs(mut_g - (0x22 / 255.0)) > 0.02
        or abs(mut_b - (0x33 / 255.0)) > 0.02
    ):
        return TestOutcome("FAIL", f"W4 ADD-11: Rhai Scene DOM macro did not persist #112233 fill mutation to {dom_out.name}: fill={mut_fill}")

    macros_code = _read_rust_stripped(ctx, "crates/coree-prod/src/macros.rs")
    plugins_code = _read_rust_stripped(ctx, "crates/coree-prod/src/plugins.rs")
    if not re.search(r"\bon_before_export\b", macros_code):
        return TestOutcome("FAIL", "[Major][CrlCQL/CrlAutomation] W4 ADD-11: macros.rs missing Hierarchical Scene DOM / event hooks")
    if not re.search(r"\b(?:jsonrpc|json_rpc|JsonRpc)\b", plugins_code, re.I):
        return TestOutcome("FAIL", "[Major][CrlAutomation] W4: plugins.rs missing JSON-RPC external engine/plugin host protocol")
    for filt, label in (("jsonrpc", "W4 ADD-11 jsonrpc"), ("on_before_export", "W4 ADD-11 event hooks"), ("cql", "W4 ADD-11 cql")):
        fail = _run_cargo_tests(ctx, ["-p", "coree-prod", "--", filt], label)
        if fail is not None:
            return fail
    return TestOutcome("PASS", "coree-cli cql + Rhai Scene DOM + event hooks + JSON-RPC plugin host verified")


def _test_w4_eps_svg_cad_rem08(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-REM08")
    src = _f(ctx, "fixture.coree")

    # Construct EPS transparency test document with DropShadow on Badge Ellipse (no page background fill)
    _, alpha_doc = inspect_coree_zip(src)
    alpha_doc["pages"][0]["printable_background"] = False
    alpha_doc["objects"]["4"]["effects"] = [
        {
            "DropShadow": {
                "dx_mm": 3.0,
                "dy_mm": 3.0,
                "blur_mm": 4.0,
                "opacity": 0.6,
                "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}},
                "feather_direction": "Outside",
            }
        }
    ]
    alpha_coree = work / "alpha_shadow.coree"
    _write_coree_from_doc(alpha_coree, "EPS Alpha DropShadow", alpha_doc)

    out_eps = work / "alpha.eps"
    r = run_cmd([str(ctx.cli_bin), "convert", str(alpha_coree), str(out_eps)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0 or not out_eps.is_file():
        return TestOutcome("FAIL", f"convert -> .eps failed: {r.stderr}")

    gs_res = run_ghostscript_render(out_eps, work / "gs_alpha.png", dpi=96, device="pngalpha", timeout_scale=ctx.timeout_scale)
    if not gs_res["ok"]:
        return TestOutcome("FAIL", f"[Blocker][CdrPsi] W4 REM-08: Ghostscript pngalpha failed on EPS: {gs_res.get('stderr')}")
    alpha_stats = gs_res.get("alpha_stats") or {}
    if (
        gs_res["png_info"]["non_white_pixels"] <= 50
        or gs_res["png_info"]["corner_pixel"][3] != 0
        or alpha_stats.get("transparent_pixels", 0) == 0
        or alpha_stats.get("non_transparent_pixels", 0) <= 50
        or alpha_stats.get("opaque_white_pixels", 1) != 0
        or alpha_stats.get("near_white_opaque_pixels", 1) != 0
    ):
        return TestOutcome(
            "FAIL",
            f"[Blocker][CdrPsi] W4 REM-08: EPS white-knockout or empty render detected: "
            f"corner={gs_res['png_info'].get('corner_pixel')}, alpha_stats={alpha_stats}",
        )

    # Verify DXF TEXT + DIMENSION export & PLT CI/AA arc export
    out_dxf = work / "rem08.dxf"
    out_plt = work / "rem08.plt"
    r_dxf = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_dxf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dxf.returncode != 0 or not out_dxf.is_file():
        return TestOutcome("FAIL", f"W4 REM-08: convert -> .dxf failed: {r_dxf.stderr}")
    r_plt = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_plt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_plt.returncode != 0 or not out_plt.is_file():
        return TestOutcome("FAIL", f"W4 REM-08: convert -> .plt failed: {r_plt.stderr}")
    try:
        dxf_s = parse_dxf_structure(out_dxf.read_bytes())
        plt_s = parse_plt_commands(out_plt.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08: Exported DXF/PLT malformed: {exc}")
    if not any(e in ("TEXT", "MTEXT") for e in dxf_s["entities"]):
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: Exported DXF dropped ArtisticText (entities={dxf_s['entities']})")
    if not plt_s["has_ci_or_aa"]:
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: Exported PLT flattened curves without CI/AA arc commands (opcodes={set(plt_s['opcodes'])})")

    # Verify DXF DIMENSION export on a document containing a DimensionObject
    _, dim_doc = inspect_coree_zip(src)
    dim_doc["objects"]["8"] = {
        "id": 8,
        "name": "Width Dim",
        "payload": {
            "Dimension": {
                "kind": "Horizontal",
                "p1": {"x": 20.0, "y": 20.0},
                "p2": {"x": 110.0, "y": 20.0},
                "p3": None,
                "offset_mm": 10.0,
                "precision": 2,
                "prefix": "",
                "suffix": " mm",
                "label_override": None,
                "text_style": dim_doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"],
                "unit_override": None,
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None",
        "outline": dim_doc["objects"]["3"]["outline"],
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
    dim_doc["pages"][0]["layers"][0]["objects"].append(8)
    dim_doc["next_id"] = 9
    dim_in = work / "dim.coree"
    _write_coree_from_doc(dim_in, "Dim Test", dim_doc)
    dim_dxf = work / "dim.dxf"
    r_dim = run_cmd([str(ctx.cli_bin), "convert", str(dim_in), str(dim_dxf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dim.returncode != 0 or not dim_dxf.is_file():
        return TestOutcome("FAIL", f"W4 REM-08: convert Dimension .coree -> .dxf failed: {r_dim.stderr}")
    try:
        dim_dxf_s = parse_dxf_structure(dim_dxf.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08: Exported Dimension DXF malformed: {exc}")
    if "DIMENSION" not in dim_dxf_s["entities"]:
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: Exported DXF missing DIMENSION entity (entities={dim_dxf_s['entities']})")

    # Verify SVG TwoColorPattern exports distinct geometry per pattern_name (not static 2x2 3-rect checkerboard)
    _, pat_doc = inspect_coree_zip(src)
    pat_doc["objects"]["3"]["fill"] = {
        "TwoColorPattern": {
            "fg": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}},
            "bg": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}},
            "pattern_name": "DiagonalStripe",
            "tile_w": 10.0,
            "tile_h": 10.0,
        }
    }
    pat_in = work / "pattern.coree"
    _write_coree_from_doc(pat_in, "TwoColorPattern Test", pat_doc)
    pat_svg = work / "pattern.svg"
    r_pat = run_cmd([str(ctx.cli_bin), "convert", str(pat_in), str(pat_svg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_pat.returncode != 0 or not pat_svg.is_file():
        return TestOutcome("FAIL", f"W4 REM-08: convert TwoColorPattern -> .svg failed: {r_pat.stderr}")
    try:
        svg_root = ET.fromstring(pat_svg.read_text(encoding="utf-8"))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08: Exported TwoColorPattern SVG malformed XML: {exc}")
    patterns = [el for el in svg_root.iter() if el.tag.split("}", 1)[-1] == "pattern"]
    if not patterns:
        return TestOutcome("FAIL", "W4 REM-08: Exported SVG missing <pattern> for TwoColorPattern")
    diag_pat_id = patterns[0].attrib.get("id", "").strip()
    if not _svg_references_pattern_id(svg_root, diag_pat_id):
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern='DiagonalStripe' <pattern id={diag_pat_id!r}> is not referenced via url(#...) in SVG")
    pat_children = [c.tag.split("}", 1)[-1] for c in patterns[0]]
    if pat_children == ["rect", "rect", "rect"]:
        return TestOutcome("FAIL", "[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern emitted static 2x2 3-rect checkerboard ignoring pattern_name='DiagonalStripe'")
    if not _pattern_has_diagonal_geometry(patterns[0]):
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern='DiagonalStripe' lacks diagonal geometry (children={pat_children})")

    # Convert a second control document with a distinct pattern_name ('PolkaDots') and verify <pattern> differs structurally & has curved dot geometry
    pat2_doc = json.loads(json.dumps(pat_doc))
    pat2_doc["objects"]["3"]["fill"]["TwoColorPattern"]["pattern_name"] = "PolkaDots"
    pat2_in = work / "pattern_polka.coree"
    _write_coree_from_doc(pat2_in, "TwoColorPattern PolkaDots Control", pat2_doc)
    pat2_svg = work / "pattern_polka.svg"
    r_pat2 = run_cmd([str(ctx.cli_bin), "convert", str(pat2_in), str(pat2_svg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_pat2.returncode != 0 or not pat2_svg.is_file():
        return TestOutcome("FAIL", f"W4 REM-08: convert PolkaDots TwoColorPattern -> .svg failed: {r_pat2.stderr}")
    try:
        svg2_root = ET.fromstring(pat2_svg.read_text(encoding="utf-8"))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08: Exported PolkaDots SVG malformed XML: {exc}")
    pat2 = [el for el in svg2_root.iter() if el.tag.split("}", 1)[-1] == "pattern"]
    if not pat2:
        return TestOutcome("FAIL", "W4 REM-08: Exported PolkaDots SVG missing <pattern>")
    polka_pat_id = pat2[0].attrib.get("id", "").strip()
    if not _svg_references_pattern_id(svg2_root, polka_pat_id):
        return TestOutcome("FAIL", f"[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern='PolkaDots' <pattern id={polka_pat_id!r}> is not referenced via url(#...) in SVG")
    if _pattern_structure_signature(patterns[0]) == _pattern_structure_signature(pat2[0]):
        return TestOutcome("FAIL", "[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern emitted identical <pattern> structure (ignoring id) for 'DiagonalStripe' and 'PolkaDots'")
    if not _pattern_has_curved_dot_geometry(pat2[0]):
        return TestOutcome("FAIL", "[Major][CdrPsi] W4 REM-08: SVG TwoColorPattern='PolkaDots' lacks circular/curved dot geometry (circle/ellipse/curved path)")

    return TestOutcome("PASS", "EPS transparent alpha (gs -dEPSCrop pngalpha, 0 opaque white) + DXF TEXT/DIMENSION + PLT CI/AA + SVG TwoColorPattern verified")


def _test_w4_preflight_missing_font_and_cups_ipp(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-PREFLIGHT-IPP")
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"]["font_family"] = "NonExistentCoreeTestFont_XYZ_999"
    in_missing = work / "missing_font.coree"
    _write_coree_from_doc(in_missing, "Missing Font Probe", doc)

    r = run_cmd([str(ctx.cli_bin), "preflight", str(in_missing), "--json"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0:
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json failed: {r.stderr}")
    try:
        rep = _require_dict(json.loads(r.stdout), "preflight missing_font")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json returned invalid JSON: {exc}")
    issues = rep.get("issues")
    if not isinstance(issues, list) or "MissingFont" not in json.dumps(issues):
        return TestOutcome("FAIL", f"W4 REQ-07: preflight --json did not report MissingFont for missing font family (issues={issues!r})")

    pr_code = _read_rust_stripped(ctx, "crates/coree-app/src/printing.rs")
    pr_comments = _read_rust_comments_only(ctx, "crates/coree-app/src/printing.rs")
    if (
        re.search(r'::new\s*\(\s*"(?:lp|lpstat|lpr|powershell)"', pr_comments)
        or re.search(r"\bCommand::new\s*\(", pr_code)
        or re.search(r"\b(?:std::process::Command|process::Command)\b", pr_code)
    ):
        return TestOutcome("FAIL", "W4 REQ-10: printing.rs still shells out via std::process::Command instead of native CUPS IPP")
    if not re.search(r"\bipp\b", pr_code, re.I):
        return TestOutcome("FAIL", "W4 REQ-10: printing.rs missing native CUPS IPP support")
    has_transport = bool(re.search(r"\b(?:UnixStream|TcpStream)\b", pr_code))
    has_ipp_proto = (
        "application/ipp" in pr_comments
        or "ipp://" in pr_comments
        or bool(re.search(r"\b(?:631|submit_ipp)\b", pr_code))
    )
    if not has_transport or not has_ipp_proto:
        return TestOutcome(
            "FAIL",
            f"W4 REQ-10: printing.rs lacks real native IPP transport/protocol (has_transport={has_transport}, has_ipp_proto={has_ipp_proto})",
        )
    fail_pr = _run_cargo_tests(ctx, ["-p", "coree-app", "--", "ipp"], "W4 REQ-10")
    if fail_pr is not None:
        return fail_pr
    col_code = _read_rust_stripped(ctx, "crates/coree-prod/src/collect.rs")
    if not re.search(r"\bfsType\b|\bfstype\b", col_code, re.I):
        return TestOutcome("FAIL", "W4 REQ-07: collect.rs missing OS/2 fsType font embedding audit")
    return TestOutcome("PASS", "Live preflight MissingFont + OS/2 fsType + native CUPS IPP verified")


def _test_w4_potrace_and_photo_16bit(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W4-POTRACE")
    out_svg = work / "potrace_out.svg"
    r = run_cmd(
        [str(ctx.cli_bin), "trace", str(_f(ctx, "trace_shape.png")), str(out_svg), "--mode", "outline", "--colors", "4"],
        cwd=work,
        timeout=25.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r.returncode != 0 or not out_svg.is_file():
        return TestOutcome("FAIL", f"W4 IMP-05: trace failed: {r.stderr}")
    try:
        svg_root = ET.fromstring(out_svg.read_text(encoding="utf-8"))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-05: trace output SVG malformed XML: {exc}")
    paths = [el.attrib.get("d", "") for el in svg_root.iter() if el.tag.split("}", 1)[-1] == "path"]
    if not paths or not all(("Z" in d or "z" in d) for d in paths):
        return TestOutcome("FAIL", f"W4 IMP-05: trace produced open/empty SVG paths: {paths[:3]}")

    # Runtime multi-color adjacent-region trace zero-gap check (touching red + blue blocks must leave 0 white seam pixels)
    aw, ah = 64, 64
    adj_buf = bytearray(aw * ah * 4)
    for yy in range(ah):
        for xx in range(aw):
            off = (yy * aw + xx) * 4
            if 8 <= yy < 56 and 8 <= xx < 32:
                adj_buf[off:off + 4] = bytes((220, 40, 40, 255))
            elif 8 <= yy < 56 and 32 <= xx < 56:
                adj_buf[off:off + 4] = bytes((30, 80, 220, 255))
            else:
                adj_buf[off:off + 4] = bytes((255, 255, 255, 255))
    adj_png = work / "adj_trace_in.png"
    adj_svg = work / "adj_trace_out.svg"
    adj_ren = work / "adj_trace_ren.png"
    adj_png.write_bytes(_encode_png_rgba_stdlib(aw, ah, adj_buf))
    r_adj = run_cmd([str(ctx.cli_bin), "trace", str(adj_png), str(adj_svg), "--mode", "outline", "--colors", "4"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_adj.returncode != 0 or not adj_svg.is_file():
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color adjacent trace failed: {r_adj.stderr}")
    try:
        adj_svg_root = ET.fromstring(adj_svg.read_text(encoding="utf-8"))
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color adjacent trace SVG malformed XML: {exc}")
    adj_paths = [
        el.attrib.get("d", "")
        for el in adj_svg_root.iter()
        if el.tag.split("}", 1)[-1] == "path" and el.attrib.get("d", "").strip()
    ]
    if len(adj_paths) < 2 or not all(("Z" in d or "z" in d) for d in adj_paths):
        return TestOutcome(
            "FAIL",
            f"W4 IMP-05: multi-color adjacent trace expected >= 2 closed <path> elements, got {len(adj_paths)}: {adj_paths[:3]}",
        )
    r_aren = run_cmd([str(ctx.cli_bin), "render", str(adj_svg), str(adj_ren), "--dpi", "300"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_aren.returncode != 0 or not adj_ren.is_file():
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color traced SVG render failed: {r_aren.stderr}")
    try:
        rw, rh, r_rgba = _decode_png_rgba_bytes_stdlib(adj_ren.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color traced SVG render PNG malformed: {exc}")
    if rw < 32 or rh < 32:
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color traced SVG render dimensions too small: {rw}x{rh}")
    def _adj_px(px: int, py: int) -> tuple[int, int, int, int]:
        p_off = (py * rw + px) * 4
        return r_rgba[p_off], r_rgba[p_off + 1], r_rgba[p_off + 2], r_rgba[p_off + 3]
    left_px = _adj_px(rw // 4, rh // 2)
    right_px = _adj_px((3 * rw) // 4, rh // 2)
    if not (left_px[0] > 150 and left_px[1] < 100 and left_px[2] < 100):
        return TestOutcome("FAIL", f"W4 IMP-05: Potrace left block (rw//4, rh//2) not red-dominant: {left_px}")
    if not (right_px[2] > 150 and right_px[0] < 100):
        return TestOutcome("FAIL", f"W4 IMP-05: Potrace right block (3*rw//4, rh//2) not blue-dominant: {right_px}")
    for cx_m, cy_m in ((0, 0), (rw - 1, 0), (0, rh - 1), (rw - 1, rh - 1)):
        corner_px = _adj_px(cx_m, cy_m)
        if not (corner_px[0] >= 245 and corner_px[1] >= 245 and corner_px[2] >= 245):
            return TestOutcome("FAIL", f"W4 IMP-05: Potrace outer corner ({cx_m}, {cy_m}) not white: {corner_px}")
    cx = rw // 2
    white_seam = 0
    for yy in range(rh // 4, (3 * rh) // 4):
        for dx in (-1, 0, 1):
            off = (yy * rw + (cx + dx)) * 4
            if r_rgba[off] >= 245 and r_rgba[off + 1] >= 245 and r_rgba[off + 2] >= 245:
                white_seam += 1
    if white_seam > 0:
        return TestOutcome("FAIL", f"W4 IMP-05: multi-color adjacent-region trace left {white_seam} white gap pixels along shared boundary")

    tr_code = _read_rust_stripped(ctx, "crates/coree-render/src/trace.rs")
    if not re.search(r"\bpotrace\b", tr_code, re.I):
        return TestOutcome("FAIL", "W4 IMP-05: trace.rs does not implement topological Potrace")
    re_code = _read_rust_stripped(ctx, "crates/coree-render/src/raster_engine.rs")
    eff_code = _read_rust_stripped(ctx, "crates/coree-core/src/effects.rs")
    has_16bit_cow = bool(
        re.search(r"\b(?:cow|copy_on_write)\b|\bArc\s*<", re_code, re.I)
        and re.search(r"\b(?:u16_pixels|channels_u16|PixelDepth|Bit16|Rgb16|Rgba16|Cmyk16|16bit)\b|\bVec\s*<\s*u16\s*>", re_code, re.I)
    )
    if not has_16bit_cow or not re.search(r"\bChannelMixer\b", eff_code):
        return TestOutcome("FAIL", "W4 IMP-06/ADD-12: 16-bit tiled CoW RasterAsset / ChannelMixer missing")
    if not re.search(r"\bChannelMixer\b", re_code) or _has_empty_match_arm(re_code, r"(?:RasterFilter::)?ChannelMixer"):
        return TestOutcome("FAIL", "W4 ADD-12: ChannelMixer handling missing or empty no-op in crates/coree-render/src/raster_engine.rs")

    # Runtime CLI Raster + ChannelMixer vs identity render pixel-diff check
    _, cm_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    asset_uuid = "22222222-2222-4222-8222-222222222222"
    raw_px = bytes([220, 40, 20, 255] * (16 * 16))
    cm_doc["assets"] = {
        asset_uuid: {
            "id": asset_uuid,
            "width": 16,
            "height": 16,
            "rgba_bytes": base64.b64encode(raw_px).decode("ascii"),
            "source_dpi": 72.0,
            "color_mode": "Rgb",
        }
    }
    base_outline = dict(cm_doc["objects"]["3"]["outline"])
    cm_doc["objects"] = {
        "8": {
            "id": 8,
            "name": "ChannelMixer Probe",
            "payload": {
                "Raster": {
                    "asset_id": asset_uuid,
                    "width_mm": 60.0,
                    "height_mm": 60.0,
                    "filters": [],
                    "crop_rect": None,
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": "None",
            "outline": {**base_outline, "width_mm": 0.0},
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
    }
    cm_doc["pages"][0]["layers"][0]["objects"] = [8]
    cm_doc["pages"][0]["layers"][1]["objects"] = []
    cm_doc["next_id"] = 9
    cm_id_in = work / "raster_identity.coree"
    cm_id_png = work / "raster_identity.png"
    _write_coree_from_doc(cm_id_in, "Raster Identity", cm_doc)
    r_cm_id = run_cmd([str(ctx.cli_bin), "render", str(cm_id_in), str(cm_id_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cm_id.returncode != 0 or not cm_id_png.is_file():
        return TestOutcome("FAIL", f"W4 ADD-12: Raster identity render failed: {r_cm_id.stderr}")

    cm_doc["objects"]["8"]["payload"]["Raster"]["filters"] = [
        {"ChannelMixer": {"red": [0.0, 1.0, 0.0], "green": [1.0, 0.0, 0.0], "blue": [0.0, 0.0, 1.0], "monochrome": False}}
    ]
    cm_mix_in = work / "raster_channel_mixer.coree"
    cm_mix_png = work / "raster_channel_mixer.png"
    _write_coree_from_doc(cm_mix_in, "Raster ChannelMixer", cm_doc)
    r_cm_mix = run_cmd([str(ctx.cli_bin), "render", str(cm_mix_in), str(cm_mix_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cm_mix.returncode != 0 or not cm_mix_png.is_file():
        return TestOutcome("FAIL", f"W4 ADD-12: Raster ChannelMixer render failed: {r_cm_mix.stderr}")
    try:
        cm_diff = _compare_same_page_pngs(cm_id_png, cm_mix_png, "W4 ADD-12 ChannelMixer")
        if cm_diff["mean_abs_diff"] <= 5.0:
            return TestOutcome("FAIL", f"W4 ADD-12: ChannelMixer filter produced no color shift vs identity raster render: {cm_diff}")
        pw, ph, mix_rgba = _decode_png_rgba_bytes_stdlib(cm_mix_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 ADD-12: Raster ChannelMixer PNG malformed: {exc}")
    page_w_mm = float(cm_doc["pages"][0]["width_mm"])
    page_h_mm = float(cm_doc["pages"][0]["height_mm"])
    cx_px = max(0, min(pw - 1, int(round((50.0 / page_w_mm) * pw))))
    cy_px = max(0, min(ph - 1, int(round((50.0 / page_h_mm) * ph))))
    off = (cy_px * pw + cx_px) * 4
    mr, mg, mb, ma = mix_rgba[off], mix_rgba[off + 1], mix_rgba[off + 2], mix_rgba[off + 3]
    if not (mg > mr + 100 and mg > 180 and mb < 60 and ma > 200):
        return TestOutcome(
            "FAIL",
            f"W4 ADD-12: ChannelMixer R<->G swap on (220,40,20) raster did not produce green-dominant center pixel at (50mm,50mm): rgba={(mr, mg, mb, ma)}",
        )
    ex_px = max(0, min(pw - 1, int(round((5.0 / page_w_mm) * pw))))
    ey_px = max(0, min(ph - 1, int(round((5.0 / page_h_mm) * ph))))
    eoff = (ey_px * pw + ex_px) * 4
    er, eg, eb, ea = mix_rgba[eoff], mix_rgba[eoff + 1], mix_rgba[eoff + 2], mix_rgba[eoff + 3]
    if not (er >= 245 and eg >= 245 and eb >= 245):
        return TestOutcome(
            "FAIL",
            f"W4 ADD-12: ChannelMixer modified exterior page pixel at (5mm,5mm) outside [20..80,20..80]mm raster: rgba={(er, eg, eb, ea)}",
        )

    fail_tr = _run_cargo_tests(ctx, ["-p", "coree-render", "--", "trace"], "W4 IMP-05")
    if fail_tr is not None:
        return fail_tr
    for filt, label in (
        ("16bit", "W4 IMP-06 16-bit"),
        ("cow", "W4 IMP-06/ADD-12 CoW"),
    ):
        fail_ras = _run_cargo_tests(ctx, ["-p", "coree-render", "--", filt], label, min_passed=1)
        if fail_ras is not None:
            return fail_ras
    return TestOutcome("PASS", f"Topological zero-gap Potrace CLI + 16-bit CoW RasterAsset + ChannelMixer R<->G (rgba={(mr, mg, mb, ma)}, ext={(er, eg, eb, ea)}, diff={cm_diff['mean_abs_diff']:.2f}) verified")


def _test_w5_ui_65_icons_and_studio_form(ctx: RunContext) -> TestOutcome:
    tb_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/toolbox.rs")
    tbar_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/toolbar.rs")
    pal_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/palette_bar.rs")
    pal_comments = _read_rust_comments_only(ctx, "crates/coree-app/src/ui/palette_bar.rs")
    cv_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs")
    ty_code = _read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs")
    if re.search(r"\bDrawingBox\b", cv_code):
        return TestOutcome("FAIL", "W5 IMP-08: canvas.rs still contains generic DrawingBox fallback previews")
    if not re.search(r"\bTwoPointLine\b", tb_code) or not re.search(r"\bIllustrator\b", ty_code):
        return TestOutcome("FAIL", "W5 IMP-08/ADD-09: TwoPointLine / Illustrator preset / 65/65 property bars incomplete")

    # FEAT-37 / ADD-09: Right Swatch Bar long-press Tint Grid & drag-and-drop swatch
    has_tint_grid = bool(
        re.search(r"tint[\s_-]*grid", pal_comments, re.I)
        or re.search(r"\btint_grid\b|\bTintGrid\b|\bshow_tint_grid\b", pal_code)
    )
    has_swatch_dnd = bool(
        re.search(r"\b(?:click_and_drag|drag_started|dnd_set_payload|DragSwatch|drag_swatch)\b", pal_code)
    )
    if not has_tint_grid or not has_swatch_dnd:
        return TestOutcome(
            "FAIL",
            f"W5 FEAT-37/ADD-09: palette_bar.rs missing Long-press Tint Grid ({has_tint_grid}) or Swatch drag-and-drop ({has_swatch_dnd})",
        )

    # IMP-08: 65 distinct tool icons in paint_tool_icon (no shared | arms) & 65 creation+selection property bars in toolbar.rs
    pti_body = _extract_brace_block(tb_code, r"\bfn\s+paint_tool_icon\b")
    if not pti_body:
        return TestOutcome("FAIL", "W5 IMP-08: fn paint_tool_icon not found in toolbox.rs")
    shared_icon_arms = re.findall(r"ToolId::([A-Za-z0-9_]+)\s*\|", pti_body)
    distinct_icon_tools = set(re.findall(r"ToolId::([A-Za-z0-9_]+)", pti_body))
    if shared_icon_arms or len(distinct_icon_tools) < 65:
        return TestOutcome(
            "FAIL",
            f"W5 IMP-08: paint_tool_icon lacks 65 distinct per-tool icons (shared_arms={shared_icon_arms}, distinct={len(distinct_icon_tools)})",
        )
    missing_prop_bars = [
        sym for sym in ("ToolId::TwoPointLine", "NodePayload::Table", "NodePayload::Dimension", "NodePayload::Connector")
        if sym not in tbar_code
    ]
    if missing_prop_bars:
        return TestOutcome(
            "FAIL",
            f"W5 IMP-08: toolbar.rs missing creation/selection Property Bar handlers for {missing_prop_bars}",
        )

    fail = _run_cargo_tests(ctx, ["-p", "coree-app", "--lib"], "W5 IMP-08", timeout=120.0)
    if fail is not None:
        return fail
    return TestOutcome("PASS", "65/65 distinct icons & property bars, Tint Grid & swatch DnD, zero DrawingBox + coree-app tests verified")


def _test_w5_i18n_rtl_page_bg_and_packaging(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W5-BG-PKG")
    cs_json = ctx.repo_root / "packaging" / "linux" / "cargo-sources.json"
    if not cs_json.is_file():
        return TestOutcome("FAIL", "W5 REQ-11: packaging/linux/cargo-sources.json missing for offline Flatpak build")
    try:
        cs_data = json.loads(cs_json.read_text(encoding="utf-8"))
        if not isinstance(cs_data, list) or not cs_data:
            return TestOutcome("FAIL", "W5 REQ-11: cargo-sources.json must be a non-empty JSON list")
    except json.JSONDecodeError as exc:
        return TestOutcome("FAIL", f"W5 REQ-11: invalid JSON in cargo-sources.json: {exc}")

    # Test Page.printable_background via live CLI render
    _, doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    doc["pages"][0]["printable_background"] = True
    doc["pages"][0]["background_fill"] = {"Solid": {"Rgba": {"r": 0.12, "g": 0.50, "b": 0.75, "a": 1.0}}}
    bg_in = work / "page_bg.coree"
    _write_coree_from_doc(bg_in, "Page Bg Test", doc)
    bg_png = work / "page_bg.png"
    r_bg = run_cmd([str(ctx.cli_bin), "render", str(bg_in), str(bg_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_bg.returncode != 0 or not bg_png.is_file():
        return TestOutcome("FAIL", f"W5 REQ-12: CLI render with printable_background failed: {r_bg.stderr}")
    try:
        bg_info = parse_png_info(bg_png.read_bytes())
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 REQ-12: printable_background PNG malformed: {exc}")
    if (
        bg_info["width"] < 32
        or bg_info["height"] < 32
        or bg_info["non_white_pixels"] < 20
        or bg_info["corner_pixel"][:3] == (255, 255, 255)
    ):
        return TestOutcome(
            "FAIL",
            f"W5 REQ-12: printable_background PNG invalid/blank or missed corner ({bg_info['width']}x{bg_info['height']}, non_white={bg_info['non_white_pixels']}, corner={bg_info['corner_pixel']})",
        )

    theme_code = (
        _read_rust_stripped(ctx, "crates/coree-app/src/theme.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-app/src/i18n.rs")
    )
    i18n_block = (
        _extract_brace_block(theme_code, r"\bpub\s+enum\s+I18nKey\s*\{")
        or _extract_brace_block(theme_code, r"\benum\s+I18nKey\s*\{")
    )
    if not i18n_block or not re.search(r"\bShellLayoutDirections\b", theme_code):
        return TestOutcome("FAIL", "W5 IMP-10: I18nKey compile-time catalog or ShellLayoutDirections RTL mirroring missing")
    i18n_count = _count_enum_variants(i18n_block)
    if i18n_count < 488:
        return TestOutcome("FAIL", f"W5 IMP-10: I18nKey enum has {i18n_count} variants (expected >= 488)")
    return TestOutcome("PASS", f"Valid cargo-sources.json + live printable Page background render + I18nKey ({i18n_count}) + RTL Shell Mirroring verified")


def _test_w5_view_simple_wireframe_and_inspector_ui(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T1-W5-SWF")
    res = run_cmd([str(ctx.cli_bin), "render", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if "simple-wireframe" not in res.stdout.lower():
        return TestOutcome("FAIL", "W5 ADD-08: `simple-wireframe` view mode not yet exposed in coree-cli render --help")

    swf_png = work / "swf.png"
    enh_png = work / "enh.png"
    r1 = run_cmd([str(ctx.cli_bin), "render", str(_f(ctx, "fixture.coree")), str(swf_png), "--dpi", "72", "--view", "simple-wireframe"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    r2 = run_cmd([str(ctx.cli_bin), "render", str(_f(ctx, "fixture.coree")), str(enh_png), "--dpi", "72", "--view", "enhanced"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r1.returncode != 0 or r2.returncode != 0:
        return TestOutcome("FAIL", f"W5 ADD-08: render --view simple-wireframe failed: {r1.stderr}")
    try:
        diff = _compare_same_page_pngs(swf_png, enh_png, "W5 ADD-08 simple-wireframe")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-08: simple-wireframe/enhanced PNG malformed: {exc}")
    if diff["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W5 ADD-08: simple-wireframe render identical to enhanced ({diff})")

    dock_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/dockers.rs")
    dlg_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/dialogs.rs")
    if "1256" not in dlg_code and "Cp1256" not in dlg_code:
        return TestOutcome("FAIL", "W5 REQ-08: S15 CP1256 codepage selector missing in dialogs.rs")
    if not re.search(r"\bHex\b|hex_view", dock_code):
        return TestOutcome("FAIL", "W5 REQ-08: S16 Hex chunk viewer missing in dockers.rs")

    # ADD-07: Facing Pages / Spreads, dynamic AutoPageNumber substitution across pages 1-2, and DrawingScale (1:10..1:100)
    core_doc_txt = (
        _read_rust_stripped(ctx, "crates/coree-core/src/document.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-core/src/text.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-core/src/dimension.rs")
    )
    ren_all_txt = (
        _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
        + "\n"
        + _read_rust_stripped(ctx, "crates/coree-render/src/text_layout.rs")
    )
    if not re.search(r"\bAutoPageNumber\b", core_doc_txt) or not re.search(r"\bAutoPageNumber\b", ren_all_txt):
        return TestOutcome("FAIL", "W5 ADD-07: dynamic AutoPageNumber field missing in coree-core or coree-render")
    if not re.search(r"\bDrawingScale\b|\bdrawing_scale\b", core_doc_txt) or not re.search(r"\bDrawingScale\b|\bdrawing_scale\b", ren_all_txt):
        return TestOutcome("FAIL", "W5 ADD-07: engineering DrawingScale (1:10..1:100) missing in coree-core or coree-render")
    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
    layout_fn = _extract_brace_block(ren_code, r"\bfn\s+compute_page_layouts\b")
    if not layout_fn or "facing_pages" not in layout_fn:
        return TestOutcome("FAIL", "W5 ADD-07: compute_page_layouts in renderer.rs does not implement facing_pages spread layout")

    # Runtime multipage AutoPageNumber check: Master AllPages object must render distinct output on Page 1 vs Page 2
    _, apn_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    base_style = dict(apn_doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    base_outline = dict(apn_doc["objects"]["3"]["outline"])
    apn_doc["objects"] = {
        "20": {
            "id": 20,
            "name": "Master Page Number",
            "payload": {
                "ArtisticText": {
                    "runs": [
                        {
                            "text": "1",
                            "style": {**base_style, "font_size_pt": 48.0, "direction": "Ltr", "align": "Left"},
                            "field": "AutoPageNumber",
                        }
                    ],
                    "direction": "Ltr",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 40.0, 40.0]},
            "fill": "None",
            "outline": {**base_outline, "width_mm": 0.0},
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
    }
    apn_p1_layer = {
        "id": 2,
        "name": "Master Number Layer",
        "visible": True,
        "locked": False,
        "printable": True,
        "master_scope": "AllPages",
        "is_cut_layer": False,
        "is_desktop_layer": False,
        "objects": [20],
    }
    apn_p2 = json.loads(json.dumps(apn_doc["pages"][0]))
    apn_p2["id"] = 101
    apn_p2["name"] = "Page 2"
    apn_p2["layers"] = [
        {
            "id": 102,
            "name": "Page 2 Layer",
            "visible": True,
            "locked": False,
            "printable": True,
            "master_scope": "None",
            "is_cut_layer": False,
            "is_desktop_layer": False,
            "objects": [],
        }
    ]
    apn_doc["pages"][0]["layers"] = [apn_p1_layer]
    apn_doc["pages"] = [apn_doc["pages"][0], apn_p2]
    apn_doc["next_id"] = 103
    apn_in = work / "autopagenum.coree"
    apn_p1_png = work / "autopagenum_p1.png"
    apn_p2_png = work / "autopagenum_p2.png"
    _write_coree_from_doc(apn_in, "AutoPageNumber Multipage", apn_doc)
    r_ap1 = run_cmd([str(ctx.cli_bin), "render", str(apn_in), str(apn_p1_png), "--page", "0", "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    r_ap2 = run_cmd([str(ctx.cli_bin), "render", str(apn_in), str(apn_p2_png), "--page", "1", "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ap1.returncode != 0 or r_ap2.returncode != 0 or not apn_p1_png.is_file() or not apn_p2_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber multipage render failed: {r_ap1.stderr or r_ap2.stderr}")
    try:
        apn_p1_bytes = apn_p1_png.read_bytes()
        apn_p2_bytes = apn_p2_png.read_bytes()
        apn_p1_info = parse_png_info(apn_p1_bytes)
        apn_p2_info = parse_png_info(apn_p2_bytes)
        if (apn_p1_info["width"], apn_p1_info["height"]) != (apn_p2_info["width"], apn_p2_info["height"]) or apn_p1_info["width"] < 32 or apn_p1_info["height"] < 32:
            return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber PNG dimensions invalid/mismatched: {apn_p1_info} vs {apn_p2_info}")
        apn_w1, apn_h1, apn_rgba1 = _decode_png_rgba_bytes_stdlib(apn_p1_bytes)
        apn_w2, apn_h2, apn_rgba2 = _decode_png_rgba_bytes_stdlib(apn_p2_bytes)
        if (apn_w1, apn_h1) != (apn_w2, apn_h2):
            return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber decoded RGBA dimensions mismatched: {(apn_w1, apn_h1)} vs {(apn_w2, apn_h2)}")
        # Crop local AutoPageNumber region (25..95, 25..75) mm so parse_png_info runs 1x1 (<= 40_000 px) around (40, 40) mm
        apn_pw_mm = float(apn_doc["pages"][0]["width_mm"])
        apn_ph_mm = float(apn_doc["pages"][0]["height_mm"])
        cx0 = max(0, int(25.0 / apn_pw_mm * apn_w1))
        cx1 = min(apn_w1, int(95.0 / apn_pw_mm * apn_w1))
        cy0 = max(0, int(25.0 / apn_ph_mm * apn_h1))
        cy1 = min(apn_h1, int(75.0 / apn_ph_mm * apn_h1))
        cw, ch = cx1 - cx0, cy1 - cy0
        if cw < 32 or ch < 32:
            return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber crop box degenerate: {cw}x{ch}")
        crop_p1_rgba = bytearray(cw * ch * 4)
        crop_p2_rgba = bytearray(cw * ch * 4)
        for row in range(ch):
            src_off = ((cy0 + row) * apn_w1 + cx0) * 4
            dst_off = row * cw * 4
            crop_p1_rgba[dst_off:dst_off + cw * 4] = apn_rgba1[src_off:src_off + cw * 4]
            crop_p2_rgba[dst_off:dst_off + cw * 4] = apn_rgba2[src_off:src_off + cw * 4]
        apn_p1_crop_png = work / "autopagenum_p1_crop.png"
        apn_p2_crop_png = work / "autopagenum_p2_crop.png"
        apn_p1_crop_png.write_bytes(_encode_png_rgba_stdlib(cw, ch, bytes(crop_p1_rgba)))
        apn_p2_crop_png.write_bytes(_encode_png_rgba_stdlib(cw, ch, bytes(crop_p2_rgba)))
        crop_p1_info = parse_png_info(apn_p1_crop_png.read_bytes())
        crop_p2_info = parse_png_info(apn_p2_crop_png.read_bytes())
        if crop_p1_info["non_white_pixels"] < 100 or crop_p2_info["non_white_pixels"] < 100:
            return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber rendered blank/too few pixels in local region: p1={crop_p1_info['non_white_pixels']}, p2={crop_p2_info['non_white_pixels']}")
        apn_crop_diff = _compare_same_page_pngs(
            apn_p1_crop_png,
            apn_p2_crop_png,
            "W5 ADD-07 AutoPageNumber",
            min_non_white_a=100,
            min_non_white_b=100,
        )
        apn_full_diff = compare_png_pixels(apn_p1_png, apn_p2_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber PNG malformed: {exc}")
    if (
        apn_crop_diff["mean_abs_diff"] <= 1.0
        or apn_crop_diff["non_white_iou"] >= 0.85
        or apn_crop_diff["nonzero_diff_ratio"] <= 0.005
        or not apn_full_diff.get("size_match")
        or apn_full_diff["max_channel_diff"] <= 0
        or apn_full_diff["nonzero_diff_ratio"] <= 0.0
        or apn_full_diff["non_white_iou"] >= 0.85
    ):
        return TestOutcome("FAIL", f"W5 ADD-07: AutoPageNumber rendered identical/insufficiently distinct output on Page 1 vs Page 2: crop={apn_crop_diff}, full={apn_full_diff}")

    # Runtime 3-page Facing Pages spread render check
    _, fp_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    p0 = json.loads(json.dumps(fp_doc["pages"][0]))
    p1 = json.loads(json.dumps(fp_doc["pages"][0]))
    p2 = json.loads(json.dumps(fp_doc["pages"][0]))
    p1["id"] = 101
    p1["name"] = "Page 2"
    p1["layers"] = [{"id": 102, "name": "L2", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [3]}]
    p2["id"] = 103
    p2["name"] = "Page 3"
    p2["layers"] = [{"id": 104, "name": "L3", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [4]}]
    fp_doc["pages"] = [p0, p1, p2]
    fp_off_in = work / "spread_off.coree"
    fp_off_png = work / "spread_off.png"
    _write_coree_from_doc(fp_off_in, "Spread Off", fp_doc)
    r_soff = run_cmd([str(ctx.cli_bin), "render", str(fp_off_in), str(fp_off_png), "--dpi", "48", "--multipage"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_soff.returncode != 0 or not fp_off_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-07: multipage non-facing render failed: {r_soff.stderr}")
    for pg in fp_doc["pages"]:
        pg["facing_pages"] = True
    fp_on_in = work / "spread_on.coree"
    fp_on_png = work / "spread_on.png"
    _write_coree_from_doc(fp_on_in, "Spread On", fp_doc)
    r_son = run_cmd([str(ctx.cli_bin), "render", str(fp_on_in), str(fp_on_png), "--dpi", "48", "--multipage"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_son.returncode != 0 or not fp_on_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-07: multipage facing_pages render failed: {r_son.stderr}")
    try:
        fp_off_info = parse_png_info(fp_off_png.read_bytes())
        fp_on_bytes = fp_on_png.read_bytes()
        fp_on_info = parse_png_info(fp_on_bytes)
        if (
            fp_off_info["width"] < 32
            or fp_off_info["height"] < 32
            or fp_on_info["width"] < 32
            or fp_on_info["height"] < 32
            or fp_off_info["non_white_pixels"] < 20
            or fp_on_info["non_white_pixels"] < 20
            or fp_on_info["width"] < int(0.6 * fp_off_info["width"])
        ):
            return TestOutcome(
                "FAIL",
                f"W5 ADD-07: facing_pages multipage render invalid/degenerate ({fp_off_info['width']}x{fp_off_info['height']} nw={fp_off_info['non_white_pixels']} vs {fp_on_info['width']}x{fp_on_info['height']} nw={fp_on_info['non_white_pixels']})",
            )
        fp_on_w, fp_on_h, fp_on_rgba = _decode_png_rgba_bytes_stdlib(fp_on_bytes)
        left_nw = 0
        right_nw = 0
        top_nw = 0
        bottom_nw = 0
        half_w = fp_on_w // 2
        half_h = fp_on_h // 2
        for y in range(fp_on_h):
            row_off = y * fp_on_w * 4
            for x in range(fp_on_w):
                off = row_off + x * 4
                r, g, b, a = fp_on_rgba[off], fp_on_rgba[off + 1], fp_on_rgba[off + 2], fp_on_rgba[off + 3]
                if (r < 250 or g < 250 or b < 250) and a > 10:
                    if x < half_w:
                        left_nw += 1
                    else:
                        right_nw += 1
                    if y < half_h:
                        top_nw += 1
                    else:
                        bottom_nw += 1
        if left_nw < 10 or right_nw < 10 or (
            fp_on_info["height"] > fp_off_info["height"] and (top_nw < 10 or bottom_nw < 10)
        ):
            return TestOutcome(
                "FAIL",
                f"W5 ADD-07: facing_pages spread missing artwork in spread halves (left={left_nw}, right={right_nw}, top={top_nw}, bottom={bottom_nw})",
            )
        spread_cmp = compare_png_pixels(fp_off_png, fp_on_png)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07: facing_pages PNG malformed: {exc}")
    if (fp_on_info["width"], fp_on_info["height"]) == (fp_off_info["width"], fp_off_info["height"]) and spread_cmp["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W5 ADD-07: facing_pages=True produced identical multipage spread layout to facing_pages=False: {spread_cmp}")

    # Runtime DrawingScale (1:1 vs 1:10) Dimension measurement render diff + drawing_scale cargo test
    _, ds_doc = inspect_coree_zip(_f(ctx, "fixture.coree"))
    ds_style = dict(ds_doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    ds_outline = dict(ds_doc["objects"]["3"]["outline"])
    ds_doc["objects"] = {
        "30": {
            "id": 30,
            "name": "Scale Dimension",
            "payload": {
                "Dimension": {
                    "kind": "Horizontal",
                    "p1": {"x": 20.0, "y": 50.0},
                    "p2": {"x": 100.0, "y": 50.0},
                    "p3": None,
                    "offset_mm": 12.0,
                    "precision": 2,
                    "prefix": "",
                    "suffix": "",
                    "label_override": None,
                    "text_style": {**ds_style, "font_size_pt": 24.0, "direction": "Ltr", "align": "Center"},
                    "unit_override": None,
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
            "fill": "None",
            "outline": {**ds_outline, "color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, "width_mm": 0.5, "is_hairline": False},
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
    }
    ds_doc["pages"][0]["layers"][0]["objects"] = [30]
    ds_doc["pages"][0]["layers"][1]["objects"] = []
    ds_doc["next_id"] = 31
    ds_doc["drawing_scale"] = {"numerator": 1.0, "denominator": 1.0}
    ds1_in = work / "drawing_scale_1_1.coree"
    ds1_png = work / "drawing_scale_1_1.png"
    _write_coree_from_doc(ds1_in, "DrawingScale 1:1", ds_doc)
    r_ds1 = run_cmd([str(ctx.cli_bin), "render", str(ds1_in), str(ds1_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ds1.returncode != 0 or not ds1_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-07: DrawingScale 1:1 render failed: {r_ds1.stderr}")
    try:
        if parse_png_info(ds1_png.read_bytes())["non_white_pixels"] <= 20:
            return TestOutcome("FAIL", "W5 ADD-07: DrawingScale 1:1 Dimension rendered blank page")
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07: DrawingScale 1:1 PNG malformed: {exc}")

    ds_doc["drawing_scale"] = {"numerator": 1.0, "denominator": 10.0}
    ds10_in = work / "drawing_scale_1_10.coree"
    ds10_png = work / "drawing_scale_1_10.png"
    _write_coree_from_doc(ds10_in, "DrawingScale 1:10", ds_doc)
    r_ds10 = run_cmd([str(ctx.cli_bin), "render", str(ds10_in), str(ds10_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ds10.returncode != 0 or not ds10_png.is_file():
        return TestOutcome("FAIL", f"W5 ADD-07: DrawingScale 1:10 render failed: {r_ds10.stderr}")
    try:
        ds_diff = _compare_same_page_pngs(ds1_png, ds10_png, "W5 ADD-07 DrawingScale", min_non_white_a=21)
    except _CLI_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07: DrawingScale 1:10 PNG malformed: {exc}")
    if ds_diff["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W5 ADD-07: DrawingScale 1:10 produced identical Dimension render to 1:1: {ds_diff}")

    ds_passed = 0
    for crate_name in ("coree-core", "coree-render"):
        ds_res = _run_cargo(ctx, ["test", "-p", crate_name, "--", "drawing_scale"], timeout=90.0)
        ds_matches = re.findall(r"(\d+)\s+passed;\s+(\d+)\s+failed", ds_res.stdout)
        if ds_res.returncode != 0 or any(int(f) > 0 for _, f in ds_matches):
            return TestOutcome("FAIL", f"W5 ADD-07: {crate_name} drawing_scale cargo test failed (rc={ds_res.returncode}): {ds_res.stdout[-300:]}")
        ds_passed += sum(int(p) for p, _ in ds_matches)
    if ds_passed < 1:
        return TestOutcome("FAIL", "W5 ADD-07: cargo test -- drawing_scale matched 0 tests across coree-core/coree-render")

    # IMP-09: Interactive visual multi-plate PrintPreviewDialog, S03 ReorderPages UI, and 65/65 shortcuts
    pp_thumb_fn = _extract_brace_block(dlg_code, r"\bfn\s+draw_print_preview_thumbnail\b")
    if not pp_thumb_fn or "separations_preview" not in pp_thumb_fn:
        return TestOutcome("FAIL", "W5 IMP-09: draw_print_preview_thumbnail in dialogs.rs lacks interactive multi-plate separation preview")
    cv_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs")
    if "CommandOp::ReorderPages" not in (cv_code + "\n" + dock_code):
        return TestOutcome("FAIL", "W5 IMP-09: S03 page drag-reorder in canvas.rs/dockers.rs does not dispatch CommandOp::ReorderPages")
    sc_code = _read_rust_stripped(ctx, "crates/coree-app/src/shortcuts.rs")
    ty_code = _read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs")
    sa_fn = _extract_brace_block(ty_code, r"\bfn\s+shortcut_action\b")
    if not sa_fn or re.search(r"_\s*=>\s*None\b", sa_fn):
        return TestOutcome("FAIL", "W5 IMP-09: ToolId::shortcut_action in types.rs leaves tools without customizable ShortcutAction (65/65 required)")
    if not re.search(r"\bSelectTool\b|\bToolId::", sc_code):
        return TestOutcome("FAIL", "W5 IMP-09: shortcuts.rs lacks 65-tool shortcut customization")

    return TestOutcome("PASS", "Live simple-wireframe + ADD-07 (facing_pages/AutoPageNumber/DrawingScale) + IMP-09 (PrintPreview plates/ReorderPages/65 shortcuts) + S15 CP1256 + S16 Hex verified")


# ---------------------------------------------------------------------------
# Tier 1 Test Registry (Covers ALL 51/51 Roadmap IDs)
# ---------------------------------------------------------------------------

def get_tests() -> list[TestCase]:
    return [
        # --- Baseline (smoke=True) ---
        TestCase(
            id="T1-BASE-CLI-CORE",
            tier=1,
            title="coree-cli --version, --diagnostics, --help subcommands & sibling GUI binary",
            roadmap_ids=("MND-05",),
            feature_refs=("EXT-F01", "EXT-F02"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_cli_core,
        ),
        TestCase(
            id="T1-BASE-GEN-INSPECT",
            tier=1,
            title="generate-sample + inspect --json + independent Python/file dissectors",
            roadmap_ids=("REQ-06", "REM-07"),
            feature_refs=("FEAT-07", "FEAT-81", "FEAT-82", "EXT-F04"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_generate_and_inspect,
        ),
        TestCase(
            id="T1-BASE-ROUNDTRIP-VEC",
            tier=1,
            title="Vector round-trips + Ghostscript + Poppler + TrimBox pixel comparison",
            roadmap_ids=("REQ-04", "ADD-06", "REM-08"),
            feature_refs=("FEAT-21", "FEAT-22", "FEAT-35", "FEAT-75", "FEAT-83", "FEAT-84", "FEAT-85", "FEAT-86"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_roundtrip_vector_and_external_oracles,
        ),
        TestCase(
            id="T1-BASE-EMPTY-PATH",
            tier=1,
            title="Pure-Rust self-contained PDF/EPS/render fallback with PATH=''",
            roadmap_ids=("MND-05", "REQ-04"),
            feature_refs=("EXT-F02", "FEAT-83", "FEAT-84"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_pure_rust_empty_path_fallback,
        ),
        TestCase(
            id="T1-BASE-RENDER-THUMB",
            tier=1,
            title="Render 4 view modes, multipage & thumbnail --size 64 PNG pixel invariants",
            roadmap_ids=("IMP-01",),
            feature_refs=("FEAT-05", "FEAT-08"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_render_viewmodes_and_thumbnail,
        ),
        TestCase(
            id="T1-BASE-LASER",
            tier=1,
            title="Laser G-Code (GRBL/Marlin/Ruida), DXF, PLT, kerf & lightburn-bridge",
            roadmap_ids=("ADD-05", "REM-09"),
            feature_refs=("EXT-F06", "EXT-F08"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_laser_cut,
        ),
        TestCase(
            id="T1-BASE-PREFLIGHT-COLL",
            tier=1,
            title="Preflight --json, Collect for Output bundle & ProductionMetrics Path Meter",
            roadmap_ids=("REQ-07",),
            feature_refs=("FEAT-77", "FEAT-80", "EXT-F07"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_preflight_and_collect,
        ),
        TestCase(
            id="T1-BASE-PRINT-MERGE",
            tier=1,
            title="Print Merge bilingual Latin+Arabic UTF-8 CSV -> .coree & PDF",
            roadmap_ids=("IMP-07",),
            feature_refs=("FEAT-78", "EXT-F05"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_print_merge,
        ),
        TestCase(
            id="T1-BASE-TRACE",
            tier=1,
            title="PowerTRACE PNG/BMP -> SVG <path> + re-render verification",
            roadmap_ids=("IMP-05",),
            feature_refs=("FEAT-70", "FEAT-71"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_trace_raster,
        ),
        TestCase(
            id="T1-BASE-RHAI-MACRO",
            tier=1,
            title="Rhai file macro & built-in recolor-cmyk semantic fill check",
            roadmap_ids=("ADD-11",),
            feature_refs=("EXT-F11", "SKIP-06"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_rhai_macro,
        ),
        TestCase(
            id="T1-BASE-HYGIENE-PAL",
            tier=1,
            title="Workspace #![forbid(unsafe_code)], 0 network crates, no #009c4a, valid .gpl/.ase/.pal",
            roadmap_ids=("MND-05",),
            feature_refs=("FEAT-37", "SKIP-01", "SKIP-02", "SKIP-03", "EXT-F02"),
            target_wave="baseline",
            smoke=True,
            run=_test_base_hygiene_and_palettes,
        ),
        TestCase(
            id="T1-BASE-LENSES",
            tier=1,
            title="All 11 non-destructive Lenses (FEAT-52..62) semantic CLI render vs control",
            roadmap_ids=("REM-04",),
            feature_refs=(
                "FEAT-52", "FEAT-53", "FEAT-54", "FEAT-55", "FEAT-56", "FEAT-57",
                "FEAT-58", "FEAT-59", "FEAT-60", "FEAT-61", "FEAT-62",
            ),
            target_wave="baseline",
            smoke=True,
            run=_test_base_all_eleven_lenses,
        ),
        TestCase(
            id="T1-REPLICA-CONTRAST",
            tier=1,
            title="Replica contrast.py 35/35 AA pass + negative contrast rejection",
            roadmap_ids=("MND-01",),
            feature_refs=("SKIP-05", "EXT-F02"),
            target_wave="baseline",
            smoke=True,
            run=_test_replica_contrast,
        ),
        TestCase(
            id="T1-REPLICA-LISTING",
            tier=1,
            title="Replica listing.py store compliance + negative avoid-term rejection",
            roadmap_ids=("MND-01",),
            feature_refs=("SKIP-05", "EXT-F01"),
            target_wave="baseline",
            smoke=True,
            run=_test_replica_listing,
        ),
        TestCase(
            id="T1-REPLICA-PARITY",
            tier=1,
            title="Replica parity.py features.csv 86/68/6/12 integrity",
            roadmap_ids=("MND-05",),
            feature_refs=("SKIP-01", "SKIP-02", "SKIP-03", "SKIP-04", "SKIP-05", "SKIP-06"),
            target_wave="baseline",
            smoke=True,
            run=_test_replica_parity,
        ),
        TestCase(
            id="T1-REPLICA-SWEEP-APP",
            tier=1,
            title="Replica sweep.py crates/coree-app clean (0 hits)",
            roadmap_ids=("MND-01",),
            feature_refs=("SKIP-05",),
            target_wave="baseline",
            smoke=True,
            run=_test_replica_sweep_app,
        ),

        # --- Wave 1 (smoke=False) ---
        TestCase(
            id="T1-W1-SWEEP-CRATES",
            tier=1,
            title="W1 MND-01: sweep.py crates/ returns 0 hits",
            roadmap_ids=("MND-01",),
            feature_refs=("SKIP-05", "EXT-F02"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_sweep_crates,
        ),
        TestCase(
            id="T1-W1-SWEEP-PACKAGING",
            tier=1,
            title="W1 MND-01: scripts/brand_sweep.py + packaging/ MIME allowlist CI gate & brand.md log",
            roadmap_ids=("MND-01", "REQ-11"),
            feature_refs=("EXT-F09",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_sweep_packaging_and_brand_md,
        ),
        TestCase(
            id="T1-W1-COREE-TOOLS",
            tier=1,
            title="W1 MND-02: coree-tools crate without egui, 0 objects.get_mut & 0 drag mark_all_dirty() in canvas.rs",
            roadmap_ids=("MND-02",),
            feature_refs=("FEAT-02",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_coree_tools_separation,
        ),
        TestCase(
            id="T1-W1-FUZZ-PACK",
            tier=1,
            title="W1-W3 MND-04: fuzz/ targets & isolated real .cdr test pack compiled & inspected",
            roadmap_ids=("MND-04",),
            feature_refs=("FEAT-81",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_fuzz_and_real_cdr_pack,
        ),
        TestCase(
            id="T1-W1-VERSION-SYNC",
            tier=1,
            title="W1 MND-05: scripts/verify_invariants.py + automated version sync across manifests & ci.yml",
            roadmap_ids=("MND-05",),
            feature_refs=("EXT-F02",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_version_sync_and_ci,
        ),
        TestCase(
            id="T1-W1-COMMAND-OPS",
            tier=1,
            title="W1 REQ-01: CommandOp completeness, DocumentDelta, encapsulation & cargo test",
            roadmap_ids=("REQ-01",),
            feature_refs=("FEAT-08", "FEAT-14"),
            target_wave="W1",
            smoke=False,
            run=_test_w1_command_ops_and_delta,
        ),
        TestCase(
            id="T1-W1-VECTOR-CLIPBOARD",
            tier=1,
            title="W1 REQ-09: OS Live Vector Clipboard codec (SVG/PDF/EMF) + cargo test",
            roadmap_ids=("REQ-09",),
            feature_refs=(),
            target_wave="W1",
            smoke=False,
            run=_test_w1_live_vector_clipboard,
        ),
        TestCase(
            id="T1-W1-IPC-UDS",
            tier=1,
            title="W1 REQ-10: Bidirectional UDS (0600) / NamedPipe IPC + strict-no-std-net",
            roadmap_ids=("REQ-10",),
            feature_refs=("EXT-F01",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_ipc_uds_and_modularize,
        ),
        TestCase(
            id="T1-W1-AUTOSAVE-WAL",
            tier=1,
            title="W1 IMP-07: SQLite WAL + async differential autosave + corrupt DB quarantine",
            roadmap_ids=("IMP-07",),
            feature_refs=("EXT-F10",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_autosave_wal_and_quarantine,
        ),
        TestCase(
            id="T1-W1-MODULARIZE-ARCH",
            tier=1,
            title="W1 IMP-11: Reconcile replica/architecture.md & modularize bloated files",
            roadmap_ids=("IMP-11",),
            feature_refs=(),
            target_wave="W1",
            smoke=False,
            run=_test_w1_modularize_and_arch,
        ),
        TestCase(
            id="T1-W1-BENCHES-INIT",
            tier=1,
            title="W1 IMP-12: Quantitative offline benchmarks directory & cargo check --benches",
            roadmap_ids=("IMP-12",),
            feature_refs=("EXT-F12",),
            target_wave="W1",
            smoke=False,
            run=_test_w1_benches_init,
        ),

        # --- Wave 2 (smoke=False) ---
        TestCase(
            id="T1-W2-CDR-WRITER",
            tier=1,
            title="W2 MND-03: Authentic 20B reversed loda + CDRO 16B ZCF + docPalette.xml + non-flattening Table/Dim/Conn",
            roadmap_ids=("MND-03",),
            feature_refs=("FEAT-81", "FEAT-28", "FEAT-29", "FEAT-30"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_cdr_writer_authentic,
        ),
        TestCase(
            id="T1-W2-CDR-LEGACY-XML",
            tier=1,
            title="W2 REQ-06/REM-06: ZCF Spot color + quick-xml/roxmltree + zcf tests",
            roadmap_ids=("REQ-06", "REM-06"),
            feature_refs=("FEAT-81",),
            target_wave="W2",
            smoke=False,
            run=_test_w2_cdr_v3_wi_zcf_xml,
        ),
        TestCase(
            id="T1-W2-UNKNOWN-CHUNKS",
            tier=1,
            title="W2 REM-05: Byte-exact XDOC/XOBJ UnknownChunkMap passthrough in .cdr round-trip",
            roadmap_ids=("REM-05",),
            feature_refs=("FEAT-81", "EXT-F03"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_unknown_chunks_passthrough,
        ),
        TestCase(
            id="T1-W2-CMX-CPT-FULL",
            tier=1,
            title="W2 REM-07: Complete CMX (disp/ixlr/ixpg) round-trip & CPT render/CPTObject",
            roadmap_ids=("REM-07",),
            feature_refs=("FEAT-82",),
            target_wave="W2",
            smoke=False,
            run=_test_w2_cmx_cpt_full,
        ),
        TestCase(
            id="T1-W2-CURVES-CONT",
            tier=1,
            title="W2 REM-01/REM-10: Analytical erase_circle, PathNode continuity & cargo test",
            roadmap_ids=("REM-01", "REM-10"),
            feature_refs=("FEAT-15", "FEAT-17", "FEAT-35"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_curves_refit_and_continuity,
        ),
        TestCase(
            id="T1-W2-CONVERT-CURVES",
            tier=1,
            title="W2 REM-02: coree-cli convert-to-curves real glyph outline conversion",
            roadmap_ids=("REM-02",),
            feature_refs=("FEAT-40",),
            target_wave="W2",
            smoke=False,
            run=_test_w2_convert_to_curves_cli,
        ),
        TestCase(
            id="T1-W2-BSPLINE-SYM-PERSP",
            tier=1,
            title="W2 ADD-02/03/04: BSpline CLI render, PowerClip Fit & 3x3 Perspective",
            roadmap_ids=("ADD-02", "ADD-03", "ADD-04"),
            feature_refs=("FEAT-18", "FEAT-27", "FEAT-42"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_bspline_symbol_perspective,
        ),
        TestCase(
            id="T1-W2-SMARTFILL-META",
            tier=1,
            title="W2 IMP-05/REM-03: Planar DCEL SmartFill & BooleanOp cargo tests",
            roadmap_ids=("IMP-05", "REM-03"),
            feature_refs=("FEAT-24", "FEAT-25"),
            target_wave="W2",
            smoke=False,
            run=_test_w2_smartfill_dcel_and_boolean_meta,
        ),

        # --- Wave 3 (smoke=False) ---
        TestCase(
            id="T1-W3-ICC-PIPELINE",
            tier=1,
            title="W3 REQ-02: Color/lerp routed via ICC + BPC + chad + CLI ICC render",
            roadmap_ids=("REQ-02",),
            feature_refs=("FEAT-31", "FEAT-36"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_icc_color_pipeline,
        ),
        TestCase(
            id="T1-W3-WGPU-TILES",
            tier=1,
            title="W3 IMP-01/IMP-02: WgpuBackend + WorldTileKey DirtyTileCache + cargo test",
            roadmap_ids=("IMP-01", "IMP-02"),
            feature_refs=("FEAT-05", "FEAT-13", "EXT-F12"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_wgpu_and_world_tilecache,
        ),
        TestCase(
            id="T1-W3-EFFECTS-COONS-LENS",
            tier=1,
            title="W3 ADD-01/IMP-04/REM-04: InnerShadow CLI render, Coons MeshFill & Frozen Lens",
            roadmap_ids=("ADD-01", "IMP-04", "REM-04"),
            feature_refs=("FEAT-33", "FEAT-43", "FEAT-63"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_effects_coons_and_frozen_lens,
        ),
        TestCase(
            id="T1-W3-TYPO-STYLUS",
            tier=1,
            title="W3 ADD-10/IMP-03: Linked text frames, SubpixelMask, Stylus/DisplayICC & cargo test",
            roadmap_ids=("ADD-10", "IMP-03"),
            feature_refs=("FEAT-38", "FEAT-39", "FEAT-40"),
            target_wave="W3",
            smoke=False,
            run=_test_w3_typography_and_stylus,
        ),

        # --- Wave 4 (smoke=False) ---
        TestCase(
            id="T1-W4-PDFX-SUBSET",
            tier=1,
            title="W4 REQ-03: Strict PDF/X-1a/4 + Ghostscript + pdfinfo + CIDFontType2 pdffonts/pdftotext",
            roadmap_ids=("REQ-03",),
            feature_refs=("FEAT-83", "FEAT-38", "EXT-F05"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_pdfx_and_font_subsetting,
        ),
        TestCase(
            id="T1-W4-DISPLAYLIST-SEP",
            tier=1,
            title="W4 REQ-04/REQ-05: ExportDisplayList + Vector Separations + Trapping + Halftone + PerfectBound",
            roadmap_ids=("REQ-04", "REQ-05"),
            feature_refs=("FEAT-74", "FEAT-76", "FEAT-83"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_display_list_and_separations,
        ),
        TestCase(
            id="T1-W4-DWG-EMF-ACB-PSD",
            tier=1,
            title="W4 ADD-06: .dwg/.emf/.acb inspect format & .emf/.wmf/.dwg/.psd export dissectors",
            roadmap_ids=("ADD-06",),
            feature_refs=("FEAT-37", "FEAT-86", "SKIP-04"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_dwg_emf_acb_psd_export,
        ),
        TestCase(
            id="T1-W4-FEFCO-2D-KERF",
            tier=1,
            title="W4 ADD-05/REM-09: 30+ FEFCO + 2D barcodes + Boolean Kerf CLI laser test",
            roadmap_ids=("ADD-05", "REM-09"),
            feature_refs=("FEAT-79", "EXT-F06", "EXT-F08"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_fefco_2d_barcodes_and_kerf,
        ),
        TestCase(
            id="T1-W4-RHAI-DOM-CQL",
            tier=1,
            title="W4 ADD-11: coree-cli cql + Rhai Scene DOM + event hooks + JSON-RPC plugin host",
            roadmap_ids=("ADD-11",),
            feature_refs=("FEAT-14", "EXT-F11"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_rhai_dom_cql_and_plugin_host,
        ),
        TestCase(
            id="T1-W4-EPS-TRANSP-CAD",
            tier=1,
            title="W4 REM-08: EPS transparent alpha (gs -dEPSCrop), dynamic SVG TwoColorPattern & DXF TEXT/DIMENSION + PLT CI/AA",
            roadmap_ids=("REM-08",),
            feature_refs=("FEAT-28", "FEAT-32", "FEAT-84", "FEAT-85"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_eps_svg_cad_rem08,
        ),
        TestCase(
            id="T1-W4-PREFLIGHT-IPP",
            tier=1,
            title="W4 REQ-07/REQ-10: Live MissingFont preflight, OS/2 fsType collect & native CUPS IPP",
            roadmap_ids=("REQ-07", "REQ-10"),
            feature_refs=("FEAT-77", "FEAT-80"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_preflight_missing_font_and_cups_ipp,
        ),
        TestCase(
            id="T1-W4-POTRACE-PHOTO16",
            tier=1,
            title="W4 IMP-05/06/ADD-12: Topological Potrace closed SVG + 16-bit CoW RasterAsset & ChannelMixer",
            roadmap_ids=("IMP-05", "IMP-06", "ADD-12"),
            feature_refs=("FEAT-68", "FEAT-70", "FEAT-71", "FEAT-86"),
            target_wave="W4",
            smoke=False,
            run=_test_w4_potrace_and_photo_16bit,
        ),

        # --- Wave 5 (smoke=False) ---
        TestCase(
            id="T1-W5-UI-65-NO-BOX",
            tier=1,
            title="W5 IMP-08/ADD-09: 65/65 icons & prop bars, zero DrawingBox, Illustrator preset & coree-app tests",
            roadmap_ids=("IMP-08", "ADD-09"),
            feature_refs=("FEAT-02", "FEAT-06", "FEAT-37"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_ui_65_icons_and_studio_form,
        ),
        TestCase(
            id="T1-W5-I18N-BG-PKG",
            tier=1,
            title="W5 REQ-11/12/IMP-10: Valid cargo-sources.json, live printable Page bg render, I18nKey & RTL Mirroring",
            roadmap_ids=("REQ-11", "REQ-12", "IMP-10"),
            feature_refs=("FEAT-11", "EXT-F01", "EXT-F05", "EXT-F09"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_i18n_rtl_page_bg_and_packaging,
        ),
        TestCase(
            id="T1-W5-SWF-CP1256-UX",
            tier=1,
            title="W5 REQ-08/ADD-07/08/IMP-09: Live simple-wireframe render, S15 CP1256 & S16 Hex viewer",
            roadmap_ids=("REQ-08", "ADD-07", "ADD-08", "IMP-09"),
            feature_refs=("FEAT-05", "EXT-F04", "EXT-F05"),
            target_wave="W5",
            smoke=False,
            run=_test_w5_view_simple_wireframe_and_inspector_ui,
        ),
    ]
