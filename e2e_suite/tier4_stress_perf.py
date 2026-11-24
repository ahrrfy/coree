"""Tier 4: Real-world multi-feature production workloads, stress, fuzz & perf gates."""

from __future__ import annotations

import base64
import json
import math
import os
import random
import re
import struct
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Any, Optional

from e2e_suite.harness import (
    _ZIP_MAX_TOTAL_BYTES,
    RunContext,
    TestCase,
    TestOutcome,
    _bounded_zip_read_info,
    _decode_png_rgba_bytes_stdlib,
    _validate_zip_infolist,
    build_blank_white_png,
    build_cdr_riff_with_unknown_xdoc,
    build_cdr_x6_bytes,
    build_cmx_bytes,
    build_coree_bytes,
    build_cpt_bytes,
    build_dxf_bytes,
    build_eps_bytes,
    build_pdf_bytes,
    build_print_merge_template_coree_bytes,
    build_psd_bytes,
    build_svg_bytes,
    compare_png_pixels,
    crop_png_center,
    dissect_cdr_riff,
    dissect_cdr_zcf,
    dissect_psd,
    inspect_coree_zip,
    parse_dxf_structure,
    parse_pdf_structure,
    parse_plt_commands,
    parse_png_info,
    parse_svg_structure,
    require_cli_subcommand,
    run_cmd,
    run_ghostscript_render,
    run_pdftoppm,
    run_poppler_fonts,
    run_poppler_info,
    run_poppler_text,
    strip_rust_comments_and_strings,
    strip_rust_comments_only,
)
from e2e_suite.tier3_color_visual import (
    IccProfileOracle,
    ciede2000,
    srgb_to_lab_d50,
)

TIER = 4
TIER_NAME = "Real-World Workloads, Stress & Perf Gates"

_FIXED_TS = "2026-11-22T00:00:00Z"
_ZIP_DT = (2026, 11, 22, 0, 0, 0)
_PARSE_EXCS = (
    ValueError,
    KeyError,
    TypeError,
    json.JSONDecodeError,
    ET.ParseError,
    zipfile.BadZipFile,
    UnicodeDecodeError,
    OSError,
    struct.error,
)


# ---------------------------------------------------------------------------
# Shared Helpers (Pure Python Stdlib, RLIMIT_AS=512 MiB Safe)
# ---------------------------------------------------------------------------

_BIDI_CTRL_RE = re.compile(r"[\u200e\u200f\u202a-\u202e\u2066-\u2069]")


def _strip_bidi_controls(text: str) -> str:
    """Strip Unicode BiDi directional formatting controls inserted by Poppler pdftotext."""
    return _BIDI_CTRL_RE.sub("", text)


def _tier4_fixture(ctx: RunContext, rel: str) -> Path:
    return ctx.fixtures_dir / "tier4" / rel


def _collect_rust_files(ctx: RunContext, rel_path: str) -> list[Path]:
    p = ctx.repo_root / rel_path
    files: list[Path] = []
    if p.is_file():
        files.append(p)
    if rel_path.endswith(".rs"):
        mod_dir = p.with_suffix("")
        if mod_dir.is_dir():
            files.extend(sorted(mod_dir.rglob("*.rs")))
    return files


def _read_rust(ctx: RunContext, rel_path: str) -> str:
    files = _collect_rust_files(ctx, rel_path)
    if not files:
        return ""
    return "\n".join(
        strip_rust_comments_and_strings(f.read_text(encoding="utf-8", errors="replace"))
        for f in files
    )


def _read_rust_comments_only(ctx: RunContext, rel_path: str) -> str:
    files = _collect_rust_files(ctx, rel_path)
    if not files:
        return ""
    return "\n".join(
        strip_rust_comments_only(f.read_text(encoding="utf-8", errors="replace"))
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
    """Remove `#[cfg(...test...)]` gated `mod`/`fn`/`impl` blocks from stripped Rust source."""
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


def _extract_all_brace_blocks(stripped_code: str, header_rx: str) -> list[tuple[re.Match[str], str]]:
    n = len(stripped_code)
    results: list[tuple[re.Match[str], str]] = []
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
                results.append((m, stripped_code[i:j]))
                break
            elif ch == ";" and paren == 0 and bracket == 0:
                break
            i += 1
    return results


def _extract_brace_block(stripped_code: str, header_rx: str) -> Optional[str]:
    blocks = _extract_all_brace_blocks(stripped_code, header_rx)
    return blocks[0][1] if blocks else None


def _count_enum_variants(enum_block: str) -> int:
    inner = enum_block.strip()
    if inner.startswith("{") and inner.endswith("}"):
        inner = inner[1:-1]
    depth_p = depth_b = depth_k = 0
    variants = 0
    tok_has_ident = False
    for ch in inner:
        if ch == "(":
            depth_p += 1
        elif ch == ")":
            depth_p = max(0, depth_p - 1)
        elif ch == "{":
            depth_b += 1
        elif ch == "}":
            depth_b = max(0, depth_b - 1)
        elif ch == "[":
            depth_k += 1
        elif ch == "]":
            depth_k = max(0, depth_k - 1)
        elif ch == "," and depth_p == 0 and depth_b == 0 and depth_k == 0:
            if tok_has_ident:
                variants += 1
                tok_has_ident = False
        elif depth_p == 0 and depth_b == 0 and depth_k == 0 and (ch.isalpha() or ch == "_"):
            tok_has_ident = True
    if tok_has_ident:
        variants += 1
    return variants


_DUMMY_LET_PREFIX_RX = re.compile(
    r"^\s*let\s+(?:_[A-Za-z0-9_]*|\(\s*_[A-Za-z0-9_]*(?:\s*,\s*_[A-Za-z0-9_]*)+\s*\))\s*"
)


def _strip_leading_dummy_lets(inner: str) -> str:
    cur = inner
    while True:
        m = _DUMMY_LET_PREFIX_RX.match(cur)
        if not m:
            break
        n = len(cur)
        i = m.end()
        t_p = t_b = 0
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
        p_depth = b_depth = 0
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


def _skip_char_literal_end(code: str, pos: int) -> Optional[int]:
    n = len(code)
    if pos + 1 < n and code[pos + 1] == "\\":
        q = pos + 2
        if q < n:
            q += 1
        while q < n and code[q] not in "'\r\n":
            if code[q] == "\\":
                q += 2
            else:
                q += 1
        if q < n and code[q] == "'":
            return q + 1
        return None
    if pos + 2 < n and code[pos + 1] not in ("'", "\r", "\n") and code[pos + 2] == "'":
        return pos + 3
    return None


_RUST_EXPR_PREFIX_KWS = frozenset({
    "as", "in", "if", "while", "match", "return", "break", "else",
    "mut", "ref", "dyn", "impl", "const", "unsafe", "async", "try",
    "yield", "box", "for",
})
_RUST_TYPE_WALK_KWS = frozenset({
    "const", "mut", "dyn", "impl", "fn", "for", "unsafe", "extern",
    "Fn", "FnMut", "FnOnce",
})
_RUST_NON_TYPE_IDENT_KWS = frozenset({
    "if", "while", "match", "for", "return", "break", "continue",
    "else", "in", "as", "let", "loop", "unsafe", "async", "try",
    "const", "mut", "ref", "dyn", "impl", "fn", "where", "yield",
})
_RUST_AFTER_GENERIC_KWS = frozenset({"as", "else", "if", "where", "in", "for"})
_RUST_CLOSURE_PREFIX_KWS = frozenset({
    "move", "async", "return", "break", "yield", "in", "if", "while",
    "match", "else", "loop", "unsafe", "try", "const", "box",
})


def _is_closure_open_pipe(code: str, pos: int, min_pos: int) -> bool:
    p = pos - 1
    while p >= min_pos and code[p].isspace():
        p -= 1
    if p < min_pos:
        return True
    ch = code[p]
    if ch in "=,;([{:+-*/%&|^!<>~":
        return True
    if ch.isalpha() or ch == "_":
        w_start = p
        while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
            w_start -= 1
        q = w_start - 1
        while q >= min_pos and code[q].isspace():
            q -= 1
        is_path_sep = q >= min_pos + 1 and code[q - 1 : q + 1] == "::"
        if q >= min_pos and (code[q] in (".", "#") or is_path_sep):
            return False
        return code[w_start : p + 1] in _RUST_CLOSURE_PREFIX_KWS
    return False


def _scan_balanced_angle_end(code: str, pos: int, *, allow_colon: bool = False) -> Optional[int]:
    n = len(code)
    angle = 1
    brace = 0
    paren = 0
    bracket = 0
    k = pos + 1
    while k < n:
        ch = code[k]
        if ch == "'":
            ch_end = _skip_char_literal_end(code, k)
            if ch_end is not None:
                k = ch_end
                continue
        if ch == "{":
            brace += 1
        elif ch == "}":
            if brace == 0:
                return None
            brace -= 1
        elif ch == "(":
            paren += 1
        elif ch == ")":
            if paren == 0:
                return None
            paren -= 1
        elif ch == "[":
            bracket += 1
        elif ch == "]":
            if bracket == 0:
                return None
            bracket -= 1
        elif brace == 0 and paren == 0 and bracket == 0:
            if ch == "<" and not (k + 1 < n and code[k + 1] == "="):
                angle += 1
            elif ch == ">" and (k == 0 or code[k - 1] not in ("-", "=")):
                angle -= 1
                if angle == 0:
                    return k
            elif ch == ";" or code[k : k + 2] in ("||", "==", "!=", "<=", ">=", "=>"):
                return None
            elif not allow_colon and ch == ":" and not (k + 1 < n and code[k + 1] == ":") and not (k > pos + 1 and code[k - 1] == ":"):
                return None
            elif code[k : k + 2] == "&&":
                q = k - 1
                while q > pos and code[q].isspace():
                    q -= 1
                if q > pos and (code[q].isalnum() or code[q] in "_)]}>"):
                    return None
                k += 2
                continue
        k += 1
    return None


def _scan_balanced_angle_start(code: str, end_gt: int, min_pos: int) -> Optional[int]:
    bal_a = 1
    brace = 0
    paren = 0
    bracket = 0
    k = end_gt - 1
    while k >= min_pos:
        ch = code[k]
        if ch == "'":
            matched_char = False
            start_lim = max(min_pos, k - 14)
            for open_q in range(k - 2, start_lim - 1, -1):
                if code[open_q] == "'":
                    if (
                        (open_q == k - 2 and code[open_q + 1] not in ("\\", "'", "\r", "\n"))
                        or (open_q + 1 < k and code[open_q + 1] == "\\")
                    ) and _skip_char_literal_end(code, open_q) == k + 1:
                        k = open_q - 1
                        matched_char = True
                        break
            if matched_char:
                continue
            k -= 1
            continue
        if ch == "}":
            brace += 1
        elif ch == "{":
            if brace == 0:
                return None
            brace -= 1
        elif ch == ")":
            paren += 1
        elif ch == "(":
            if paren == 0:
                return None
            paren -= 1
        elif ch == "]":
            bracket += 1
        elif ch == "[":
            if bracket == 0:
                return None
            bracket -= 1
        elif brace == 0 and paren == 0 and bracket == 0:
            if ch == ">" and not (k - 1 >= min_pos and code[k - 1] in ("-", "=")):
                bal_a += 1
            elif ch == "<" and not (k + 1 < len(code) and code[k + 1] == "="):
                bal_a -= 1
                if bal_a == 0:
                    return k
            elif ch == ";":
                return None
            elif bal_a == 1 and k - 1 >= min_pos and code[k - 1 : k + 1] == "||":
                return None
            elif bal_a == 1 and k - 1 >= min_pos and code[k - 1 : k + 1] == "&&":
                q = k - 2
                while q >= min_pos and code[q].isspace():
                    q -= 1
                if q >= min_pos and (code[q].isalnum() or code[q] in "_)]}>"):
                    return None
                k -= 2
                continue
            elif k - 1 >= min_pos and code[k - 1 : k + 1] in ("->", "=>", "<=", ">=", "==", "!="):
                k -= 2
                continue
        k -= 1
    return None


def _has_bound_kw_before(code: str, plus_pos: int, min_pos: int) -> bool:
    q = plus_pos - 1
    b_p = 0
    b_k = 0
    while q >= min_pos:
        ch = code[q]
        if ch.isspace():
            q -= 1
            continue
        if ch == ")":
            b_p += 1
            q -= 1
            continue
        if ch == "(":
            if b_p == 0:
                return False
            b_p -= 1
            q -= 1
            continue
        if ch == "]":
            b_k += 1
            q -= 1
            continue
        if ch == "[":
            if b_k == 0:
                return False
            b_k -= 1
            q -= 1
            continue
        if ch == ">":
            if q - 1 >= min_pos and code[q - 1] in ("-", "="):
                if code[q - 1] == "=" and b_p == 0 and b_k == 0:
                    return False
                q -= 2
                continue
            if b_p == 0 and b_k == 0:
                lt = _scan_balanced_angle_start(code, q, min_pos)
                if lt is None:
                    return False
                q = lt - 1
                continue
            q -= 1
            continue
        if b_p > 0 or b_k > 0:
            if ch in ("{", "}", ";"):
                return False
            q -= 1
            continue
        if ch == "<":
            return False
        if ch == ":":
            if q - 1 >= min_pos and code[q - 1] == ":":
                q -= 2
                continue
            return False
        if ch in ("=", "{", "}", ";", ","):
            return False
        if ch.isalpha() or ch == "_":
            w_start = q
            while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
                w_start -= 1
            w = code[w_start : q + 1]
            if w in ("dyn", "impl"):
                prev = w_start - 1
                while prev >= min_pos and code[prev].isspace():
                    prev -= 1
                is_path_sep = prev >= min_pos + 1 and code[prev - 1 : prev + 1] == "::"
                if not (prev >= min_pos and (code[prev] in (".", "#") or is_path_sep)):
                    return True
            if w in _RUST_NON_TYPE_IDENT_KWS and w not in ("for", "const", "mut", "fn", "unsafe", "extern"):
                return False
            q = w_start - 1
            continue
        if ch in ("'", "&", "*", "+") or ch.isdigit():
            q -= 1
            continue
        return False
    return False


def _has_type_ctx_before(code: str, id_start: int, min_pos: int) -> bool:
    p = id_start - 1
    bal_p = 0
    bal_b = 0
    need_parens = 0
    just_skipped_path_sep = False
    in_bound_chain = False
    while p >= min_pos:
        if code[p].isspace():
            p -= 1
            continue
        prev_was_path_sep = just_skipped_path_sep
        just_skipped_path_sep = False
        ch = code[p]
        if ch == ")":
            bal_p += 1
            p -= 1
            continue
        if ch == "(":
            if bal_p > 0:
                bal_p -= 1
                p -= 1
                continue
            in_bound_chain = False
            q = p - 1
            while q >= min_pos and code[q].isspace():
                q -= 1
            if q < min_pos:
                return False
            if code[q] in ("(", "[", ",", ":", "*", "&"):
                if need_parens > 0:
                    need_parens -= 1
                p -= 1
                continue
            if q - 1 >= min_pos and code[q - 1 : q + 1] == "->":
                return True
            if code[q].isalpha() or code[q] == "_":
                w_start = q
                while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
                    w_start -= 1
                w = code[w_start : q + 1]
                if w_start > min_pos and code[w_start - 1] == "'":
                    if need_parens > 0:
                        need_parens -= 1
                    p = w_start - 2
                    continue
                if w_start == min_pos or code[w_start - 1] != "#":
                    if w == "as":
                        if need_parens > 0:
                            need_parens -= 1
                        if need_parens == 0:
                            return True
                        p = w_start - 1
                        continue
                    if w in ("const", "mut", "fn", "Fn", "FnMut", "FnOnce"):
                        if need_parens > 0:
                            need_parens -= 1
                        p = w_start - 1
                        continue
            return False
        if ch == "]":
            bal_b += 1
            p -= 1
            continue
        if ch == "[":
            if bal_b > 0:
                bal_b -= 1
                p -= 1
                continue
            in_bound_chain = False
            q = p - 1
            while q >= min_pos and code[q].isspace():
                q -= 1
            if q < min_pos:
                return False
            if code[q] in ("(", "[", ",", ":", "*", "&"):
                if need_parens > 0:
                    need_parens -= 1
                p -= 1
                continue
            if q - 1 >= min_pos and code[q - 1 : q + 1] == "->":
                return True
            if code[q].isalpha() or code[q] == "_":
                w_start = q
                while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
                    w_start -= 1
                w = code[w_start : q + 1]
                if w_start > min_pos and code[w_start - 1] == "'":
                    if need_parens > 0:
                        need_parens -= 1
                    p = w_start - 2
                    continue
                if w_start == min_pos or code[w_start - 1] != "#":
                    if w == "as":
                        if need_parens > 0:
                            need_parens -= 1
                        if need_parens == 0:
                            return True
                        p = w_start - 1
                        continue
                    if w in ("const", "mut", "fn", "Fn", "FnMut", "FnOnce"):
                        if need_parens > 0:
                            need_parens -= 1
                        p = w_start - 1
                        continue
            return False
        if bal_p > 0 or bal_b > 0:
            if ch in ("{", "}", ";"):
                return False
            p -= 1
            continue
        if ch == ",":
            in_bound_chain = False
            need_parens = 1
            p -= 1
            continue
        if ch == ":":
            if p - 1 >= min_pos and code[p - 1] == ":":
                just_skipped_path_sep = True
                p -= 2
                continue
            if need_parens > 0:
                p -= 1
                continue
            q = p - 1
            while q >= min_pos and code[q].isspace():
                q -= 1
            if q >= min_pos and (code[q].isalnum() or code[q] == "_"):
                while q >= min_pos and (code[q].isalnum() or code[q] == "_"):
                    q -= 1
                if q >= min_pos and code[q] == "'":
                    return False
            return True
        if ch == "+":
            if _has_bound_kw_before(code, p, min_pos):
                in_bound_chain = True
                p -= 1
                continue
            return False
        if ch == ">":
            if p - 1 >= min_pos and code[p - 1] == "-":
                if need_parens > 0:
                    p -= 2
                    continue
                return True
            if p - 1 >= min_pos and code[p - 1] == "=":
                return False
            lt = _scan_balanced_angle_start(code, p, min_pos)
            if lt is None:
                return False
            q_w = lt - 1
            while q_w >= min_pos and code[q_w].isspace():
                q_w -= 1
            if q_w >= min_pos and (code[q_w].isalnum() or code[q_w] == "_"):
                w_start = q_w
                while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
                    w_start -= 1
                if code[w_start : q_w + 1] == "for" and not (w_start > min_pos and code[w_start - 1] == "#"):
                    p = w_start - 1
                    continue
            if (prev_was_path_sep or in_bound_chain) and need_parens == 0:
                p = lt - 1
                continue
            return False
        if ch in ("*", "&", "'"):
            p -= 1
            continue
        if ch.isalnum() or ch == "_":
            w_start = p
            while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
                w_start -= 1
            w = code[w_start : p + 1]
            if w == "as":
                if need_parens == 0:
                    return True
                p = w_start - 1
                continue
            if w in _RUST_TYPE_WALK_KWS:
                p = w_start - 1
                continue
            if w in _RUST_NON_TYPE_IDENT_KWS:
                return False
            if w[0].isalpha() or w[0] == "_":
                p = w_start - 1
                continue
            return False
        return False
    return False


def _is_generic_angle_open(code: str, pos: int, min_pos: int, *, active_generic: bool) -> bool:
    n = len(code)
    if pos + 1 < n and code[pos + 1] == "=":
        return False
    if active_generic:
        return True
    pt = pos - 1
    while pt >= min_pos and code[pt].isspace():
        pt -= 1
    if pt >= min_pos + 1 and code[pt - 1 : pt + 1] == "::":
        return True
    is_qual_prefix = False
    if pt < min_pos:
        is_qual_prefix = True
    elif code[pt] in "([{,;+-*/%&|^!<>:=?.~":
        is_qual_prefix = True
    elif code[pt].isalpha() or code[pt] == "_":
        w_start = pt
        while w_start > min_pos and (code[w_start - 1].isalnum() or code[w_start - 1] == "_"):
            w_start -= 1
        if code[w_start : pt + 1] in _RUST_EXPR_PREFIX_KWS:
            is_qual_prefix = True
    if is_qual_prefix:
        end_gt = _scan_balanced_angle_end(code, pos, allow_colon=True)
        if end_gt is not None:
            la = end_gt + 1
            while la < n and code[la].isspace():
                la += 1
            if la + 1 < n and code[la : la + 2] == "::":
                return True
    if pt >= min_pos and (code[pt].isalnum() or code[pt] == "_"):
        id_start = pt
        while id_start > min_pos and (code[id_start - 1].isalnum() or code[id_start - 1] == "_"):
            id_start -= 1
        ident = code[id_start : pt + 1]
        if ident == "for":
            la_in = pos + 1
            while la_in < n and code[la_in].isspace():
                la_in += 1
            if la_in < n and code[la_in] == "'" and _scan_balanced_angle_end(code, pos) is not None:
                return True
        if (ident[0].isalpha() or ident[0] == "_") and ident not in _RUST_NON_TYPE_IDENT_KWS:
            if _has_type_ctx_before(code, id_start, min_pos):
                end_gt = _scan_balanced_angle_end(code, pos)
                if end_gt is not None:
                    la = end_gt + 1
                    while la < n and code[la].isspace():
                        la += 1
                    if la >= n:
                        return True
                    nxt = code[la]
                    if nxt in ",)]};>|!=&+-*/%?.<" or code[la : la + 2] == "::" or nxt == "{":
                        return True
                    if nxt.isalpha() or nxt == "_":
                        w_end = la + 1
                        while w_end < n and (code[w_end].isalnum() or code[w_end] == "_"):
                            w_end += 1
                        if code[la:w_end] in _RUST_AFTER_GENERIC_KWS:
                            return True
    return False


def _extract_match_arm_bodies(code: str, variant_rx: str) -> list[str]:
    arms: dict[tuple[int, int], str] = {}
    n = len(code)
    cur = 0
    cur_paren = 0
    cur_bracket = 0

    def _new_frame(
        kind: str,
        saved_p: int,
        saved_b: int,
        from_cond: Optional[list[Any]] = None,
    ) -> dict[str, Any]:
        return {
            "kind": kind,
            "saved_paren": saved_p,
            "saved_bracket": saved_b,
            "in_arm_body": False,
            "in_guard": False,
            "arm_expect_head": False,
            "arm_block_mode": None,
            "arm_cond_stack": [],
            "arm_after_else": False,
            "arm_turbo_levels": [],
            "arm_in_closure_params": False,
            "pending_matches": [],
            "from_cond": from_cond,
        }

    def _reset_arm_state(frame: dict[str, Any], *, in_body: bool) -> None:
        frame["in_arm_body"] = in_body
        frame["in_guard"] = False
        frame["arm_expect_head"] = in_body
        frame["arm_block_mode"] = None
        frame["arm_cond_stack"].clear()
        frame["arm_after_else"] = False
        frame["arm_turbo_levels"].clear()
        frame["arm_in_closure_params"] = False

    def _prune_turbo(frame: dict[str, Any], paren: int, bracket: int) -> None:
        tl = frame["arm_turbo_levels"]
        while tl and (tl[-1][0] > paren or tl[-1][1] > bracket):
            tl.pop()

    def _mark_arm_cond_tok(frame: dict[str, Any], val: bool) -> None:
        acs = frame["arm_cond_stack"]
        if acs and acs[-1][0] in ("if_cond", "while_cond", "for_iter", "let_expr"):
            acs[-1][1] = val

    def _active_pm(frame: dict[str, Any], paren: int, bracket: int) -> Optional[dict[str, Any]]:
        pms = frame["pending_matches"]
        if pms and pms[-1]["paren"] == paren and pms[-1]["bracket"] == bracket:
            return pms[-1]
        return None

    def _prune_pms(frame: dict[str, Any], paren: int, bracket: int) -> None:
        pms = frame["pending_matches"]
        while pms and (pms[-1]["paren"] > paren or pms[-1]["bracket"] > bracket):
            pms.pop()

    def _clear_pms_at_level(frame: dict[str, Any], paren: int, bracket: int) -> None:
        pms = frame["pending_matches"]
        while pms and pms[-1]["paren"] == paren and pms[-1]["bracket"] == bracket:
            pms.pop()

    stack: list[dict[str, Any]] = [_new_frame("root", 0, 0)]

    for m in re.finditer(rf"(?<![A-Za-z0-9_])(?:{variant_rx})\b", code):
        while cur < m.start():
            c = code[cur]
            if c.isspace():
                cur += 1
                continue
            top = stack[-1]
            pm = _active_pm(top, cur_paren, cur_bracket)
            if c == "(":
                if top["kind"] == "match" and top["in_arm_body"] and top["arm_expect_head"] and cur_paren == 0 and cur_bracket == 0:
                    top["arm_block_mode"] = "expr"
                    top["arm_expect_head"] = False
                if pm is not None:
                    pm["has_scrutinee_tok"] = True
                    pm["scrutinee_kw"] = None
                    if pm["cond_stack"]:
                        pm["cond_stack"][-1][1] = True
                cur_paren += 1
                cur += 1
                continue
            if c == ")":
                cur_paren = max(0, cur_paren - 1)
                _prune_pms(top, cur_paren, cur_bracket)
                _prune_turbo(top, cur_paren, cur_bracket)
                if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_turbo_levels"]:
                    _mark_arm_cond_tok(top, True)
                cur += 1
                continue
            if c == "[":
                if top["kind"] == "match" and top["in_arm_body"] and top["arm_expect_head"] and cur_paren == 0 and cur_bracket == 0:
                    top["arm_block_mode"] = "expr"
                    top["arm_expect_head"] = False
                if pm is not None:
                    pm["has_scrutinee_tok"] = True
                    pm["scrutinee_kw"] = None
                    if pm["cond_stack"]:
                        pm["cond_stack"][-1][1] = True
                cur_bracket += 1
                cur += 1
                continue
            if c == "]":
                cur_bracket = max(0, cur_bracket - 1)
                _prune_pms(top, cur_paren, cur_bracket)
                _prune_turbo(top, cur_paren, cur_bracket)
                if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_turbo_levels"]:
                    _mark_arm_cond_tok(top, True)
                cur += 1
                continue
            if c == "{":
                from_cond: Optional[list[Any]] = None
                if pm is not None:
                    is_scrutinee_block = False
                    if top["arm_turbo_levels"]:
                        is_scrutinee_block = True
                    elif not pm["has_scrutinee_tok"]:
                        is_scrutinee_block = True
                        pm["has_scrutinee_tok"] = True
                    elif pm["scrutinee_kw"] is not None:
                        is_scrutinee_block = True
                        pm["scrutinee_kw"] = None
                        if pm["cond_stack"]:
                            pm["cond_stack"][-1][1] = True
                    elif pm["cond_stack"]:
                        is_scrutinee_block = True
                        top_cond = pm["cond_stack"][-1]
                        if top_cond[0] != "for_pat":
                            if not top_cond[1]:
                                top_cond[1] = True
                            else:
                                from_cond = pm["cond_stack"].pop()
                    if is_scrutinee_block:
                        pm["scrutinee_kw"] = None
                        new_kind = "other"
                    else:
                        top["pending_matches"].pop()
                        new_kind = "match"
                elif (top["kind"] == "match" and not top["in_arm_body"] and not top["in_guard"]) or top["kind"] == "pattern":
                    new_kind = "pattern"
                elif top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0:
                    if top["arm_turbo_levels"]:
                        new_kind = "other"
                    elif top["arm_expect_head"]:
                        top["arm_block_mode"] = "block"
                        top["arm_expect_head"] = False
                        new_kind = "arm_block"
                    elif top["arm_block_mode"] in ("if_block", "kw_block"):
                        new_kind = "arm_block"
                        if top["arm_after_else"]:
                            top["arm_after_else"] = False
                        elif top["arm_cond_stack"]:
                            top_ac = top["arm_cond_stack"][-1]
                            if top_ac[0] in ("let_pat", "for_pat"):
                                pass
                            elif not top_ac[1]:
                                top_ac[1] = True
                            else:
                                top["arm_cond_stack"].pop()
                    else:
                        new_kind = "other"
                else:
                    new_kind = "other"
                stack.append(_new_frame(new_kind, cur_paren, cur_bracket, from_cond=from_cond))
                cur_paren = 0
                cur_bracket = 0
                cur += 1
                continue
            if c == "}":
                if len(stack) > 1:
                    popped = stack.pop()
                    cur_paren = popped["saved_paren"]
                    cur_bracket = popped["saved_bracket"]
                    top = stack[-1]
                    if popped.get("from_cond") is not None:
                        pm_outer = _active_pm(top, cur_paren, cur_bracket)
                        if pm_outer is not None:
                            la_c = cur + 1
                            while la_c < n and code[la_c].isspace():
                                la_c += 1
                            if la_c < n:
                                restore_cond = False
                                if code[la_c] in ".?+-*/%&^<>|=" or code[la_c:la_c + 2] == "!=":
                                    restore_cond = True
                                elif code[la_c].isalpha() or code[la_c] == "_":
                                    la_cend = la_c + 1
                                    while la_cend < n and (code[la_cend].isalnum() or code[la_cend] == "_"):
                                        la_cend += 1
                                    if code[la_c:la_cend] in ("as", "in"):
                                        restore_cond = True
                                if restore_cond:
                                    pm_outer["cond_stack"].append(popped["from_cond"])
                    if (
                        top["kind"] == "match"
                        and top["in_arm_body"]
                        and cur_paren == 0
                        and cur_bracket == 0
                        and popped["kind"] in ("arm_block", "match")
                        and top["arm_block_mode"] in ("block", "if_block", "kw_block")
                    ):
                        la = cur + 1
                        while la < n and code[la].isspace():
                            la += 1
                        keep_arm = False
                        in_cond_hdr = bool(top["arm_cond_stack"])
                        if la < n:
                            if code[la].isalpha() or code[la] == "_":
                                la_end = la + 1
                                while la_end < n and (code[la_end].isalnum() or code[la_end] == "_"):
                                    la_end += 1
                                la_word = code[la:la_end]
                                if la_word == "else" and (top["arm_block_mode"] == "if_block" or in_cond_hdr):
                                    keep_arm = True
                                    top["arm_after_else"] = True
                                elif la_word == "as" or (la_word == "in" and in_cond_hdr):
                                    keep_arm = True
                            elif code[la] == "{" and (in_cond_hdr or top["arm_after_else"]):
                                keep_arm = True
                            elif (code[la] == "." and (code[la:la + 2] != ".." or in_cond_hdr)) or code[la] == "?":
                                keep_arm = True
                            elif in_cond_hdr and (code[la] in "+-*/%&^<>|=[(" or code[la:la + 2] == "!="):
                                if code[la] != "=" or la + 1 >= n or code[la + 1] != ">":
                                    keep_arm = True
                        if not keep_arm:
                            _reset_arm_state(top, in_body=False)
                else:
                    cur_paren = 0
                    cur_bracket = 0
                cur += 1
                continue
            if c == "=" and cur + 1 < n and code[cur + 1] == ">":
                if top["kind"] == "match" and not top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0:
                    _reset_arm_state(top, in_body=True)
                _clear_pms_at_level(top, cur_paren, cur_bracket)
                cur += 2
                continue
            if c == ",":
                if (
                    top["kind"] == "match"
                    and top["in_arm_body"]
                    and cur_paren == 0
                    and cur_bracket == 0
                    and not top["arm_turbo_levels"]
                    and not top["arm_in_closure_params"]
                ):
                    _reset_arm_state(top, in_body=False)
                if pm is not None:
                    pm["has_scrutinee_tok"] = True
                    pm["scrutinee_kw"] = "op"
                    if pm["cond_stack"]:
                        pm["cond_stack"][-1][1] = True
                cur += 1
                continue
            if c == ";":
                _clear_pms_at_level(top, cur_paren, cur_bracket)
                cur += 1
                continue
            if c == "'":
                # Skip char literals ('a', '\n', '{') in unstripped snippets before checking lifetimes/labels
                ch_end = _skip_char_literal_end(code, cur)
                if ch_end is not None:
                    if pm is not None:
                        pm["has_scrutinee_tok"] = True
                        pm["scrutinee_kw"] = None
                        if pm["cond_stack"]:
                            pm["cond_stack"][-1][1] = True
                    if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0:
                        if top["arm_expect_head"]:
                            top["arm_block_mode"] = "expr"
                            top["arm_expect_head"] = False
                        elif not top["arm_turbo_levels"]:
                            _mark_arm_cond_tok(top, True)
                    cur = ch_end
                    continue
                if cur + 1 < n and (code[cur + 1].isalpha() or code[cur + 1] == "_"):
                    id_end = cur + 2
                    while id_end < n and (code[id_end].isalnum() or code[id_end] == "_"):
                        id_end += 1
                    k = id_end
                    while k < n and code[k].isspace():
                        k += 1
                    if k < n and code[k] == ":" and (k + 1 >= n or code[k + 1] != ":"):
                        if pm is not None:
                            pm["scrutinee_kw"] = "label"
                            if pm["cond_stack"]:
                                pm["cond_stack"][-1][1] = True
                        if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_turbo_levels"]:
                            _mark_arm_cond_tok(top, False)
                        cur = k + 1
                        continue
                    if pm is not None:
                        pm["has_scrutinee_tok"] = True
                        pm["scrutinee_kw"] = None
                        if pm["cond_stack"]:
                            pm["cond_stack"][-1][1] = True
                    if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0:
                        if top["arm_expect_head"]:
                            top["arm_block_mode"] = "expr"
                            top["arm_expect_head"] = False
                        elif not top["arm_turbo_levels"]:
                            _mark_arm_cond_tok(top, True)
                    cur = id_end
                    continue
            if c.isalpha() or c == "_":
                id_end = cur + 1
                while id_end < n and (code[id_end].isalnum() or code[id_end] == "_"):
                    id_end += 1
                word = code[cur:id_end]
                p = cur - 1
                while p >= 0 and code[p].isspace():
                    p -= 1
                is_path_sep = p >= 1 and code[p - 1 : p + 1] == "::"
                is_kw = not (p >= 0 and (code[p] in (".", "#") or is_path_sep))
                if pm is not None:
                    pm["has_scrutinee_tok"] = True
                    if pm["cond_stack"]:
                        pm["cond_stack"][-1][1] = True
                    if is_kw:
                        if word in ("if", "while"):
                            pm["cond_stack"].append([word, False])
                            pm["scrutinee_kw"] = None
                        elif word == "for":
                            pm["cond_stack"].append(["for_pat", False])
                            pm["scrutinee_kw"] = None
                        elif word == "in" and pm["cond_stack"] and pm["cond_stack"][-1][0] == "for_pat":
                            pm["cond_stack"][-1] = ["for_body", False]
                            pm["scrutinee_kw"] = None
                        elif word in ("else", "loop", "unsafe", "async", "try", "const"):
                            pm["scrutinee_kw"] = word
                        elif word == "move" and pm["scrutinee_kw"] == "async":
                            pm["scrutinee_kw"] = "move"
                        else:
                            pm["scrutinee_kw"] = None
                    else:
                        pm["scrutinee_kw"] = None
                if is_kw:
                    if word == "match":
                        top["pending_matches"].append({
                            "paren": cur_paren,
                            "bracket": cur_bracket,
                            "has_scrutinee_tok": False,
                            "cond_stack": [],
                            "scrutinee_kw": None,
                        })
                    if top["kind"] == "match":
                        if not top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and word == "if":
                            top["in_guard"] = True
                        elif top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_turbo_levels"] and not top["arm_in_closure_params"]:
                            if top["arm_expect_head"]:
                                if word == "if":
                                    top["arm_block_mode"] = "if_block"
                                    top["arm_cond_stack"].append(["if_cond", False])
                                elif word == "while":
                                    top["arm_block_mode"] = "kw_block"
                                    top["arm_cond_stack"].append(["while_cond", False])
                                elif word == "for":
                                    top["arm_block_mode"] = "kw_block"
                                    top["arm_cond_stack"].append(["for_pat", False])
                                elif word in ("match", "loop", "unsafe", "async", "try", "const"):
                                    top["arm_block_mode"] = "kw_block"
                                else:
                                    top["arm_block_mode"] = "expr"
                                top["arm_expect_head"] = False
                            elif top["arm_cond_stack"] or top["arm_after_else"]:
                                if word == "if":
                                    _mark_arm_cond_tok(top, True)
                                    top["arm_after_else"] = False
                                    top["arm_cond_stack"].append(["if_cond", False])
                                elif word == "else":
                                    pass
                                elif word == "let" and top["arm_cond_stack"] and top["arm_cond_stack"][-1][0] in ("if_cond", "while_cond", "let_expr"):
                                    top["arm_cond_stack"][-1] = ["let_pat", False]
                                elif word == "in" and top["arm_cond_stack"] and top["arm_cond_stack"][-1][0] == "for_pat":
                                    top["arm_cond_stack"][-1] = ["for_iter", False]
                                elif word in ("loop", "unsafe", "async", "try", "const", "move"):
                                    _mark_arm_cond_tok(top, False)
                                else:
                                    _mark_arm_cond_tok(top, True)
                elif top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_in_closure_params"]:
                    if top["arm_expect_head"]:
                        top["arm_block_mode"] = "expr"
                        top["arm_expect_head"] = False
                    elif not top["arm_turbo_levels"]:
                        _mark_arm_cond_tok(top, True)
                cur = id_end
                continue
            if c == "|":
                if cur + 1 < n and code[cur + 1] in ("|", "="):
                    if pm is not None:
                        pm["has_scrutinee_tok"] = True
                        pm["scrutinee_kw"] = "op"
                        if pm["cond_stack"]:
                            pm["cond_stack"][-1][1] = True
                    if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0:
                        if top["arm_expect_head"]:
                            top["arm_block_mode"] = "expr"
                            top["arm_expect_head"] = False
                        elif not top["arm_turbo_levels"] and not top["arm_in_closure_params"]:
                            _mark_arm_cond_tok(top, False)
                    cur += 2
                    continue
                if (
                    top["kind"] == "match"
                    and (top["in_arm_body"] or top["in_guard"])
                    and cur_paren == 0
                    and cur_bracket == 0
                    and not top["arm_turbo_levels"]
                ):
                    if top["arm_in_closure_params"]:
                        top["arm_in_closure_params"] = False
                        if top["in_arm_body"]:
                            _mark_arm_cond_tok(top, False)
                    elif _is_closure_open_pipe(code, cur, 0):
                        top["arm_in_closure_params"] = True
                        if top["in_arm_body"]:
                            if top["arm_expect_head"]:
                                top["arm_block_mode"] = "expr"
                                top["arm_expect_head"] = False
                            else:
                                _mark_arm_cond_tok(top, False)
                    elif top["in_arm_body"]:
                        if top["arm_expect_head"]:
                            top["arm_block_mode"] = "expr"
                            top["arm_expect_head"] = False
                        else:
                            _mark_arm_cond_tok(top, False)
                if pm is not None:
                    pm["has_scrutinee_tok"] = True
                    pm["scrutinee_kw"] = "op"
                    if pm["cond_stack"]:
                        pm["cond_stack"][-1][1] = True
                cur += 1
                continue
            popped_turbo = False
            if top["kind"] == "match" and top["in_arm_body"]:
                if c == "<":
                    active_g = bool(top["arm_turbo_levels"] and top["arm_turbo_levels"][-1] == (cur_paren, cur_bracket))
                    if _is_generic_angle_open(code, cur, 0, active_generic=active_g):
                        top["arm_turbo_levels"].append((cur_paren, cur_bracket))
                elif c == ">":
                    if top["arm_turbo_levels"] and top["arm_turbo_levels"][-1] == (cur_paren, cur_bracket) and (cur == 0 or code[cur - 1] not in ("-", "=")):
                        top["arm_turbo_levels"].pop()
                        popped_turbo = True
            if pm is not None:
                pm["has_scrutinee_tok"] = True
                pm["scrutinee_kw"] = "op" if c in "+-*/%&|^=<>!" else None
                if pm["cond_stack"]:
                    pm["cond_stack"][-1][1] = True
            if top["kind"] == "match" and top["in_arm_body"] and cur_paren == 0 and cur_bracket == 0 and not top["arm_in_closure_params"]:
                if top["arm_expect_head"]:
                    top["arm_block_mode"] = "expr"
                    top["arm_expect_head"] = False
                elif not top["arm_turbo_levels"]:
                    if popped_turbo or c.isdigit():
                        _mark_arm_cond_tok(top, True)
                    elif c == "=":
                        prev_ch = code[cur - 1] if cur > 0 else ""
                        next_ch = code[cur + 1] if cur + 1 < n else ""
                        if prev_ch not in ("=", "!", "<", ">", ".") and next_ch not in ("=", ">"):
                            if top["arm_cond_stack"] and top["arm_cond_stack"][-1][0] == "let_pat":
                                top["arm_cond_stack"][-1] = ["let_expr", False]
                        else:
                            _mark_arm_cond_tok(top, False)
                    elif c in "+-*/%&|^<>!":
                        _mark_arm_cond_tok(top, False)
            cur += 1

        pat_idx = len(stack) - 1
        while pat_idx > 0 and stack[pat_idx]["kind"] == "pattern":
            pat_idx -= 1
        enclosing = stack[pat_idx]
        if enclosing["kind"] != "match" or enclosing["in_arm_body"] or enclosing["in_guard"]:
            continue

        local_pk = [(stack[idx]["saved_paren"], stack[idx]["saved_bracket"]) for idx in range(pat_idx + 1, len(stack))]
        paren = cur_paren
        bracket = cur_bracket
        guard_turbo: list[tuple[int, int, int]] = []
        guard_in_closure_params = False
        arrow_end = -1
        i = m.end()
        while i < n:
            ch = code[i]
            if ch == "'":
                ch_end = _skip_char_literal_end(code, i)
                if ch_end is not None:
                    i = ch_end
                    continue
            if ch == "|":
                if i + 1 < n and code[i + 1] in ("|", "="):
                    i += 2
                    continue
                if not local_pk and paren == 0 and bracket == 0 and not guard_turbo:
                    if guard_in_closure_params:
                        guard_in_closure_params = False
                    elif _is_closure_open_pipe(code, i, m.start()):
                        guard_in_closure_params = True
                i += 1
                continue
            if ch == "(":
                paren += 1
            elif ch == ")":
                paren = max(0, paren - 1)
                while guard_turbo and guard_turbo[-1] > (len(local_pk), paren, bracket):
                    guard_turbo.pop()
            elif ch == "[":
                bracket += 1
            elif ch == "]":
                bracket = max(0, bracket - 1)
                while guard_turbo and guard_turbo[-1] > (len(local_pk), paren, bracket):
                    guard_turbo.pop()
            elif ch == "{":
                local_pk.append((paren, bracket))
                paren = 0
                bracket = 0
            elif ch == "}":
                if not local_pk:
                    break
                paren, bracket = local_pk.pop()
                while guard_turbo and guard_turbo[-1] > (len(local_pk), paren, bracket):
                    guard_turbo.pop()
            elif ch == "<":
                active_g = bool(guard_turbo and guard_turbo[-1] == (len(local_pk), paren, bracket))
                if _is_generic_angle_open(code, i, m.end(), active_generic=active_g):
                    guard_turbo.append((len(local_pk), paren, bracket))
            elif ch == ">":
                if guard_turbo and guard_turbo[-1] == (len(local_pk), paren, bracket) and (i == 0 or code[i - 1] not in ("-", "=")):
                    guard_turbo.pop()
            elif not local_pk and paren == 0 and bracket == 0:
                if ch in (";", ",") and not guard_turbo and not guard_in_closure_params:
                    break
                if ch == "=" and not guard_turbo:
                    if i + 1 < n and code[i + 1] == ">":
                        arrow_end = i + 2
                        break
                    if not guard_in_closure_params:
                        prev_ch = code[i - 1] if i > 0 else ""
                        next_ch = code[i + 1] if i + 1 < n else ""
                        if prev_ch not in ("=", "!", "<", ">", ".") and next_ch != "=":
                            if not re.search(
                                r"(?:\bif\s+|\&\&\s*)let\b(?:[^=;']|(?<=\.\.)=|'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\])'|'[A-Za-z_](?!'))*$",
                                code[m.end():i],
                            ):
                                break
            i += 1
        if arrow_end < 0:
            continue
        j = arrow_end
        while j < n and code[j].isspace():
            j += 1
        if j >= n:
            continue
        k0 = j
        while k0 < n and code[k0] == "'" and k0 + 1 < n and (code[k0 + 1].isalpha() or code[k0 + 1] == "_"):
            lbl_end = k0 + 2
            while lbl_end < n and (code[lbl_end].isalnum() or code[lbl_end] == "_"):
                lbl_end += 1
            lbl_c = lbl_end
            while lbl_c < n and code[lbl_c].isspace():
                lbl_c += 1
            if lbl_c < n and code[lbl_c] == ":" and (lbl_c + 1 >= n or code[lbl_c + 1] != ":"):
                k0 = lbl_c + 1
                while k0 < n and code[k0].isspace():
                    k0 += 1
            else:
                break
        head_mode = "expr"
        hw = ""
        hw_end = k0
        if k0 < n and code[k0] == "{":
            head_mode = "block"
        elif k0 < n and (code[k0].isalpha() or code[k0] == "_"):
            hw_end = k0 + 1
            while hw_end < n and (code[hw_end].isalnum() or code[hw_end] == "_"):
                hw_end += 1
            hw = code[k0:hw_end]
            if hw == "if":
                head_mode = "if_block"
            elif hw in ("loop", "while", "for", "match", "unsafe", "async", "try", "const"):
                head_mode = "kw_block"

        d_b = d_p = d_k = 0
        after_else = False
        turbo_levels: list[tuple[int, int, int]] = []
        cond_stack: list[list[Any]] = []
        in_closure_params = False
        if head_mode == "if_block":
            cond_stack.append(["if_cond", False])
            k = hw_end
        elif head_mode == "kw_block":
            if hw == "while":
                cond_stack.append(["while_cond", False])
            elif hw == "for":
                cond_stack.append(["for_pat", False])
            elif hw == "match":
                cond_stack.append(["match_scrut", False])
            k = hw_end
        else:
            k = j

        def _mark_local_cond_tok(val: bool) -> None:
            if cond_stack and cond_stack[-1][0] in ("if_cond", "while_cond", "for_iter", "let_expr", "match_scrut"):
                cond_stack[-1][1] = val

        while k < n:
            ch = code[k]
            if ch == "'":
                ch_end = _skip_char_literal_end(code, k)
                if ch_end is not None:
                    if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and not in_closure_params:
                        _mark_local_cond_tok(True)
                    k = ch_end
                    continue
                if k + 1 < n and (code[k + 1].isalpha() or code[k + 1] == "_"):
                    id_end = k + 2
                    while id_end < n and (code[id_end].isalnum() or code[id_end] == "_"):
                        id_end += 1
                    k_lbl = id_end
                    while k_lbl < n and code[k_lbl].isspace():
                        k_lbl += 1
                    if k_lbl < n and code[k_lbl] == ":" and (k_lbl + 1 >= n or code[k_lbl + 1] != ":"):
                        if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and not in_closure_params:
                            _mark_local_cond_tok(False)
                        k = k_lbl + 1
                        continue
                    k = id_end
                    continue
            if ch == "|":
                if k + 1 < n and code[k + 1] in ("|", "="):
                    if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and not in_closure_params:
                        _mark_local_cond_tok(False)
                    k += 2
                    continue
                if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    if in_closure_params:
                        in_closure_params = False
                        _mark_local_cond_tok(False)
                    elif _is_closure_open_pipe(code, k, j):
                        in_closure_params = True
                        _mark_local_cond_tok(False)
                    else:
                        _mark_local_cond_tok(False)
                    k += 1
                    continue
            if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and (ch.isalpha() or ch == "_"):
                id_end = k + 1
                while id_end < n and (code[id_end].isalnum() or code[id_end] == "_"):
                    id_end += 1
                if not in_closure_params:
                    word = code[k:id_end]
                    p = k - 1
                    while p >= j and code[p].isspace():
                        p -= 1
                    is_path_sep = p >= j + 1 and code[p - 1 : p + 1] == "::"
                    is_kw = not (p >= j and (code[p] in (".", "#") or is_path_sep))
                    if is_kw and (cond_stack or after_else):
                        if word == "if":
                            _mark_local_cond_tok(True)
                            after_else = False
                            cond_stack.append(["if_cond", False])
                        elif word == "else":
                            pass
                        elif word == "let" and cond_stack and cond_stack[-1][0] in ("if_cond", "while_cond", "let_expr"):
                            cond_stack[-1] = ["let_pat", False]
                        elif word == "in" and cond_stack and cond_stack[-1][0] == "for_pat":
                            cond_stack[-1] = ["for_iter", False]
                        elif word == "match":
                            _mark_local_cond_tok(True)
                            after_else = False
                            cond_stack.append(["match_scrut", False])
                        elif word in ("loop", "unsafe", "async", "try", "const", "move"):
                            _mark_local_cond_tok(False)
                        else:
                            _mark_local_cond_tok(True)
                    else:
                        _mark_local_cond_tok(True)
                k = id_end
                continue
            if ch == "{":
                if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    if after_else:
                        after_else = False
                    elif cond_stack:
                        top_cs = cond_stack[-1]
                        if top_cs[0] in ("let_pat", "for_pat"):
                            pass
                        elif not top_cs[1]:
                            top_cs[1] = True
                        else:
                            cond_stack.pop()
                d_b += 1
            elif ch == "}":
                if d_b == 0 and d_p == 0 and d_k == 0:
                    break
                d_b -= 1
                while turbo_levels and turbo_levels[-1][0] > d_b:
                    turbo_levels.pop()
                if head_mode != "expr" and d_b == 0 and d_p == 0 and d_k == 0:
                    la = k + 1
                    while la < n and code[la].isspace():
                        la += 1
                    keep_scanning = False
                    in_cond_hdr = bool(cond_stack)
                    if la < n:
                        if turbo_levels:
                            keep_scanning = True
                        elif code[la].isalpha() or code[la] == "_":
                            la_end = la + 1
                            while la_end < n and (code[la_end].isalnum() or code[la_end] == "_"):
                                la_end += 1
                            la_word = code[la:la_end]
                            if la_word == "else" and (head_mode == "if_block" or in_cond_hdr):
                                after_else = True
                                keep_scanning = True
                            elif la_word == "as" or (la_word == "in" and in_cond_hdr):
                                keep_scanning = True
                        elif code[la] == "{" and (in_cond_hdr or after_else):
                            keep_scanning = True
                        elif (code[la] == "." and (code[la:la + 2] != ".." or in_cond_hdr)) or code[la] == "?":
                            keep_scanning = True
                        elif in_cond_hdr and (
                            (code[la] in "+-*/%&^<>|=[(")
                            or code[la:la + 2] == "!="
                        ):
                            if code[la] != "=" or la + 1 >= n or code[la + 1] != ">":
                                keep_scanning = True
                    if not keep_scanning:
                        k += 1
                        break
            elif ch == "(":
                d_p += 1
            elif ch == ")":
                d_p = max(0, d_p - 1)
                while turbo_levels and (turbo_levels[-1][0] > d_b or turbo_levels[-1][1] > d_p or turbo_levels[-1][2] > d_k):
                    turbo_levels.pop()
                if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    _mark_local_cond_tok(True)
            elif ch == "[":
                d_k += 1
            elif ch == "]":
                d_k = max(0, d_k - 1)
                while turbo_levels and (turbo_levels[-1][0] > d_b or turbo_levels[-1][1] > d_p or turbo_levels[-1][2] > d_k):
                    turbo_levels.pop()
                if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    _mark_local_cond_tok(True)
            elif ch == "<":
                active_g = bool(turbo_levels and turbo_levels[-1] == (d_b, d_p, d_k))
                if _is_generic_angle_open(code, k, j, active_generic=active_g):
                    turbo_levels.append((d_b, d_p, d_k))
                elif d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    _mark_local_cond_tok(False)
            elif ch == ">":
                if turbo_levels and turbo_levels[-1] == (d_b, d_p, d_k) and (k == 0 or code[k - 1] not in ("-", "=")):
                    turbo_levels.pop()
                    if d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                        _mark_local_cond_tok(True)
                elif d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels:
                    _mark_local_cond_tok(False)
            elif ch == "," and d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and not in_closure_params:
                break
            elif d_b == 0 and d_p == 0 and d_k == 0 and not turbo_levels and not in_closure_params:
                if ch.isdigit():
                    _mark_local_cond_tok(True)
                elif ch == "=":
                    prev_ch = code[k - 1] if k > 0 else ""
                    next_ch = code[k + 1] if k + 1 < n else ""
                    if prev_ch not in ("=", "!", "<", ">", ".") and next_ch not in ("=", ">"):
                        if cond_stack and cond_stack[-1][0] == "let_pat":
                            cond_stack[-1] = ["let_expr", False]
                    else:
                        _mark_local_cond_tok(False)
                elif ch in "+-*/%&|^!":
                    _mark_local_cond_tok(False)
            k += 1
        arms[(j, k)] = code[j:k].rstrip()
    return list(arms.values())


_EMPTY_MATCH_ARM_BODY_RX = re.compile(
    r"^(?:"
    r"(?:(?:'[A-Za-z_][A-Za-z0-9_]*\s*:(?!:)\s*)+)?\{\s*(?:;|\(\s*\)\s*;?|return\s*(?:\(\s*\)\s*)?;?|continue(?:\s*'[A-Za-z_][A-Za-z0-9_]*)?\s*;?)?\s*\}"
    r"|\(\s*\)"
    r"|return\s*(?:\(\s*\)\s*)?;?"
    r"|continue(?:\s*'[A-Za-z_][A-Za-z0-9_]*)?\s*;?"
    r")$"
)


def _has_empty_match_arm(code: str, variant_rx: str) -> bool:
    return any(
        _EMPTY_MATCH_ARM_BODY_RX.match(body.strip()) is not None
        for body in _extract_match_arm_bodies(code, variant_rx)
    )


def _cli_in_repo_target(ctx: RunContext) -> bool:
    try:
        return ctx.cli_bin.resolve().is_relative_to((ctx.repo_root / "target").resolve())
    except (OSError, ValueError):
        return False


def _cargo_env() -> dict[str, str]:
    cargo_bin = str(Path.home() / ".cargo" / "bin")
    cur_path = os.environ.get("PATH", "")
    new_path = f"{cargo_bin}:{cur_path}" if cargo_bin not in cur_path else cur_path
    return {"PATH": new_path}


_BENCH_PARENS_RX = re.compile(
    r"\([^)]*(?:\b(?:target|budget|sla|limit|gate|baseline|vs|warmup|threshold|ceiling|floor|required)\b|\b\d+\s+ops\s+in\b)[^)]*\)",
    re.I,
)
_BENCH_BRACKETS_RX = re.compile(
    r"\[[^\]]*(?:\b(?:target|budget|sla|limit|gate|baseline|vs|warmup|threshold|ceiling|floor|required)\b|\b\d+\s+ops\s+in\b)[^\]]*\]",
    re.I,
)
_BENCH_TRAILING_CLAUSE_RX = re.compile(
    r"(?:;|,)\s*(?:target|budget|sla|limit|gate|baseline|vs|warmup|threshold|ceiling|floor|required)\b.*",
    re.I,
)
_BENCH_IGNORE_LINE_RX = re.compile(
    r"(?:\bwarming\s+up\b|\bcollecting\b.*\bsamples\b|\banalyzing\b|^\s*change\s*:|\bfound\s+\d+\s+outliers?\b|\b(?:high|low)\s+(?:mild|severe)\b)",
    re.I,
)
_BENCH_AUTOSAVE_TIME_RX = re.compile(
    r"(?<![-.\dA-Za-z_])(\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)\s*(ns|ms|us|µs|μs|s)\b",
    re.I,
)
_BENCH_FPS_RX = re.compile(
    r"(?<![-.\dA-Za-z_])(\d+(?:\.\d+)?(?:[eE][-+]?\d+)?)\s*FPS\b",
    re.I,
)


def _strip_bench_annotations(line: str) -> str:
    s = _BENCH_PARENS_RX.sub("", line)
    s = _BENCH_BRACKETS_RX.sub("", s)
    s = _BENCH_TRAILING_CLAUSE_RX.sub("", s)
    return s


def _extract_bench_autosave_ms(text: str) -> list[float]:
    vals: list[float] = []
    in_autosave_block = False
    for line in text.splitlines():
        if not line.strip():
            in_autosave_block = False
            continue
        if _BENCH_IGNORE_LINE_RX.search(line):
            continue
        has_autosave = bool(re.search(r"autosave", line, re.I))
        is_indented = line[0].isspace()
        if has_autosave:
            in_autosave_block = True
        elif not is_indented:
            in_autosave_block = False
        if not in_autosave_block:
            continue
        if re.match(r"^\s*thrpt\s*:", line, re.I):
            continue
        clean_line = _strip_bench_annotations(line)
        for num_s, unit in _BENCH_AUTOSAVE_TIME_RX.findall(clean_line):
            v = float(num_s)
            u = unit.lower()
            if u == "ns":
                v /= 1_000_000.0
            elif u in ("us", "µs", "μs"):
                v /= 1000.0
            elif u == "s":
                v *= 1000.0
            vals.append(v)
    return vals


def _extract_bench_fps(text: str) -> list[float]:
    vals: list[float] = []
    for line in text.splitlines():
        if not line.strip() or _BENCH_IGNORE_LINE_RX.search(line):
            continue
        clean_line = _strip_bench_annotations(line)
        for num_s in _BENCH_FPS_RX.findall(clean_line):
            vals.append(float(num_s))
    return vals


def _write_coree_zip_from_doc(out_path: Path, title: str, doc: dict[str, Any]) -> None:
    manifest = {
        "format_version": 1,
        "app": "coree",
        "app_version": "0.3.1",
        "document_id": doc["id"],
        "title": title,
        "created_at": _FIXED_TS,
        "modified_at": _FIXED_TS,
        "page_count": len(doc.get("pages", [])),
        "asset_count": len(doc.get("assets", {})),
        "has_preview": False,
    }
    manifest_bytes = json.dumps(manifest, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    doc_bytes = json.dumps(doc, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    with zipfile.ZipFile(out_path, "w", compression=zipfile.ZIP_DEFLATED) as zf:
        for name, payload in (("manifest.json", manifest_bytes), ("document.json", doc_bytes)):
            zi = zipfile.ZipInfo(name, date_time=_ZIP_DT)
            zi.compress_type = zipfile.ZIP_DEFLATED
            zf.writestr(zi, payload)
    del doc_bytes


def _base_doc_template(ctx: RunContext) -> dict[str, Any]:
    tmp_bytes = build_coree_bytes("Tier4 Seed", "Cmyk")
    tmp_zip = ctx.make_scratch_dir("_seed_cache") / "seed.coree"
    tmp_zip.write_bytes(tmp_bytes)
    _, doc = inspect_coree_zip(tmp_zip)
    return doc


def _rect_bezier_path(w: float, h: float) -> dict[str, Any]:
    return {
        "subpaths": [
            {
                "nodes": [
                    {"pos": {"x": x, "y": y}, "handle_in": None, "handle_out": None, "kind": "Cusp"}
                    for (x, y) in ((0.0, 0.0), (w, 0.0), (w, h), (0.0, h))
                ],
                "closed": True,
            }
        ]
    }


def _pick_icc_profile(ctx: RunContext, preferred: tuple[str, ...]) -> Optional[Path]:
    if ctx.icc_dir is None or not ctx.icc_dir.is_dir():
        return None
    all_icc = sorted(ctx.icc_dir.rglob("*.icc"))
    if not all_icc:
        return None
    by_name = {p.name: p for p in all_icc}
    for name in preferred:
        if name in by_name:
            return by_name[name]
    return all_icc[0]


def _check_rss(res: Any, max_kb: int, label: str) -> Optional[str]:
    if res.max_rss_kb is None:
        return f"{label}: /usr/bin/time RSS measurement unavailable (max_rss_kb=None)"
    if res.max_rss_kb > max_kb:
        return f"{label}: peak RSS {res.max_rss_kb} KiB exceeded ceiling {max_kb} KiB"
    return None


def _pt_seg_dist(pt: tuple[float, float], a: tuple[float, float], b: tuple[float, float]) -> float:
    dx, dy = b[0] - a[0], b[1] - a[1]
    l2 = dx * dx + dy * dy
    if l2 <= 1e-12:
        return math.hypot(pt[0] - a[0], pt[1] - a[1])
    t = max(0.0, min(1.0, ((pt[0] - a[0]) * dx + (pt[1] - a[1]) * dy) / l2))
    return math.hypot(pt[0] - (a[0] + t * dx), pt[1] - (a[1] + t * dy))


def _segments_intersect_or_overlap(
    p1: tuple[float, float],
    p2: tuple[float, float],
    p3: tuple[float, float],
    p4: tuple[float, float],
) -> bool:
    def orient(a: tuple[float, float], b: tuple[float, float], c: tuple[float, float]) -> float:
        return (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])

    len12 = math.hypot(p2[0] - p1[0], p2[1] - p1[1])
    len34 = math.hypot(p4[0] - p3[0], p4[1] - p3[1])
    if len12 <= 1e-9 or len34 <= 1e-9:
        return False
    d1 = orient(p3, p4, p1) / len34
    d2 = orient(p3, p4, p2) / len34
    d3 = orient(p1, p2, p3) / len12
    d4 = orient(p1, p2, p4) / len12
    if ((d1 > 1e-4 and d2 < -1e-4) or (d1 < -1e-4 and d2 > 1e-4)) and (
        (d3 > 1e-4 and d4 < -1e-4) or (d3 < -1e-4 and d4 > 1e-4)
    ):
        return True
    if abs(d1) < 1e-4 and abs(d2) < 1e-4 and abs(d3) < 1e-4 and abs(d4) < 1e-4:
        ux, uy = (p2[0] - p1[0]) / len12, (p2[1] - p1[1]) / len12
        t3 = (p3[0] - p1[0]) * ux + (p3[1] - p1[1]) * uy
        t4 = (p4[0] - p1[0]) * ux + (p4[1] - p1[1]) * uy
        if min(len12, max(t3, t4)) - max(0.0, min(t3, t4)) > 1e-4:
            return True
    return False


def _extract_svg_cut_polys(svg_bytes: bytes) -> list[list[tuple[float, float]]]:
    root = ET.fromstring(svg_bytes.decode("utf-8"))
    polys: list[list[tuple[float, float]]] = []
    num_rx = re.compile(r"[-+]?(?:\d*\.\d+|\d+\.?)(?:[eE][-+]?\d+)?")
    cmd_rx = re.compile(r"([MLCHVQSTAZmlchvqstaz])([^MLCHVQSTAZmlchvqstaz]*)")

    def _parse_arc_tuples(rest: str) -> list[tuple[float, float, float, int, int, float, float]]:
        out: list[tuple[float, float, float, int, int, float, float]] = []
        pos = 0
        rn = len(rest)

        def _skip_sep(p: int) -> int:
            while p < rn and (rest[p].isspace() or rest[p] == ","):
                p += 1
            return p

        def _read_num(p: int) -> tuple[Optional[float], int]:
            p = _skip_sep(p)
            m_num = num_rx.match(rest, p)
            if not m_num:
                return None, p
            return float(m_num.group(0)), m_num.end()

        def _read_flag(p: int) -> tuple[Optional[int], int]:
            p = _skip_sep(p)
            if p < rn and rest[p] in ("0", "1"):
                return int(rest[p]), p + 1
            return None, p

        while pos < rn:
            rx_v, pos = _read_num(pos)
            if rx_v is None:
                break
            ry_v, pos = _read_num(pos)
            if ry_v is None:
                break
            rot_v, pos = _read_num(pos)
            if rot_v is None:
                break
            fa_v, pos = _read_flag(pos)
            if fa_v is None:
                break
            fs_v, pos = _read_flag(pos)
            if fs_v is None:
                break
            x_v, pos = _read_num(pos)
            if x_v is None:
                break
            y_v, pos = _read_num(pos)
            if y_v is None:
                break
            out.append((rx_v, ry_v, rot_v, fa_v, fs_v, x_v, y_v))
        return out

    def _arc_pts(
        x1: float,
        y1: float,
        rx: float,
        ry: float,
        rot_deg: float,
        large_arc: int,
        sweep: int,
        x2: float,
        y2: float,
    ) -> list[tuple[float, float]]:
        if math.hypot(x2 - x1, y2 - y1) <= 1e-12:
            return []
        rx = abs(rx)
        ry = abs(ry)
        if rx <= 1e-12 or ry <= 1e-12:
            return [(x2, y2)]

        phi = math.radians(rot_deg % 360.0)
        c_phi = math.cos(phi)
        s_phi = math.sin(phi)
        dx2 = (x1 - x2) * 0.5
        dy2 = (y1 - y2) * 0.5
        x1p = c_phi * dx2 + s_phi * dy2
        y1p = -s_phi * dx2 + c_phi * dy2

        rx2 = rx * rx
        ry2 = ry * ry
        x1p2 = x1p * x1p
        y1p2 = y1p * y1p
        lam = x1p2 / rx2 + y1p2 / ry2
        if lam > 1.0:
            s_lam = math.sqrt(lam)
            rx *= s_lam
            ry *= s_lam
            rx2 = rx * rx
            ry2 = ry * ry

        den = rx2 * y1p2 + ry2 * x1p2
        if den <= 1e-18:
            return [(x2, y2)]
        num = max(0.0, rx2 * ry2 - den)
        factor = math.sqrt(num / den)
        if large_arc == sweep:
            factor = -factor
        cxp = factor * (rx * y1p / ry)
        cyp = factor * (-ry * x1p / rx)

        cx_arc = c_phi * cxp - s_phi * cyp + (x1 + x2) * 0.5
        cy_arc = s_phi * cxp + c_phi * cyp + (y1 + y2) * 0.5

        def _vec_angle(ux: float, uy: float, vx: float, vy: float) -> float:
            len_prod = math.hypot(ux, uy) * math.hypot(vx, vy)
            if len_prod <= 1e-18:
                return 0.0
            ratio = max(-1.0, min(1.0, (ux * vx + uy * vy) / len_prod))
            ang = math.acos(ratio)
            if (ux * vy - uy * vx) < 0.0:
                ang = -ang
            return ang

        px1 = (x1p - cxp) / rx
        py1 = (y1p - cyp) / ry
        px2 = (-x1p - cxp) / rx
        py2 = (-y1p - cyp) / ry
        theta1 = _vec_angle(1.0, 0.0, px1, py1)
        dtheta = _vec_angle(px1, py1, px2, py2)
        if sweep == 0 and dtheta > 0.0:
            dtheta -= 2.0 * math.pi
        elif sweep == 1 and dtheta < 0.0:
            dtheta += 2.0 * math.pi

        steps = max(8, int(math.ceil(abs(dtheta) / (math.pi / 12.0))))
        out: list[tuple[float, float]] = []
        for s in range(1, steps + 1):
            if s == steps:
                out.append((x2, y2))
            else:
                theta = theta1 + dtheta * (s / steps)
                xp = rx * math.cos(theta)
                yp = ry * math.sin(theta)
                out.append((cx_arc + c_phi * xp - s_phi * yp, cy_arc + s_phi * xp + c_phi * yp))
        return out

    def _cubic_pts(
        p0: tuple[float, float],
        p1: tuple[float, float],
        p2: tuple[float, float],
        p3: tuple[float, float],
        steps: int = 10,
    ) -> list[tuple[float, float]]:
        out: list[tuple[float, float]] = []
        for s in range(1, steps + 1):
            t = s / steps
            u = 1.0 - t
            a0 = (u * p0[0] + t * p1[0], u * p0[1] + t * p1[1])
            a1 = (u * p1[0] + t * p2[0], u * p1[1] + t * p2[1])
            a2 = (u * p2[0] + t * p3[0], u * p2[1] + t * p3[1])
            b0 = (u * a0[0] + t * a1[0], u * a0[1] + t * a1[1])
            b1 = (u * a1[0] + t * a2[0], u * a1[1] + t * a2[1])
            out.append((u * b0[0] + t * b1[0], u * b0[1] + t * b1[1]))
        return out

    def _quad_pts(
        p0: tuple[float, float],
        p1: tuple[float, float],
        p2: tuple[float, float],
        steps: int = 10,
    ) -> list[tuple[float, float]]:
        out: list[tuple[float, float]] = []
        for s in range(1, steps + 1):
            t = s / steps
            u = 1.0 - t
            a0 = (u * p0[0] + t * p1[0], u * p0[1] + t * p1[1])
            a1 = (u * p1[0] + t * p2[0], u * p1[1] + t * p2[1])
            out.append((u * a0[0] + t * a1[0], u * a0[1] + t * a1[1]))
        return out

    def parse_path_d(d: str) -> None:
        pts: list[tuple[float, float]] = []
        cx, cy = 0.0, 0.0
        sx, sy = 0.0, 0.0
        last_ctrl_c: tuple[float, float] | None = None
        last_ctrl_q: tuple[float, float] | None = None

        def _append_pt(pt: tuple[float, float]) -> None:
            if not pts or math.hypot(pt[0] - pts[-1][0], pt[1] - pts[-1][1]) > 1e-9:
                pts.append(pt)

        for m in cmd_rx.finditer(d):
            cmd, rest = m.group(1), m.group(2)
            if cmd in ("A", "a"):
                for rx_v, ry_v, rot_v, fa_v, fs_v, x_v, y_v in _parse_arc_tuples(rest):
                    if not pts:
                        pts.append((cx, cy))
                    nx = cx + x_v if cmd == "a" else x_v
                    ny = cy + y_v if cmd == "a" else y_v
                    for pt in _arc_pts(cx, cy, rx_v, ry_v, rot_v, fa_v, fs_v, nx, ny):
                        _append_pt(pt)
                    cx, cy = nx, ny
                last_ctrl_c = None
                last_ctrl_q = None
                continue
            nums = [float(v) for v in num_rx.findall(rest)] if cmd not in ("Z", "z") else []
            if cmd in ("M", "m"):
                if len(pts) >= 3:
                    polys.append(pts)
                pts = []
                for k in range(0, len(nums) - 1, 2):
                    nx = cx + nums[k] if cmd == "m" else nums[k]
                    ny = cy + nums[k + 1] if cmd == "m" else nums[k + 1]
                    cx, cy = nx, ny
                    if k == 0:
                        sx, sy = cx, cy
                    _append_pt((cx, cy))
                last_ctrl_c = None
                last_ctrl_q = None
            elif cmd in ("L", "l"):
                for k in range(0, len(nums) - 1, 2):
                    if not pts:
                        pts.append((cx, cy))
                    nx = cx + nums[k] if cmd == "l" else nums[k]
                    ny = cy + nums[k + 1] if cmd == "l" else nums[k + 1]
                    cx, cy = nx, ny
                    _append_pt((cx, cy))
                last_ctrl_c = None
                last_ctrl_q = None
            elif cmd in ("H", "h"):
                for x in nums:
                    if not pts:
                        pts.append((cx, cy))
                    cx = cx + x if cmd == "h" else x
                    _append_pt((cx, cy))
                last_ctrl_c = None
                last_ctrl_q = None
            elif cmd in ("V", "v"):
                for y in nums:
                    if not pts:
                        pts.append((cx, cy))
                    cy = cy + y if cmd == "v" else y
                    _append_pt((cx, cy))
                last_ctrl_c = None
                last_ctrl_q = None
            elif cmd in ("C", "c"):
                for k in range(0, len(nums) - 5, 6):
                    p0 = (cx, cy)
                    if not pts:
                        pts.append(p0)
                    if cmd == "c":
                        p1 = (cx + nums[k], cy + nums[k + 1])
                        p2 = (cx + nums[k + 2], cy + nums[k + 3])
                        p3 = (cx + nums[k + 4], cy + nums[k + 5])
                    else:
                        p1 = (nums[k], nums[k + 1])
                        p2 = (nums[k + 2], nums[k + 3])
                        p3 = (nums[k + 4], nums[k + 5])
                    for pt in _cubic_pts(p0, p1, p2, p3, steps=10):
                        _append_pt(pt)
                    cx, cy = p3
                    last_ctrl_c = p2
                last_ctrl_q = None
            elif cmd in ("S", "s"):
                for k in range(0, len(nums) - 3, 4):
                    p0 = (cx, cy)
                    if not pts:
                        pts.append(p0)
                    p1 = (2.0 * cx - last_ctrl_c[0], 2.0 * cy - last_ctrl_c[1]) if last_ctrl_c is not None else p0
                    if cmd == "s":
                        p2 = (cx + nums[k], cy + nums[k + 1])
                        p3 = (cx + nums[k + 2], cy + nums[k + 3])
                    else:
                        p2 = (nums[k], nums[k + 1])
                        p3 = (nums[k + 2], nums[k + 3])
                    for pt in _cubic_pts(p0, p1, p2, p3, steps=10):
                        _append_pt(pt)
                    cx, cy = p3
                    last_ctrl_c = p2
                last_ctrl_q = None
            elif cmd in ("Q", "q"):
                for k in range(0, len(nums) - 3, 4):
                    p0 = (cx, cy)
                    if not pts:
                        pts.append(p0)
                    if cmd == "q":
                        p1 = (cx + nums[k], cy + nums[k + 1])
                        p2 = (cx + nums[k + 2], cy + nums[k + 3])
                    else:
                        p1 = (nums[k], nums[k + 1])
                        p2 = (nums[k + 2], nums[k + 3])
                    for pt in _quad_pts(p0, p1, p2, steps=10):
                        _append_pt(pt)
                    cx, cy = p2
                    last_ctrl_q = p1
                last_ctrl_c = None
            elif cmd in ("T", "t"):
                for k in range(0, len(nums) - 1, 2):
                    p0 = (cx, cy)
                    if not pts:
                        pts.append(p0)
                    p1 = (2.0 * cx - last_ctrl_q[0], 2.0 * cy - last_ctrl_q[1]) if last_ctrl_q is not None else p0
                    p2 = (cx + nums[k], cy + nums[k + 1]) if cmd == "t" else (nums[k], nums[k + 1])
                    for pt in _quad_pts(p0, p1, p2, steps=10):
                        _append_pt(pt)
                    cx, cy = p2
                    last_ctrl_q = p1
                last_ctrl_c = None
            elif cmd in ("Z", "z"):
                if len(pts) >= 3:
                    polys.append(pts)
                pts = []
                cx, cy = sx, sy
                last_ctrl_c = None
                last_ctrl_q = None
        if len(pts) >= 3:
            polys.append(pts)

    seen_path_elems: set[int] = set()
    for g in root.iter():
        if g.tag.split("}", 1)[-1] == "g" and g.attrib.get("id", "").upper() == "CUT":
            for p in g.iter():
                if p.tag.split("}", 1)[-1] == "path" and p.attrib.get("d") and id(p) not in seen_path_elems:
                    seen_path_elems.add(id(p))
                    parse_path_d(p.attrib["d"])
    if not polys:
        for el in root.iter():
            if el.tag.split("}", 1)[-1] != "path" or not el.attrib.get("d"):
                continue
            stroke = (el.attrib.get("stroke") or "").lower()
            layer = (el.attrib.get("data-layer") or el.attrib.get("id") or "").lower()
            if "cut" in layer or "#ff0000" in stroke or "#ff00ff" in stroke or stroke in ("red", "magenta") or "cut" in stroke:
                parse_path_d(el.attrib["d"])
    return polys


def _count_poly_self_intersections(polys: list[list[tuple[float, float]]]) -> int:
    def norm_poly(pl: list[tuple[float, float]]) -> list[tuple[float, float]]:
        if len(pl) >= 2 and abs(pl[0][0] - pl[-1][0]) < 1e-6 and abs(pl[0][1] - pl[-1][1]) < 1e-6:
            return pl[:-1]
        return pl

    hits = 0
    cleaned = [norm_poly(p) for p in polys]
    for pts in cleaned:
        n = len(pts)
        if n < 4:
            continue
        cum_arc = [0.0] * (n + 1)
        for i in range(n):
            p_cur, p_nxt = pts[i], pts[(i + 1) % n]
            cum_arc[i + 1] = cum_arc[i] + math.hypot(p_nxt[0] - p_cur[0], p_nxt[1] - p_cur[1])
        total_arc = cum_arc[n]
        for i in range(n):
            p1, p2 = pts[i], pts[(i + 1) % n]
            for j in range(i + 2, n):
                if i == 0 and j == n - 1:
                    continue
                p3, p4 = pts[j], pts[(j + 1) % n]
                if _segments_intersect_or_overlap(p1, p2, p3, p4):
                    hits += 1
                elif j > i + 2 and not (i == 0 and j == n - 2) and math.hypot(p1[0] - p3[0], p1[1] - p3[1]) < 1e-7:
                    fwd_arc = cum_arc[j] - cum_arc[i]
                    bwd_arc = total_arc - fwd_arc
                    if fwd_arc > 1e-2 and bwd_arc > 1e-2:
                        hits += 1
        for i in range(n):
            a, b, c = pts[i], pts[(i + 1) % n], pts[(i + 2) % n]
            ux, uy = b[0] - a[0], b[1] - a[1]
            vx, vy = c[0] - b[0], c[1] - b[1]
            lu, lv = math.hypot(ux, uy), math.hypot(vx, vy)
            if lu > 1e-9 and lv > 1e-9:
                if (ux * vx + uy * vy) < 0.0 and min(_pt_seg_dist(a, b, c), _pt_seg_dist(c, a, b)) < 1e-7:
                    hits += 1
        # Intra-polygon vertex-on-non-adjacent-edge interior T-junction detection
        for i in range(n):
            a, b = pts[i], pts[(i + 1) % n]
            i_next = (i + 1) % n
            skip_k = {i, i_next, (i - 1) % n, (i + 2) % n}
            for k in range(n):
                if k in skip_k:
                    continue
                pk = pts[k]
                if (
                    math.hypot(pk[0] - a[0], pk[1] - a[1]) >= 1e-4
                    and math.hypot(pk[0] - b[0], pk[1] - b[1]) >= 1e-4
                    and _pt_seg_dist(pk, a, b) < 1e-4
                ):
                    hits += 1
    for ia in range(len(cleaned)):
        pa = cleaned[ia]
        for ib in range(ia + 1, len(cleaned)):
            pb = cleaned[ib]
            na, nb = len(pa), len(pb)
            if na < 2 or nb < 2:
                continue
            for i in range(na):
                p1, p2 = pa[i], pa[(i + 1) % na]
                for j in range(nb):
                    p3, p4 = pb[j], pb[(j + 1) % nb]
                    if (
                        _pt_seg_dist(p1, p3, p4) < 1e-4
                        or _pt_seg_dist(p3, p1, p2) < 1e-4
                        or _segments_intersect_or_overlap(p1, p2, p3, p4)
                    ):
                        hits += 1
    return hits


def _stream_write_16bit_multilayer_psd(out_path: Path, width: int = 4096, height: int = 4096) -> int:
    """Stream-write a valid 16-bit 2-layer RGBA + Layer Mask (-2) + Spot Channel PSD in < 2 MiB Python RAM."""
    channels = 5  # R, G, B, Alpha, SpotCut
    depth = 16
    color_mode = 3  # RGB
    row_bytes = width * 2
    chan_pixel_bytes = height * row_bytes
    chan_block_bytes = 2 + chan_pixel_bytes  # 2B compression(=0) + raw rows

    # Image Resources: 1005 (ResolutionInfo 300 DPI) + 1007 (AlphaChannelsNames "SpotCut")
    res_1005 = (
        b"8BIM"
        + struct.pack(">HBB", 1005, 0, 0)
        + struct.pack(">I", 16)
        + struct.pack(">IHHIHH", 300 << 16, 1, 2, 300 << 16, 1, 2)
    )
    res_1007 = (
        b"8BIM"
        + struct.pack(">HBB", 1007, 0, 0)
        + struct.pack(">I", 8)
        + b"\x07SpotCut"
    )
    img_resources = res_1005 + res_1007

    layer_specs = (
        (b"BaseLayer", (0, 1, 2, -1), False),
        (b"HighlightLayer", (0, 1, 2, -1, -2), True),
    )
    layer_records = bytearray()
    total_layer_channels = 0
    for lname, ch_ids, has_mask in layer_specs:
        pascal = bytes([len(lname)]) + lname
        pad_len = (4 - (len(pascal) % 4)) % 4
        pascal_padded = pascal + (b"\x00" * pad_len)
        lmask_block = (
            struct.pack(">IiiiiBBH", 20, 0, 0, height, width, 255, 0, 0)
            if has_mask
            else struct.pack(">I", 0)
        )
        lranges_block = struct.pack(">I", 0)
        extra_data = lmask_block + lranges_block + pascal_padded
        rec = bytearray()
        rec.extend(struct.pack(">iiiiH", 0, 0, height, width, len(ch_ids)))
        for ch_id in ch_ids:
            rec.extend(struct.pack(">hI", ch_id, chan_block_bytes))
            total_layer_channels += 1
        rec.extend(b"8BIMnorm")
        rec.extend(struct.pack(">BBBBI", 255, 0, 0, 0, len(extra_data)))
        rec.extend(extra_data)
        layer_records.extend(rec)

    all_layers_channel_bytes = total_layer_channels * chan_block_bytes
    layer_info_len = 2 + len(layer_records) + all_layers_channel_bytes
    if layer_info_len % 2 != 0:
        layer_info_len += 1
    layer_mask_len = 4 + layer_info_len + 4

    row_r = struct.pack(f">{width}H", *(((x * 16) & 0xFFFF) for x in range(width)))
    row_g = struct.pack(f">{width}H", *(((65535 - x * 12) & 0xFFFF) for x in range(width)))
    row_b = struct.pack(f">{width}H", *((((x * 7) + 12000) & 0xFFFF) for x in range(width)))
    row_a = struct.pack(f">{width}H", *([65535] * width))
    row_mask = struct.pack(f">{width}H", *([(65535 if x < width // 2 else 16384) for x in range(width)]))
    row_spot = struct.pack(f">{width}H", *([(48000 if (x // 4) % 2 == 0 else 0) for x in range(width)]))
    ch_row_map = {
        0: row_r,
        1: row_g,
        2: row_b,
        -1: row_a,
        -2: row_mask,
    }
    comp_patterns = (row_r, row_g, row_b, row_a, row_spot)

    with out_path.open("wb") as fh:
        # 1. File Header (26B)
        fh.write(struct.pack(">4sH6sHIIHH", b"8BPS", 1, b"\x00" * 6, channels, height, width, depth, color_mode))
        # 2. Color Mode Data (0B) + Image Resources
        fh.write(struct.pack(">II", 0, len(img_resources)))
        fh.write(img_resources)
        # 3. Layer and Mask Information Section
        fh.write(struct.pack(">I", layer_mask_len))
        fh.write(struct.pack(">I", layer_info_len))
        fh.write(struct.pack(">h", len(layer_specs)))
        fh.write(layer_records)
        comp_raw = struct.pack(">H", 0)
        for _lname, ch_ids, _has_mask in layer_specs:
            for ch_id in ch_ids:
                fh.write(comp_raw)
                pat = ch_row_map[ch_id]
                for _y in range(height):
                    fh.write(pat)
        if (2 + len(layer_records) + all_layers_channel_bytes) % 2 != 0:
            fh.write(b"\x00")
        fh.write(struct.pack(">I", 0))  # Global layer mask info len = 0
        # 4. Composite Image Data (compression=0, 5 planar channels: R, G, B, A, SpotCut)
        fh.write(comp_raw)
        for pat in comp_patterns:
            for _y in range(height):
                fh.write(pat)

    return out_path.stat().st_size


def _inspect_psd_header_and_layers(psd_path: Path) -> dict[str, Any]:
    """Bounded stream-seek PSD header & layer record inspector (safe on >300 MiB PSD under RLIMIT_AS=512 MiB)."""
    with psd_path.open("rb") as fh:
        hdr = fh.read(26)
        if len(hdr) < 26 or hdr[:4] != b"8BPS":
            raise ValueError("Invalid PSD magic (expected 8BPS)")
        version, _resv, channels, height, width, depth, color_mode = struct.unpack(">H6sHIIHH", hdr[4:26])
        if version not in (1, 2):
            raise ValueError(f"Invalid PSD version {version}")
        cmd_len = struct.unpack(">I", fh.read(4))[0]
        fh.seek(cmd_len, os.SEEK_CUR)
        res_len = struct.unpack(">I", fh.read(4))[0]
        fh.seek(res_len, os.SEEK_CUR)
        len_fmt = ">Q" if version == 2 else ">I"
        len_sz = 8 if version == 2 else 4
        lm_raw = fh.read(len_sz)
        if len(lm_raw) < len_sz:
            raise ValueError("Truncated PSD layer/mask length")
        layer_mask_len = struct.unpack(len_fmt, lm_raw)[0]
        layer_count = 0
        has_layer_mask_channel = False
        has_nonempty_lmask = False
        if layer_mask_len >= len_sz + 2:
            li_len = struct.unpack(len_fmt, fh.read(len_sz))[0]
            if li_len >= 2:
                raw_lc = struct.unpack(">h", fh.read(2))[0]
                layer_count = abs(raw_lc)
                ch_ref_sz = 10 if version == 2 else 6
                ch_ref_fmt = ">hQ" if version == 2 else ">hI"
                for _ in range(layer_count):
                    lrec = fh.read(18)
                    if len(lrec) < 18:
                        raise ValueError("Truncated PSD layer record")
                    num_ch = struct.unpack(">H", lrec[16:18])[0]
                    for _ in range(num_ch):
                        ch_id, _ch_len = struct.unpack(ch_ref_fmt, fh.read(ch_ref_sz))
                        if ch_id in (-2, -3):
                            has_layer_mask_channel = True
                    blend_extra = fh.read(16)
                    if len(blend_extra) < 16 or blend_extra[:4] != b"8BIM":
                        raise ValueError("Invalid PSD layer blend signature")
                    extra_len = struct.unpack(">I", blend_extra[12:16])[0]
                    extra_pos = fh.tell()
                    if extra_len >= 4:
                        lmask_len = struct.unpack(">I", fh.read(4))[0]
                        if lmask_len > 0:
                            has_nonempty_lmask = True
                    fh.seek(extra_pos + extra_len, os.SEEK_SET)
    return {
        "version": version,
        "channels": channels,
        "width": width,
        "height": height,
        "depth": depth,
        "color_mode": color_mode,
        "image_resources_len": res_len,
        "layer_mask_len": layer_mask_len,
        "layer_count": layer_count,
        "has_layer_mask_channel": has_layer_mask_channel,
        "has_nonempty_lmask": has_nonempty_lmask,
    }


def _build_cp1256_arabic_cdr_bytes() -> bytes:
    """Build a legacy RIFF CDR v9 file with explicit + heuristic Windows-1256 (CP1256) Arabic text."""
    def riff_chunk(fourcc: bytes, payload: bytes) -> bytes:
        pad = b"\x00" if (len(payload) & 1) else b""
        return fourcc + struct.pack("<I", len(payload)) + payload + pad

    def riff_list(list_type: bytes, children: list[bytes]) -> bytes:
        body = list_type + b"".join(children)
        pad = b"\x00" if (len(body) & 1) else b""
        return b"LIST" + struct.pack("<I", len(body)) + body + pad

    arabic_explicit = "تصميم عربي كلاسيكي"
    arabic_auto = "مرحبا بالعالم العربي"
    bytes_explicit = arabic_explicit.encode("cp1256")
    bytes_auto = arabic_auto.encode("cp1256")

    font_name = b"Traditional Arabic"
    stlt_payload = (
        struct.pack("<IHBBBB", 1, 1256, 178, 0, 2, 2)
        + struct.pack("<dddfH", 28.0, 0.0, 1.2, 0.0, len(font_name))
        + font_name
        + struct.pack("<HH4B", 5, 4, 0, 0, 0, 0)
    )
    txsm_explicit = (
        bytes([0x01])
        + struct.pack("<IHBBiiI", 1, 1256, 178, 0, 0, 0, len(bytes_explicit))
        + bytes_explicit
    )
    txsm_auto = (
        bytes([0x01])
        + struct.pack("<IHBBiiI", 0, 0, 0, 0, 0, 0, len(bytes_auto))
        + bytes_auto
    )

    doc_list = riff_list(
        b"doc ",
        [
            riff_list(b"stlt", [riff_chunk(b"stlt", stlt_payload)]),
            riff_list(
                b"page",
                [
                    riff_list(
                        b"layr",
                        [
                            riff_list(b"obj ", [riff_chunk(b"txsm", txsm_explicit)]),
                            riff_list(b"obj ", [riff_chunk(b"txsm", txsm_auto)]),
                        ],
                    )
                ],
            ),
        ],
    )
    inner = b"CDR9" + riff_chunk(b"vrsn", struct.pack("<H", 900)) + doc_list
    return b"RIFF" + struct.pack("<I", len(inner)) + inner


# ---------------------------------------------------------------------------
# Workload 1: Commercial CMYK Brochure + Bilingual Arabic/English PDF/X-1a
# ---------------------------------------------------------------------------

def _test_t4_brochure_pdfx(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-BROCHURE-PDFX")
    doc = _base_doc_template(ctx)
    icc_path = _pick_icc_profile(ctx, ("FOGRA39L_coated.icc", "SWOP_TR003_coated_3.icc", "default_cmyk.icc"))
    if icc_path is None:
        return TestOutcome("FAIL", "W4 REQ-03/REQ-02: No system CMYK .icc profile found under /usr/share/color/icc/")
    doc["custom_icc_profile"] = base64.b64encode(icc_path.read_bytes()).decode("ascii")
    doc["custom_icc_name"] = icc_path.name

    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    magenta = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    yellow = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 1.0, "k": 0.0, "a": 1.0}}
    black = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}

    doc["objects"] = {
        "10": {
            "id": 10,
            "name": "Hero Gradient Banner",
            "payload": {"Shape": {"Rectangle": {"width": 170.0, "height": 55.0, "corner_radii": [4.0, 4.0, 4.0, 4.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": {"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 170.0, "y": 55.0}, "stops": [{"offset": 0.0, "color": cyan}, {"offset": 1.0, "color": magenta}]}},
            "outline": {**base_outline, "color": black, "width_mm": 0.3},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "11": {
            "id": 11,
            "name": "Seal Radial",
            "payload": {"Shape": {"Ellipse": {"rx": 22.0, "ry": 22.0, "mode": "Full", "start_deg": 0.0, "end_deg": 360.0}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 160.0, 120.0]},
            "fill": {"RadialGradient": {"center": {"x": 0.0, "y": 0.0}, "radius": 22.0, "focal": {"x": 0.0, "y": 0.0}, "stops": [{"offset": 0.0, "color": yellow}, {"offset": 1.0, "color": magenta}]}},
            "outline": {**base_outline, "width_mm": 0.25},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "12": {
            "id": 12,
            "name": "Bilingual Headline",
            "payload": {
                "ArtisticText": {
                    "runs": [
                        {"text": "coree Commercial Brochure 2026 ", "style": {**base_style, "font_family": "Noto Sans", "font_size_pt": 22.0}},
                        {"text": "تصميم احترافي", "style": {**base_style, "font_family": "Noto Naskh Arabic", "font_size_pt": 22.0, "direction": "Rtl", "align": "ForceJustifyKashida", "kashida_stretch": 0.6}},
                    ],
                    "direction": "Auto",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 95.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "13": {
            "id": 13,
            "name": "Body Columns",
            "payload": {
                "ParagraphText": {
                    "runs": [{"text": "Bilingual CMYK editorial layout for offset press.", "style": {**base_style, "font_size_pt": 12.0}}],
                    "frame_size": {"x": 110.0, "y": 60.0},
                    "columns": 2,
                    "column_gutter_mm": 6.0,
                    "linked_next_frame": None,
                    "wrap_mode": "None",
                    "drop_cap": None,
                    "direction": "Ltr",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 130.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "20": {
            "id": 20,
            "name": "Page2 Card",
            "payload": {"Shape": {"Rectangle": {"width": 140.0, "height": 80.0, "corner_radii": [2.0, 2.0, 2.0, 2.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 35.0, 40.0]},
            "fill": {"Solid": cyan}, "outline": {**base_outline, "width_mm": 0.3},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "21": {
            "id": 21,
            "name": "Page2 Subtitle",
            "payload": {
                "ArtisticText": {
                    "runs": [{"text": "Second Page الصفحة الثانية", "style": {**base_style, "font_size_pt": 18.0}}],
                    "direction": "Auto",
                    "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 35.0, 140.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
    }
    doc["pages"] = [
        {
            "id": 1, "name": "Brochure P1", "width_mm": 210.0, "height_mm": 297.0, "bleed_mm": 3.0, "facing_pages": False,
            "layers": [{"id": 2, "name": "Artwork P1", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [10, 11, 12, 13]}],
        },
        {
            "id": 3, "name": "Brochure P2", "width_mm": 210.0, "height_mm": 297.0, "bleed_mm": 3.0, "facing_pages": False,
            "layers": [{"id": 4, "name": "Artwork P2", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [20, 21]}],
        },
    ]
    doc["next_id"] = 22

    in_coree = work / "brochure.coree"
    out_pdf = work / "brochure_pdfx.pdf"
    _write_coree_zip_from_doc(in_coree, "Brochure PDFX", doc)

    r_pdf = run_cmd(
        [str(ctx.cli_bin), "convert", str(in_coree), str(out_pdf), "--cmyk", "--crop-marks"],
        cwd=work, timeout=40.0, timeout_scale=ctx.timeout_scale, measure_rss=True,
    )
    if r_pdf.returncode != 0 or not out_pdf.is_file():
        return TestOutcome("FAIL", f"W4 REQ-03: Brochure PDF export failed: {r_pdf.stderr}")
    rss_err = _check_rss(r_pdf, 524_288, "T4-W4-BROCHURE-PDFX convert")
    if rss_err:
        return TestOutcome("FAIL", rss_err)

    failures: list[str] = []
    try:
        pdf_s = parse_pdf_structure(out_pdf.read_bytes())
        if not (pdf_s["has_pdfx_version"] and pdf_s["has_output_intents"] and pdf_s["has_trapped"] and pdf_s["has_device_cmyk"]):
            failures.append(f"W4 REQ-03: Brochure PDF missing PDF/X-1a/4 markers (/GTS_PDFXVersion, /OutputIntents, /Trapped, /DeviceCMYK): {pdf_s}")
    except _PARSE_EXCS as exc:
        failures.append(f"W4 REQ-03: Exported brochure PDF malformed: {exc}")

    pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
    fields = pinfo.get("fields", {})
    if (
        not pinfo["ok"]
        or fields.get("Pages") != "2"
        or "MediaBox" not in fields
        or "TrimBox" not in fields
        or "BleedBox" not in fields
        or fields["TrimBox"] == fields["MediaBox"]
    ):
        failures.append(f"W4 REQ-03: pdfinfo -box invalid for 2p crop-marks brochure: {fields}")

    fonts_rep = run_poppler_fonts(out_pdf, timeout_scale=ctx.timeout_scale)
    cid_types = {"CID TrueType", "CID TrueType (OT)", "Type 0", "CID Type 0", "CID Type 0 (OT)", "CID Type 0C", "CID Type 0C (OT)", "TrueType (CID)"}
    if not fonts_rep["ok"] or not any(
        f["type"] in cid_types and f["emb"] == "yes" and f["sub"] == "yes" and f["uni"] == "yes"
        for f in fonts_rep.get("fonts", [])
    ):
        failures.append(f"W4 REQ-03: pdffonts lacks subsetted /Type0+/CIDFontType2+/ToUnicode row: {fonts_rep}")

    txt_rep = run_poppler_text(out_pdf, timeout_scale=ctx.timeout_scale)
    extracted = _strip_bidi_controls(txt_rep.get("text", ""))
    missing_txt = [
        tok
        for tok in (
            "coree Commercial Brochure 2026",
            "تصميم احترافي",
            "Bilingual CMYK editorial layout",
            "Second Page",
            "الصفحة الثانية",
        )
        if tok not in extracted
    ]
    if not txt_rep["ok"] or missing_txt:
        failures.append(f"W4 REQ-03: pdftotext missing bilingual P1+P2/ParagraphText tokens {missing_txt}: {extracted[:300]!r}")

    gs_res = run_ghostscript_render(out_pdf, work / "gs_brochure.png", dpi=96, device="png16m", timeout_scale=ctx.timeout_scale)
    if not gs_res["ok"] or (gs_res.get("png_info") or {}).get("non_white_pixels", 0) < 100:
        failures.append(f"W4 REQ-03: Ghostscript png16m failed on brochure PDF: {gs_res}")

    cli_p1 = work / "cli_p1.png"
    cli_p2 = work / "cli_p2.png"
    r_ren1 = run_cmd([str(ctx.cli_bin), "render", str(in_coree), str(cli_p1), "--page", "0", "--dpi", "96"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    r_ren2 = run_cmd([str(ctx.cli_bin), "render", str(in_coree), str(cli_p2), "--page", "1", "--dpi", "96"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if r_ren1.returncode != 0 or not cli_p1.is_file():
        failures.append(f"W4 REQ-03: Brochure CLI render page 0 failed: {r_ren1.stderr}")
    if r_ren2.returncode != 0 or not cli_p2.is_file():
        failures.append(f"W4 REQ-03: Brochure CLI render page 1 failed: {r_ren2.stderr}")

    ppm_res = run_pdftoppm(out_pdf, work / "ppm_page", dpi=96, single_file=False, timeout_scale=ctx.timeout_scale)
    if not ppm_res["ok"] or len(ppm_res.get("pages", [])) != 2:
        failures.append(f"W4 REQ-03: pdftoppm expected 2 pages: {ppm_res}")

    p1_diff: dict[str, Any] = {}
    if cli_p1.is_file() and cli_p2.is_file() and len(ppm_res.get("pages", [])) == 2:
        for pg_idx, cli_png, ppm_src in (
            (0, cli_p1, Path(ppm_res["pages"][0])),
            (1, cli_p2, Path(ppm_res["pages"][1])),
        ):
            try:
                cli_info = parse_png_info(cli_png.read_bytes())
                if cli_info["non_white_pixels"] < 100:
                    failures.append(f"W4 REQ-03: Brochure CLI page {pg_idx} rendered blank: {cli_info}")
                    continue
                ppm_cropped = work / f"ppm_p{pg_idx}_trim.png"
                crop_png_center(ppm_src, cli_info["width"], cli_info["height"], ppm_cropped)
                diff = compare_png_pixels(cli_png, ppm_cropped)
                blank_png = work / f"blank_p{pg_idx}.png"
                build_blank_white_png(cli_info["width"], cli_info["height"], blank_png)
                blank_diff = compare_png_pixels(cli_png, blank_png)
                if pg_idx == 0:
                    p1_diff = diff
                if diff["mean_abs_diff"] >= 8.0 or diff["non_white_iou"] <= 0.85 or diff["mean_abs_diff"] >= blank_diff["mean_abs_diff"]:
                    failures.append(f"W4 REQ-03: Brochure page {pg_idx} pdftoppm TrimBox vs CLI render diverged: diff={diff}, blank={blank_diff}")
            except _PARSE_EXCS as exc:
                failures.append(f"W4 REQ-03: PNG oracle comparison failed on page {pg_idx}: {exc}")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"max_rss_kb": r_pdf.max_rss_kb})

    return TestOutcome(
        "PASS",
        f"Commercial 2p CMYK PDF/X brochure + CIDFontType2 + Arabic/Latin P1/P2 pdftotext + TrimBox pixel oracle (diff={p1_diff.get('mean_abs_diff', 0.0):.2f}, iou={p1_diff.get('non_white_iou', 0.0):.3f}, rss={r_pdf.max_rss_kb} KiB) verified",
        metrics={"max_rss_kb": r_pdf.max_rss_kb, "mean_abs_diff": p1_diff.get("mean_abs_diff"), "non_white_iou": p1_diff.get("non_white_iou")},
    )


# ---------------------------------------------------------------------------
# Workload 2: Die-Cut Packaging Box (30+ FEFCO + 2D/GS1 + Spot Separations)
# ---------------------------------------------------------------------------

def _test_t4_packaging_fefco(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-PACKAGING-FEFCO")
    failures: list[str] = []

    # 1. Runtime packaging workload FIRST (preflight + laser DXF + CMYK/Spot PDF + gs)
    doc = _base_doc_template(ctx)
    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    cut_spot = {"Spot": {"name": "CutContour", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.0, 0.0], "alpha": 1.0}}
    crease_spot = {"Spot": {"name": "Crease", "tint": 1.0, "fallback_cmyk": [1.0, 0.0, 1.0, 0.0], "alpha": 1.0}}
    cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}

    lowres_uuid = "66666666-6666-4666-8666-666666666666"
    lowres_rgba = bytes([20, 140, 210, 255] * (16 * 16))
    doc["assets"] = {
        lowres_uuid: {
            "id": lowres_uuid, "width": 16, "height": 16,
            "rgba_bytes": base64.b64encode(lowres_rgba).decode("ascii"),
            "source_dpi": 72.0, "color_mode": "Cmyk",
        }
    }
    doc["objects"] = {
        "3": {
            "id": 3, "name": "Panel Artwork",
            "payload": {"Shape": {"Rectangle": {"width": 120.0, "height": 90.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 25.0, 25.0]},
            "fill": {"Solid": cyan}, "outline": {**base_outline, "width_mm": 0.3},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "4": {
            "id": 4, "name": "LowRes Label Asset",
            "payload": {"Raster": {"asset_id": lowres_uuid, "width_mm": 100.0, "height_mm": 60.0, "filters": [], "crop_rect": None}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 35.0, 35.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "5": {
            "id": 5, "name": "Missing Font SKU",
            "payload": {"ArtisticText": {"runs": [{"text": "GS1-128 BOX-0201", "style": {**base_style, "font_family": "NonExistentPkgFont_404", "font_size_pt": 14.0}}], "direction": "Ltr", "align": "Left"}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 105.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "6": {
            "id": 6, "name": "FEFCO CutContour",
            "payload": {"Shape": {"Rectangle": {"width": 126.0, "height": 96.0, "corner_radii": [2.0, 2.0, 2.0, 2.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 22.0, 22.0]},
            "fill": "None", "outline": {**base_outline, "color": cut_spot, "width_mm": 0.0762, "is_hairline": True},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "7": {
            "id": 7, "name": "FEFCO Crease Fold",
            "payload": {"Shape": {"Rectangle": {"width": 116.0, "height": 86.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 27.0, 27.0]},
            "fill": "None", "outline": {**base_outline, "color": crease_spot, "width_mm": 0.0762, "is_hairline": True, "dash_pattern": [2.0, 2.0]},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
    }
    doc["pages"][0]["layers"] = [
        {"id": 2, "name": "Graphics", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [3, 4, 5]},
        {"id": 8, "name": "CutContour", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": True, "is_desktop_layer": False, "objects": [6, 7]},
    ]
    doc["next_id"] = 9

    pkg_in = work / "packaging.coree"
    _write_coree_zip_from_doc(pkg_in, "Packaging FEFCO", doc)

    r_pf = run_cmd([str(ctx.cli_bin), "preflight", str(pkg_in), "--json"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_pf.returncode != 0:
        failures.append(f"W4 REQ-07: packaging preflight --json failed: {r_pf.stderr}")
    else:
        rss_err = _check_rss(r_pf, 524_288, "T4-W4-PACKAGING-FEFCO preflight")
        if rss_err:
            failures.append(rss_err)
        try:
            pf_json = json.loads(r_pf.stdout)
            issues = pf_json.get("issues", []) if isinstance(pf_json, dict) else []
            has_lowres_id4 = any(isinstance(i, dict) and i.get("kind") == "LowResolutionImage" and i.get("object_id") == 4 for i in issues)
            has_missfont_id5 = any(isinstance(i, dict) and i.get("kind") == "MissingFont" and i.get("object_id") == 5 for i in issues)
            if not has_lowres_id4 or not has_missfont_id5:
                failures.append(f"W4 REQ-07: preflight missing LowResolutionImage(id=4) or MissingFont(id=5): {issues[:8]}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REQ-07: preflight --json invalid JSON: {exc}")

    pkg_dxf = work / "packaging.dxf"
    r_dxf = run_cmd([str(ctx.cli_bin), "laser", str(pkg_in), str(pkg_dxf), "--format", "dxf"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if r_dxf.returncode != 0 or not pkg_dxf.is_file():
        failures.append(f"W4 ADD-05: packaging laser --format dxf failed: {r_dxf.stderr}")
    else:
        try:
            dxf_s = parse_dxf_structure(pkg_dxf.read_bytes())
            if "ENTITIES" not in dxf_s["sections"] or not dxf_s["has_eof"] or not dxf_s["entities"]:
                failures.append(f"W4 ADD-05: packaging DXF missing ENTITIES/EOF: {dxf_s}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 ADD-05: packaging DXF malformed: {exc}")

    pkg_pdf = work / "packaging.pdf"
    r_pdf = run_cmd([str(ctx.cli_bin), "convert", str(pkg_in), str(pkg_pdf), "--cmyk"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_pdf.returncode != 0 or not pkg_pdf.is_file():
        failures.append(f"W4 REQ-05: packaging CMYK PDF export failed: {r_pdf.stderr}")
    else:
        try:
            pdf_raw = pkg_pdf.read_bytes()
            pdf_s = parse_pdf_structure(pdf_raw)
            if not pdf_s["has_device_cmyk"] or not pdf_s["has_separation"]:
                failures.append(f"W4 REQ-05: packaging PDF missing CMYK/Spot /Separation channels: {pdf_s}")
            if b"/CutContour" not in pdf_raw or b"/Crease" not in pdf_raw:
                failures.append("W4 REQ-05: packaging PDF missing /CutContour and /Crease Spot color separation definitions")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REQ-05: packaging PDF malformed: {exc}")
        gs_res = run_ghostscript_render(pkg_pdf, work / "gs_pkg.png", dpi=96, device="png16m", timeout_scale=ctx.timeout_scale)
        if not gs_res["ok"] or (gs_res.get("png_info") or {}).get("non_white_pixels", 0) < 100:
            failures.append(f"W4 REQ-05: Ghostscript failed to render packaging PDF: {gs_res}")

    # 2. Verify W4 ADD-05 & REQ-05 non-stub implementations (comment-stripped)
    cut_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/cut_packaging.rs"))
    cut_no_comments = _strip_cfg_test_modules(_read_rust_comments_only(ctx, "crates/coree-prod/src/cut_packaging.rs"))
    fefco_enum_counts = [
        _count_enum_variants(blk)
        for rx in (r"\bpub\s+enum\s+DielineKind\s*\{", r"\bpub\s+enum\s+Fefco[A-Za-z0-9_]*\s*\{", r"\benum\s+Fefco[A-Za-z0-9_]*\s*\{")
        for blk in [_extract_brace_block(cut_code, rx)]
        if blk
    ]
    fefco_ident_codes = set(re.findall(r"\b(?:Fefco_?|F)(0\d{3})\b", cut_code))
    fefco_str_codes = set(re.findall(r'"(?:FEFCO[\s_-]*)?(0\d{3})"', cut_no_comments, re.I))
    fefco_count = max(fefco_enum_counts + [len(fefco_ident_codes), len(fefco_str_codes), 0])
    if fefco_count < 30:
        failures.append(f"W4 ADD-05: cut_packaging.rs implements {fefco_count} FEFCO codes (< 30 required)")
    if _has_empty_match_arm(cut_code, r"(?:DielineKind|Fefco[A-Za-z0-9_]*)::(?:Fefco_?|F)?0\d{3}"):
        failures.append("W4 ADD-05: FEFCO match arm in cut_packaging.rs is an empty no-op stub")
    cut_fns = _extract_all_brace_blocks(cut_code, r"\bfn\s+(\w+)\b")
    if not any(not _is_trivial_stub_body(body) and re.search(r"(?:(?<![A-Za-z0-9])(?i:fefco)|Fefco|(?<![A-Z])FEFCO)(?![a-z])", m.group(1) + " " + body) for m, body in cut_fns):
        failures.append("W4 ADD-05: cut_packaging.rs lacks non-stub FEFCO dieline generator implementation")

    bc_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/barcode.rs"))
    bc_fns = _extract_all_brace_blocks(bc_code, r"\bfn\s+(\w+)\b")
    for sym in ("DataMatrix", "Pdf417", "Aztec", "Gs1128"):
        if not re.search(rf"\b{sym}\b", bc_code):
            failures.append(f"W4 ADD-05: barcode.rs missing symbology {sym}")
            continue
        sym_rx = rf"(?:Barcode[A-Za-z0-9_]*::)?{sym}"
        sym_arms = _extract_match_arm_bodies(bc_code, sym_rx)
        has_real_fn = any(not _is_trivial_stub_body(b) and re.search(rf"\b{sym}\b", b) for _m, b in bc_fns)
        if _has_empty_match_arm(bc_code, sym_rx) or not sym_arms or not any(not _is_trivial_stub_body(a) for a in sym_arms) or not has_real_fn:
            failures.append(f"W4 ADD-05: {sym} in barcode.rs lacks non-stub encoding match arm / implementation")

    prep_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/prepress.rs"))
    trapping_rx = r"(?:(?<![A-Za-z0-9])(?i:trapping)|Trapping|(?<![A-Z])TRAPPING)(?![a-z])"
    halftone_rx = r"(?:(?<![A-Za-z0-9])(?i:halftone)|Halftone|(?<![A-Z])HALFTONE)(?![a-z])"
    for sym, sym_rx in (("Trapping", trapping_rx), ("Halftone", halftone_rx)):
        arm_rx = rf"(?:[A-Za-z0-9_]+::)*{sym}"
        if not re.search(sym_rx, prep_code) or _has_empty_match_arm(prep_code, arm_rx):
            failures.append(f"W4 REQ-05: prepress.rs missing non-stub {sym} vector separation control")
    prep_fns = _extract_all_brace_blocks(prep_code, r"\bfn\s+(\w+)\b")
    if not any(not _is_trivial_stub_body(b) and re.search(trapping_rx, m.group(1) + " " + b) for m, b in prep_fns):
        failures.append("W4 REQ-05: prepress.rs lacks non-stub Trapping implementation")
    if not any(not _is_trivial_stub_body(b) and re.search(halftone_rx, m.group(1) + " " + b) for m, b in prep_fns):
        failures.append("W4 REQ-05: prepress.rs lacks non-stub Halftone implementation")
    if re.search(r"\.clamp\s*\(\s*8\s*,\s*4096\s*\)", prep_code):
        failures.append("W4 REQ-05: prepress.rs still clamps separations to 4096px raster cap")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"fefco_count": fefco_count, "max_rss_kb": r_pdf.max_rss_kb})

    return TestOutcome(
        "PASS",
        f"Packaging FEFCO ({fefco_count} templates) + 2D/GS1 barcodes + Spot /CutContour+/Crease separations + preflight + DXF + gs verified",
        metrics={"fefco_count": fefco_count, "max_rss_kb": r_pdf.max_rss_kb},
    )


# ---------------------------------------------------------------------------
# Workload 3: 64-Page Facing-Pages Saddle-Stitch / PerfectBound Booklet
# ---------------------------------------------------------------------------

def _test_t4_booklet_64p(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W5-BOOKLET-64P")
    failures: list[str] = []

    doc = _base_doc_template(ctx)
    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    magenta = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}

    objects: dict[str, Any] = {
        "10": {
            "id": 10, "name": "Master Header AutoNum",
            "payload": {
                "ArtisticText": {
                    "runs": [
                        {"text": "Page ", "style": {**base_style, "font_size_pt": 14.0}},
                        {"text": "#", "style": {**base_style, "font_size_pt": 14.0}, "field": "AutoPageNumber"},
                    ],
                    "direction": "Ltr", "align": "Left",
                }
            },
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 15.0, 15.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "11": {
            "id": 11, "name": "Odd Page Marker",
            "payload": {"Shape": {"Rectangle": {"width": 16.0, "height": 12.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 120.0, 15.0]},
            "fill": {"Solid": cyan}, "outline": {**base_outline, "width_mm": 0.2},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "12": {
            "id": 12, "name": "Even Page Marker",
            "payload": {"Shape": {"Rectangle": {"width": 16.0, "height": 12.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 12.0, 180.0]},
            "fill": {"Solid": magenta}, "outline": {**base_outline, "width_mm": 0.2},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
    }

    pages: list[dict[str, Any]] = []
    next_id = 100
    for p_idx in range(64):
        rect_id = next_id
        txt_id = next_id + 1
        next_id += 2
        y_shift = 40.0 + ((p_idx % 8) * 14.0)
        objects[str(rect_id)] = {
            "id": rect_id, "name": f"Booklet Block {p_idx + 1}",
            "payload": {"Shape": {"Rectangle": {"width": 90.0, "height": 24.0, "corner_radii": [1.0, 1.0, 1.0, 1.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 25.0, y_shift]},
            "fill": {"Solid": cyan if (p_idx % 2 == 0) else magenta},
            "outline": {**base_outline, "width_mm": 0.25},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
        objects[str(txt_id)] = {
            "id": txt_id, "name": f"Chapter Label {p_idx + 1}",
            "payload": {"ArtisticText": {"runs": [{"text": f"Section {p_idx + 1:02d}", "style": {**base_style, "font_size_pt": 13.0}}], "direction": "Ltr", "align": "Left"}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 28.0, y_shift + 48.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
        layers = []
        if p_idx == 0:
            layers.extend([
                {"id": 1000, "name": "Master All", "visible": True, "locked": False, "printable": True, "master_scope": "AllPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [10]},
                {"id": 1001, "name": "Master Odd", "visible": True, "locked": False, "printable": True, "master_scope": "OddPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [11]},
                {"id": 1002, "name": "Master Even", "visible": True, "locked": False, "printable": True, "master_scope": "EvenPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [12]},
            ])
        layers.append({
            "id": 2000 + p_idx, "name": f"Content P{p_idx + 1}",
            "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False,
            "objects": [rect_id, txt_id],
        })
        pages.append({
            "id": 500 + p_idx, "name": f"Page {p_idx + 1}",
            "width_mm": 148.0, "height_mm": 210.0, "bleed_mm": 3.0, "facing_pages": True,
            "layers": layers,
        })

    doc["objects"] = objects
    doc["pages"] = pages
    doc["next_id"] = next_id
    doc["multipage_view"] = True

    in_64p = work / "booklet_64p.coree"
    out_64p = work / "booklet_64p.pdf"
    _write_coree_zip_from_doc(in_64p, "Booklet 64P", doc)

    # Isolated 2-page probe with IDENTICAL per-page body layers so ONLY OddPages/EvenPages + AutoPageNumber differ
    probe_doc = json.loads(json.dumps({**doc, "pages": [], "objects": {k: objects[k] for k in ("10", "11", "12")}}))
    probe_doc["objects"]["20"] = {
        "id": 20, "name": "Identical Body",
        "payload": {"Shape": {"Rectangle": {"width": 60.0, "height": 30.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 40.0, 80.0]},
        "fill": {"Solid": cyan}, "outline": {**base_outline, "width_mm": 0.2},
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    probe_doc["pages"] = [
        {
            "id": 500, "name": "Probe P1", "width_mm": 148.0, "height_mm": 210.0, "bleed_mm": 3.0, "facing_pages": True,
            "layers": [
                {"id": 1000, "name": "Master All", "visible": True, "locked": False, "printable": True, "master_scope": "AllPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [10]},
                {"id": 1001, "name": "Master Odd", "visible": True, "locked": False, "printable": True, "master_scope": "OddPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [11]},
                {"id": 1002, "name": "Master Even", "visible": True, "locked": False, "printable": True, "master_scope": "EvenPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [12]},
                {"id": 2000, "name": "Body P1", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [20]},
            ],
        },
        {
            "id": 501, "name": "Probe P2", "width_mm": 148.0, "height_mm": 210.0, "bleed_mm": 3.0, "facing_pages": True,
            "layers": [
                {"id": 2001, "name": "Body P2", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [20]},
            ],
        },
    ]
    probe_doc["next_id"] = 21
    probe_coree = work / "master_probe_2p.coree"
    _write_coree_zip_from_doc(probe_coree, "Master Probe 2P", probe_doc)
    del objects, pages, doc, probe_doc

    r_pdf = run_cmd([str(ctx.cli_bin), "convert", str(in_64p), str(out_64p), "--cmyk"], cwd=work, timeout=45.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_pdf.returncode != 0 or not out_64p.is_file():
        failures.append(f"W5 ADD-07: 64-page booklet PDF export failed: {r_pdf.stderr}")
    else:
        rss_err = _check_rss(r_pdf, 786_432, "T4-W5-BOOKLET-64P pdf")
        if rss_err:
            failures.append(rss_err)
        if r_pdf.duration_ms > 30_000.0:
            failures.append(f"W5 IMP-12: 64-page PDF export took {r_pdf.duration_ms:.1f} ms (> 30000 ms)")

        pinfo = run_poppler_info(out_64p, timeout_scale=ctx.timeout_scale)
        if not pinfo["ok"] or pinfo.get("fields", {}).get("Pages") != "64":
            failures.append(f"W5 ADD-07: 64-page PDF pdfinfo Pages != 64: {pinfo.get('fields')}")

        r_p1t = run_cmd(["pdftotext", "-enc", "UTF-8", "-f", "1", "-l", "1", str(out_64p), "-"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        r_p2t = run_cmd(["pdftotext", "-enc", "UTF-8", "-f", "2", "-l", "2", str(out_64p), "-"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        r_p64t = run_cmd(["pdftotext", "-enc", "UTF-8", "-f", "64", "-l", "64", str(out_64p), "-"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        p1_txt = _strip_bidi_controls(r_p1t.stdout)
        p2_txt = _strip_bidi_controls(r_p2t.stdout)
        p64_txt = _strip_bidi_controls(r_p64t.stdout)
        if r_p1t.returncode != 0 or "Page 1" not in p1_txt or "Page 2" in p1_txt:
            failures.append(f"W5 ADD-07: AutoPageNumber page 1 mismatch in pdftotext: {r_p1t.stdout[:200]!r}")
        if r_p2t.returncode != 0 or "Page 2" not in p2_txt or "Page 1" in p2_txt:
            failures.append(f"W5 ADD-07: AutoPageNumber page 2 mismatch in pdftotext: {r_p2t.stdout[:200]!r}")
        if r_p64t.returncode != 0 or "Page 64" not in p64_txt:
            failures.append(f"W5 ADD-07: AutoPageNumber 'Page 64' missing from pdftotext of 64-page booklet: {r_p64t.stdout[:200]!r}")

    # Isolated Master Odd vs Master Even pixel oracle (identical per-page body layers)
    mp0_png = work / "mp0.png"
    mp1_png = work / "mp1.png"
    r_mp0 = run_cmd([str(ctx.cli_bin), "render", str(probe_coree), str(mp0_png), "--page", "0", "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    r_mp1 = run_cmd([str(ctx.cli_bin), "render", str(probe_coree), str(mp1_png), "--page", "1", "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_mp0.returncode != 0 or r_mp1.returncode != 0 or not mp0_png.is_file() or not mp1_png.is_file():
        failures.append(f"W5 ADD-07: Master Odd/Even probe page 0/1 render failed: {r_mp0.stderr} {r_mp1.stderr}")
    else:
        try:
            mp01_diff = compare_png_pixels(mp0_png, mp1_png)
            if mp01_diff["mean_abs_diff"] <= 0.2:
                failures.append(f"W5 ADD-07: Master Odd/Even + AutoPageNumber probe rendered identically across page 0 & 1: {mp01_diff}")
            w0, h0, rgba0 = _decode_png_rgba_bytes_stdlib(mp0_png.read_bytes())
            w1, h1, rgba1 = _decode_png_rgba_bytes_stdlib(mp1_png.read_bytes())
            # Odd marker center at (128, 21) mm on 148x210 mm page; Even marker center at (20, 186) mm
            odd_x = max(0, min(w0 - 1, int(round((128.0 / 148.0) * w0))))
            odd_y = max(0, min(h0 - 1, int(round((21.0 / 210.0) * h0))))
            even_x = max(0, min(w1 - 1, int(round((20.0 / 148.0) * w1))))
            even_y = max(0, min(h1 - 1, int(round((186.0 / 210.0) * h1))))
            o0 = (odd_y * w0 + odd_x) * 4
            o1 = (odd_y * w1 + odd_x) * 4
            e0 = (even_y * w0 + even_x) * 4
            e1 = (even_y * w1 + even_x) * 4
            # Page 0: Odd marker cyan (b > 150, r < 80), Page 1 at odd marker position is white (>= 245)
            if not (rgba0[o0 + 2] > 150 and rgba0[o0] < 80 and rgba1[o1] >= 245 and rgba1[o1 + 1] >= 245):
                failures.append(f"W5 ADD-07: OddPages master marker not isolated to page 0: p0={tuple(rgba0[o0:o0+4])}, p1={tuple(rgba1[o1:o1+4])}")
            # Page 1: Even marker magenta under naive or ICC CMYK->sRGB (R > 150, B > 90, B > G + 40, G < 90),
            # Page 0 at even marker position is white (>= 245)
            is_magenta_px = (
                rgba1[e1] > 150
                and rgba1[e1 + 2] > 90
                and rgba1[e1 + 2] > rgba1[e1 + 1] + 40
                and rgba1[e1 + 1] < 90
            )
            if not (is_magenta_px and rgba0[e0] >= 245 and rgba0[e0 + 1] >= 245 and rgba0[e0 + 2] >= 245):
                failures.append(f"W5 ADD-07: EvenPages master marker not isolated to page 1: p0={tuple(rgba0[e0:e0+4])}, p1={tuple(rgba1[e1:e1+4])}")
        except _PARSE_EXCS as exc:
            failures.append(f"W5 ADD-07: Master Odd/Even probe PNG decode failed: {exc}")

    collect_dir = work / "collect_bundle"
    r_col = run_cmd([str(ctx.cli_bin), "collect", str(in_64p), str(collect_dir)], cwd=work, timeout=40.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_col.returncode != 0:
        failures.append(f"W5 REQ-07: 64p collect failed: {r_col.stderr}")
    else:
        manifest_p = collect_dir / "manifest.json"
        fonts_manifest_p = collect_dir / "fonts" / "fonts_manifest.json"
        if not manifest_p.is_file() or not fonts_manifest_p.is_file():
            failures.append("W5 REQ-07: 64p collect bundle missing manifest.json or fonts/fonts_manifest.json")
        else:
            try:
                col_manifest = json.loads(manifest_p.read_text(encoding="utf-8"))
                pages_list = col_manifest.get("pages") if isinstance(col_manifest, dict) else None
                if not isinstance(col_manifest, dict) or col_manifest.get("format") != "coree-collect-for-output/v1":
                    failures.append(f"W5 REQ-07: collect manifest invalid format: {col_manifest.get('format') if isinstance(col_manifest, dict) else None}")
                elif not isinstance(pages_list, list) or len(pages_list) != 64:
                    failures.append(f"W5 REQ-07: collect manifest len(pages)={len(pages_list) if isinstance(pages_list, list) else type(pages_list).__name__} != 64")
            except _PARSE_EXCS as exc:
                failures.append(f"W5 REQ-07: collect manifest.json malformed: {exc}")

    # 2. Verify non-stub PerfectBound, AutoPageNumber, and OS/2 fsType implementations (comment-stripped)
    prep_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/prepress.rs"))
    if not re.search(r"\bPerfectBound\b", prep_code):
        failures.append("W4/W5 REQ-05: prepress.rs missing ImpositionMode::PerfectBound")
    else:
        pb_rx = r"(?:ImpositionMode::)?PerfectBound"
        pb_arms = _extract_match_arm_bodies(prep_code, pb_rx)
        prep_fns = _extract_all_brace_blocks(prep_code, r"\bfn\s+(\w+)\b")
        has_pb_fn = any(
            not _is_trivial_stub_body(b)
            and re.search(
                r"(?:(?<![A-Za-z0-9])(?i:perfect_?bound)|PerfectBound|(?<![A-Z])PERFECT_BOUND)(?![a-z])",
                m.group(1) + " " + b,
            )
            for m, b in prep_fns
        )
        if _has_empty_match_arm(prep_code, pb_rx) or not pb_arms or not any(not _is_trivial_stub_body(a) for a in pb_arms) or not has_pb_fn:
            failures.append("W4/W5 REQ-05: prepress.rs PerfectBound match arm / implementation is empty or stub")

    text_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-core/src/text.rs"))
    if not re.search(r"\bAutoPageNumber\b", text_code):
        failures.append("W5 ADD-07: text.rs missing TextDynamicField::AutoPageNumber")

    col_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/collect.rs"))
    col_fns = _extract_all_brace_blocks(col_code, r"\bfn\s+(\w+)\b")
    fstype_rx = r"(?:(?<![A-Za-z0-9])(?i:fs_?type)|FsType|Fs_Type|(?<![A-Z])FS_?TYPE)(?![a-z])"
    if not re.search(fstype_rx, col_code) or not any(
        not _is_trivial_stub_body(b) and re.search(fstype_rx, m.group(1) + " " + b) for m, b in col_fns
    ):
        failures.append("W4/W5 REQ-07: collect.rs missing non-stub OS/2 fsType font embedding audit")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"max_rss_kb": r_pdf.max_rss_kb, "pdf_duration_ms": round(r_pdf.duration_ms, 1)})

    return TestOutcome(
        "PASS",
        f"64-page facing-pages booklet + PerfectBound + isolated Odd/Even/AutoPageNumber + collect + RSS={r_pdf.max_rss_kb} KiB ({r_pdf.duration_ms:.0f} ms) verified",
        metrics={"max_rss_kb": r_pdf.max_rss_kb, "pdf_duration_ms": round(r_pdf.duration_ms, 1)},
    )


# ---------------------------------------------------------------------------
# Workload 4: Industrial Laser / Vinyl Plotter Nesting Job
# ---------------------------------------------------------------------------

def _test_t4_laser_nesting(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-LASER-NESTING")
    failures: list[str] = []

    doc = _base_doc_template(ctx)
    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    cut_spot = {"Spot": {"name": "CutContour", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.0, 0.0], "alpha": 1.0}}
    black = {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}

    star_obj = {
        "id": 3, "name": "Nested Acute Star",
        "payload": {"Shape": {"Star": {"points": 5, "outer_radius": 35.0, "inner_ratio": 0.18, "complex": False}}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 55.0, 55.0]},
        "fill": "None", "outline": {**base_outline, "color": cut_spot, "width_mm": 0.0762, "is_hairline": True},
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"] = {
        "3": star_obj,
        "4": {
            "id": 4, "name": "Nested Arc Badge",
            "payload": {"Shape": {"Ellipse": {"rx": 24.0, "ry": 24.0, "mode": "Full", "start_deg": 0.0, "end_deg": 360.0}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 130.0, 55.0]},
            "fill": "None", "outline": {**base_outline, "color": cut_spot, "width_mm": 0.0762, "is_hairline": True},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "5": {
            "id": 5, "name": "Engrave Plate",
            "payload": {"Shape": {"Rectangle": {"width": 60.0, "height": 22.0, "corner_radii": [2.0, 2.0, 2.0, 2.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 35.0, 110.0]},
            "fill": {"Solid": black}, "outline": {**base_outline, "width_mm": 0.25},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "6": {
            "id": 6, "name": "Part Label",
            "payload": {"ArtisticText": {"runs": [{"text": "NEST-PART-A1", "style": {**base_style, "font_size_pt": 14.0}}], "direction": "Ltr", "align": "Left"}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 40.0, 145.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
    }
    doc["pages"][0]["printable_background"] = False
    doc["pages"][0]["layers"] = [
        {"id": 2, "name": "Engrave", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [5, 6]},
        {"id": 7, "name": "CutContour", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": True, "is_desktop_layer": False, "objects": [3, 4]},
    ]
    doc["next_id"] = 8

    nest_in = work / "nesting.coree"
    _write_coree_zip_from_doc(nest_in, "Laser Nesting", doc)

    # Isolated acute star for strict --kerf -6.0 vs 0.0 vs +2.0 bounds comparison (anti-no-op oracle)
    star_doc = json.loads(json.dumps(doc))
    star_doc["objects"] = {"3": star_obj}
    star_doc["pages"][0]["layers"] = [
        {"id": 7, "name": "CutContour", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": True, "is_desktop_layer": False, "objects": [3]},
    ]
    star_doc["next_id"] = 4
    star_in = work / "star_only.coree"
    _write_coree_zip_from_doc(star_in, "Kerf Star", star_doc)

    # 1. --kerf 0.0 vs +2.0 vs -6.0 bounds + Boolean self-intersection cleanup check on acute star
    star_polys: dict[str, list[list[tuple[float, float]]]] = {}
    for k_arg in ("0.0", "2.0", "-6.0"):
        k_svg = work / f"star_kerf_{k_arg}.svg"
        rk = run_cmd(
            [str(ctx.cli_bin), "laser", str(star_in), str(k_svg), "--format", "svg", "--kerf", k_arg],
            cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale,
        )
        if rk.returncode != 0 or not k_svg.is_file():
            failures.append(f"W4 REM-09: laser --kerf {k_arg} failed: {rk.stderr}")
            continue
        try:
            polys_k = _extract_svg_cut_polys(k_svg.read_bytes())
            if not polys_k:
                failures.append(f"W4 REM-09: laser --kerf {k_arg} SVG missing CUT polygons")
            else:
                star_polys[k_arg] = polys_k
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-09: laser --kerf {k_arg} SVG malformed: {exc}")

    if all(k in star_polys for k in ("0.0", "2.0", "-6.0")):
        w0 = max(p[0] for pl in star_polys["0.0"] for p in pl) - min(p[0] for pl in star_polys["0.0"] for p in pl)
        w_pos = max(p[0] for pl in star_polys["2.0"] for p in pl) - min(p[0] for pl in star_polys["2.0"] for p in pl)
        w_neg = max(p[0] for pl in star_polys["-6.0"] for p in pl) - min(p[0] for pl in star_polys["-6.0"] for p in pl)
        if w_pos <= w0 + 0.5:
            failures.append(f"W4 REM-09: outward --kerf 2.0 did not expand star bounds (w0={w0:.2f}, w_pos={w_pos:.2f})")
        if w_neg >= w0 - 1.0:
            failures.append(f"W4 REM-09: inward --kerf -6.0 did not shrink star bounds (w0={w0:.2f}, w_neg={w_neg:.2f})")
        si_star = _count_poly_self_intersections(star_polys["-6.0"])
        if si_star > 0:
            failures.append(f"W4 REM-09: laser --kerf -6.0 left {si_star} uncleaned self-intersections on acute star")

    # Full nesting job SVG with dedupe, join, hatch, weeding box, and --kerf -6.0
    kerf_svg = work / "nest_kerf.svg"
    r_svg = run_cmd(
        [str(ctx.cli_bin), "laser", str(nest_in), str(kerf_svg), "--format", "svg", "--kerf", "-6.0", "--dedupe", "0.05", "--join", "0.05", "--hatch-spacing", "1.0", "--weeding-box"],
        cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale,
    )
    if r_svg.returncode != 0 or not kerf_svg.is_file():
        failures.append(f"W4 REM-09: laser --format svg --kerf -6.0 on nesting job failed: {r_svg.stderr}")
    else:
        try:
            svg_bytes = kerf_svg.read_bytes()
            svg_s = parse_svg_structure(svg_bytes)
            polys = _extract_svg_cut_polys(svg_bytes)
            if not polys or svg_s["vector_elements"] < 2:
                failures.append(f"W4 REM-09: nesting laser SVG missing CUT polygons: {svg_s}")
            else:
                si_hits = _count_poly_self_intersections(polys)
                if si_hits > 0:
                    failures.append(f"W4 REM-09: nesting laser --kerf -6.0 left {si_hits} self-intersections")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-09: nesting laser SVG malformed: {exc}")

    # 2. Laser toolpath checks: GRBL & Ruida G-Code, laser DXF R12 (CUT/ENGRAVE + POLYLINE/LINE), laser PLT (PU/PD + SP)
    for dialect in ("grbl", "ruida"):
        out_gc = work / f"nest_{dialect}.gcode"
        r_gc = run_cmd(
            [str(ctx.cli_bin), "laser", str(nest_in), str(out_gc), "--format", "gcode", "--dialect", dialect, "--kerf", "0.15", "--dedupe", "0.05", "--join", "0.05", "--weeding-box"],
            cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale,
        )
        if r_gc.returncode != 0 or not out_gc.is_file():
            failures.append(f"W4 REM-09: laser --dialect {dialect} failed: {r_gc.stderr}")
        else:
            gc_txt = out_gc.read_text(encoding="utf-8", errors="replace")
            if ("G0" not in gc_txt and "G1" not in gc_txt) or ("M3" not in gc_txt and "M5" not in gc_txt):
                failures.append(f"W4 REM-09: laser --dialect {dialect} missing G0/G1 or M3/M5 spindle commands")

    laser_dxf = work / "nest_laser.dxf"
    r_ldxf = run_cmd([str(ctx.cli_bin), "laser", str(nest_in), str(laser_dxf), "--format", "dxf", "--weeding-box"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_ldxf.returncode != 0 or not laser_dxf.is_file():
        failures.append(f"W4 REM-09: laser --format dxf failed: {r_ldxf.stderr}")
    else:
        try:
            ldxf_raw = laser_dxf.read_bytes()
            ldxf_s = parse_dxf_structure(ldxf_raw)
            ldxf_txt = ldxf_raw.decode("utf-8", errors="replace")
            if not ldxf_s["has_eof"] or not any(e in ("LWPOLYLINE", "POLYLINE", "LINE") for e in ldxf_s["entities"]) or "CUT" not in ldxf_txt:
                failures.append(f"W4 REM-09: laser DXF missing CUT layer / POLYLINE / LINE / EOF: {ldxf_s}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-09: laser DXF malformed: {exc}")

    laser_plt = work / "nest_laser.plt"
    r_lplt = run_cmd([str(ctx.cli_bin), "laser", str(nest_in), str(laser_plt), "--format", "plt", "--weeding-box"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_lplt.returncode != 0 or not laser_plt.is_file():
        failures.append(f"W4 REM-09: laser --format plt failed: {r_lplt.stderr}")
    else:
        try:
            lplt_s = parse_plt_commands(laser_plt.read_bytes())
            if not lplt_s["has_pu_pd"] or "SP" not in lplt_s["opcodes"]:
                failures.append(f"W4 REM-09: laser PLT missing PU/PD or SP pen switch: {lplt_s}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-09: laser PLT malformed: {exc}")

    # 3. REM-08 checks routed through `coree-cli convert` (coree-io/src/{cad_plot,eps}.rs)
    conv_dxf = work / "nest_conv.dxf"
    conv_plt = work / "nest_conv.plt"
    conv_eps = work / "nest_conv.eps"
    for fmt, out_p in (("dxf", conv_dxf), ("plt", conv_plt), ("eps", conv_eps)):
        rf = run_cmd([str(ctx.cli_bin), "convert", str(nest_in), str(out_p)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if rf.returncode != 0 or not out_p.is_file():
            failures.append(f"W4 REM-08: convert -> .{fmt} failed: {rf.stderr}")

    if conv_dxf.is_file():
        try:
            dxf_s = parse_dxf_structure(conv_dxf.read_bytes())
            if not dxf_s["has_eof"] or not any(e in ("TEXT", "MTEXT") for e in dxf_s["entities"]):
                failures.append(f"W4 REM-08: convert DXF missing TEXT/MTEXT or EOF: {dxf_s}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-08: convert DXF malformed: {exc}")

    if conv_plt.is_file():
        try:
            plt_s = parse_plt_commands(conv_plt.read_bytes())
            if not plt_s["has_pu_pd"] or not plt_s["has_ci_or_aa"]:
                failures.append(f"W4 REM-08: convert PLT missing CI/AA arc commands: {plt_s}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 REM-08: convert PLT malformed: {exc}")

    if conv_eps.is_file():
        gs_eps = run_ghostscript_render(conv_eps, work / "gs_nest_eps.png", dpi=96, device="pngalpha", timeout_scale=ctx.timeout_scale)
        alpha = gs_eps.get("alpha_stats") or {}
        if (
            not gs_eps["ok"]
            or alpha.get("non_transparent_pixels", 0) <= 50
            or alpha.get("opaque_white_pixels", 1) != 0
            or alpha.get("near_white_opaque_pixels", 1) != 0
        ):
            failures.append(f"W4 REM-08: convert EPS pngalpha failed or left white knockout halo: {alpha}")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]))

    return TestOutcome(
        "PASS",
        "Laser nesting (--kerf 0/+2/-6 bounds & 0 self-intersections, GRBL/Ruida, laser DXF/PLT) + convert REM-08 (DXF TEXT, PLT CI/AA, EPS pngalpha 0 white) verified",
    )


# ---------------------------------------------------------------------------
# Workload 5: Legacy .cdr / .cmx / .cpt Migration (v3-2025, CP1256, UnknownChunks)
# ---------------------------------------------------------------------------

def _test_t4_cdr_migration(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W2-CDR-MIGRATION")
    failures: list[str] = []

    # 1. Structured non-flattening .coree -> RIFF/X6 .cdr -> .coree (< 0.01 mm fidelity, PowerClip, Symbol, DropShadow, TwoColorPattern, UnknownChunks)
    doc = _base_doc_template(ctx)
    base_outline = dict(doc["objects"]["3"]["outline"])
    base_style = dict(doc["objects"]["5"]["payload"]["ArtisticText"]["runs"][0]["style"])
    sym_uuid = "44444444-4444-4444-8444-444444444444"
    sym_rect = dict(doc["objects"]["3"])
    sym_rect["id"] = 20
    sym_rect["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]}
    doc["symbols"] = {sym_uuid: {"id": sym_uuid, "name": "MigSym", "objects": [sym_rect]}}
    doc["objects"]["3"]["effects"] = [{"DropShadow": {"dx_mm": 2.0, "dy_mm": 2.0, "blur_mm": 3.0, "opacity": 0.6, "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, "feather_direction": "Outside"}}]
    doc["objects"]["3"]["unknown_chunks"] = [{"fourcc": [88, 79, 66, 74], "version_hint": 1200, "raw_bytes": base64.b64encode(b"COREE_T4_XOBJ_v1").decode("ascii")}]
    doc["objects"]["4"]["fill"] = {"TwoColorPattern": {"fg": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "bg": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "pattern_name": "DiagonalStripe", "tile_w": 10.0, "tile_h": 10.0}}
    doc["objects"]["8"] = {
        "id": 8, "name": "MigTable",
        "payload": {"Table": {"rows": 2, "cols": 2, "col_widths_mm": [20.0, 20.0], "row_heights_mm": [10.0, 10.0], "cells": [{"row": r, "col": c, "row_span": 1, "col_span": 1, "text": f"C{r}{c}", "text_style": base_style, "fill": "None", "padding_mm": 2.0, "merged_into": None} for r in range(2) for c in range(2)], "border_style": base_outline}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 120.0]},
        "fill": "None", "outline": base_outline, "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"]["9"] = {
        "id": 9, "name": "MigDim",
        "payload": {"Dimension": {"kind": "Horizontal", "p1": {"x": 20.0, "y": 20.0}, "p2": {"x": 110.0, "y": 20.0}, "p3": None, "offset_mm": 8.0, "precision": 2, "prefix": "", "suffix": " mm", "label_override": None, "text_style": base_style, "unit_override": None}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None", "outline": base_outline, "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"]["10"] = {
        "id": 10, "name": "MigConn",
        "payload": {"Connector": {"from_obj": 3, "from_anchor": "Right", "from_fallback": {"x": 110.0, "y": 47.5}, "to_obj": 4, "to_anchor": "Left", "to_fallback": {"x": 108.0, "y": 60.0}, "style": "RoundedRightAngle", "corner_radius_mm": 3.0}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]},
        "fill": "None", "outline": base_outline, "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"]["11"] = {
        "id": 11, "name": "MigSymInst",
        "payload": {"SymbolInstance": sym_uuid},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 140.0, 140.0]},
        "fill": "None", "outline": base_outline, "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"]["12"] = {
        "id": 12, "name": "MigPowerClip",
        "payload": {
            "PowerClip": {
                "container": _rect_bezier_path(45.0, 35.0),
                "container_parametric": {"Rectangle": {"width": 45.0, "height": 35.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}},
                "lock_contents": True,
                "contents": [13],
            }
        },
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 135.0, 190.0]},
        "fill": "None", "outline": base_outline, "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["objects"]["13"] = {
        "id": 13, "name": "MigPowerClipChild",
        "payload": {"Shape": {"Ellipse": {"rx": 18.0, "ry": 12.0, "mode": "Full", "start_deg": 0.0, "end_deg": 360.0}}},
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, 155.0, 205.0]},
        "fill": {"Solid": {"Cmyk": {"c": 0.0, "m": 1.0, "y": 1.0, "k": 0.0, "a": 1.0}}}, "outline": base_outline,
        "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
        "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
    }
    doc["pages"][0]["layers"][0]["objects"].extend([8, 9, 10, 11, 12])
    doc["next_id"] = 14

    in_coree = work / "mig_src.coree"
    out_riff = work / "mig_riff.cdr"
    out_x6 = work / "mig_x6.cdr"
    rt_coree = work / "mig_rt.coree"
    _write_coree_zip_from_doc(in_coree, "CDR Migration", doc)

    r_riff = run_cmd([str(ctx.cli_bin), "convert", str(in_coree), str(out_riff), "--cdr-version", "riff"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_riff.returncode != 0 or not out_riff.is_file():
        failures.append(f"W2 MND-03: convert -> RIFF .cdr failed: {r_riff.stderr}")
    else:
        try:
            riff_info = dissect_cdr_riff(out_riff.read_bytes())
            if not riff_info["has_20b_loda"]:
                failures.append(f"W2 MND-03: RIFF .cdr emitted 12B synthetic loda instead of 20B reversed loda: {riff_info['loda_headers']}")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 MND-03: RIFF .cdr dissector failed: {exc}")

    r_x6 = run_cmd([str(ctx.cli_bin), "convert", str(in_coree), str(out_x6), "--cdr-version", "x6"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_x6.returncode != 0 or not out_x6.is_file():
        failures.append(f"W2 MND-03: convert -> X6 .cdr failed: {r_x6.stderr}")
    else:
        try:
            zcf = dissect_cdr_zcf(out_x6.read_bytes())
            if (
                not zcf["has_cdro_root"]
                or not zcf["has_valid_indirect_pointers"]
                or zcf["external_pointer_count"] < 1
                or zcf["unindirected_loda_count"] != 0
                or not zcf["has_20b_deref_loda"]
                or zcf["deref_12b_loda_count"] != 0
                or zcf["cdss_key_count"] < 1
                or zcf["doc_palette_color_count"] < 2
            ):
                failures.append(f"W2 MND-03: X6 .cdr writer missing authentic CDRO 16B indirect pointers / 20B loda / cdss / docPalette.xml: {zcf}")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 MND-03: X6 .cdr dissector failed: {exc}")

        r_rt = run_cmd([str(ctx.cli_bin), "convert", str(out_x6), str(rt_coree)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r_rt.returncode != 0 or not rt_coree.is_file():
            failures.append(f"W2 MND-03: X6 .cdr -> .coree round-trip failed: {r_rt.stderr}")
        else:
            try:
                _, rt_doc = inspect_coree_zip(rt_coree)
                rt_objs = rt_doc.get("objects", {}) if isinstance(rt_doc.get("objects"), dict) else {}
                payload_kinds = {next(iter(o.get("payload", {}).keys()), "") for o in rt_objs.values() if isinstance(o, dict) and isinstance(o.get("payload"), dict)}
                missing_st = [k for k in ("Table", "Dimension", "Connector", "SymbolInstance", "PowerClip") if k not in payload_kinds]
                if missing_st:
                    failures.append(f"W2 MND-03: .cdr round-trip flattened structured payloads {missing_st}")
                if not rt_doc.get("symbols"):
                    failures.append("W2 MND-03: .cdr round-trip dropped symbols definitions")
                if not any(isinstance(eff, dict) and "DropShadow" in eff for o in rt_objs.values() if isinstance(o, dict) and isinstance(o.get("effects"), list) for eff in o["effects"]):
                    failures.append("W2 MND-03: .cdr round-trip dropped Effect::DropShadow")
                if not any(isinstance(o, dict) and isinstance(o.get("fill"), dict) and "TwoColorPattern" in o["fill"] for o in rt_objs.values()):
                    failures.append("W2 MND-03: .cdr round-trip flattened Fill::TwoColorPattern")
                # Coordinate & bounds fidelity (< 0.01 mm) on round-tripped objects
                rt_by_name = {o.get("name"): o for o in rt_objs.values() if isinstance(o, dict) and isinstance(o.get("name"), str)}
                for src_id, exp_x, exp_y in (("3", 20.0, 20.0), ("8", 20.0, 120.0), ("11", 140.0, 140.0), ("12", 135.0, 190.0)):
                    src_name = doc["objects"][src_id]["name"]
                    rt_o = rt_by_name.get(src_name)
                    m = rt_o.get("transform", {}).get("m") if isinstance(rt_o, dict) and isinstance(rt_o.get("transform"), dict) else None
                    if rt_o is None:
                        failures.append(f"W2 MND-03: round-trip missing object '{src_name}' for < 0.01 mm bounds check")
                    elif not isinstance(m, list) or len(m) != 6:
                        failures.append(f"W2 MND-03: round-trip object '{src_name}' missing 6-element transform matrix")
                    else:
                        if any(isinstance(x, bool) for x in m):
                            m_vals: list[float] = []
                        else:
                            try:
                                m_vals = [float(x) for x in m]
                            except (TypeError, ValueError):
                                m_vals = []
                        if len(m_vals) != 6 or not all(math.isfinite(v) for v in m_vals):
                            failures.append(f"W2 MND-03: round-trip object '{src_name}' has non-numeric or non-finite transform matrix: {m!r}")
                        elif abs(m_vals[4] - exp_x) >= 0.01 or abs(m_vals[5] - exp_y) >= 0.01:
                            failures.append(f"W2 MND-03: {src_name} round-trip translation drift >= 0.01 mm: got=({m_vals[4]}, {m_vals[5]}) expected=({exp_x}, {exp_y})")
            except _PARSE_EXCS as exc:
                failures.append(f"W2 MND-03: .cdr -> .coree round-trip malformed: {exc}")

    # 2. UnknownChunkMap passthrough (XDOC) & CP1256 Arabic .cdr decoding
    unk_in = work / "unknown_in.cdr"
    unk_out = work / "unknown_rt.cdr"
    xdoc_m = b"COREE_T4_XDOC_v1"
    unk_in.write_bytes(build_cdr_riff_with_unknown_xdoc(xdoc_payload=xdoc_m))
    r_unk = run_cmd([str(ctx.cli_bin), "convert", str(unk_in), str(unk_out), "--cdr-version", "riff"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_unk.returncode != 0 or not unk_out.is_file():
        failures.append(f"W2 REM-05: UnknownChunkMap convert failed: {r_unk.stderr}")
    else:
        try:
            if xdoc_m not in dissect_cdr_riff(unk_out.read_bytes())["payloads_by_fourcc"].get("XDOC", []):
                failures.append("W2 REM-05: UnknownChunkMap dropped XDOC chunk across .cdr round-trip")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 REM-05: UnknownChunkMap output RIFF malformed: {exc}")

    cp_in = work / "cp1256_arabic.cdr"
    cp_out = work / "cp1256_rt.coree"
    cp_in.write_bytes(_build_cp1256_arabic_cdr_bytes())
    r_cp = run_cmd([str(ctx.cli_bin), "convert", str(cp_in), str(cp_out)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_cp.returncode != 0 or not cp_out.is_file():
        failures.append(f"W2 REQ-08: CP1256 Arabic .cdr convert failed: {r_cp.stderr}")
    else:
        try:
            _, cp_doc = inspect_coree_zip(cp_out)
            texts = [
                run.get("text", "")
                for o in (cp_doc.get("objects") or {}).values()
                if isinstance(o, dict) and isinstance(o.get("payload"), dict) and isinstance(o["payload"].get("ArtisticText"), dict)
                for run in o["payload"]["ArtisticText"].get("runs", [])
                if isinstance(run, dict)
            ]
            joined = "\n".join(texts)
            if "تصميم عربي كلاسيكي" not in joined or "مرحبا بالعالم العربي" not in joined:
                failures.append(f"W2 REQ-08: CP1256 Arabic text decode mismatch: got={joined!r}")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 REQ-08: CP1256 converted .coree malformed: {exc}")

    # 3. CMX inspect/convert & CPT render
    cmx_in = work / "mig.cmx"
    cmx_coree = work / "mig_cmx.coree"
    cpt_in = work / "mig.cpt"
    cpt_png = work / "mig_cpt.png"
    cmx_in.write_bytes(build_cmx_bytes())
    cpt_in.write_bytes(build_cpt_bytes())

    r_cmx_i = run_cmd([str(ctx.cli_bin), "inspect", str(cmx_in), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_cmx_i.returncode != 0:
        failures.append(f"W2 REM-07: CMX inspect failed: {r_cmx_i.stderr}")
    else:
        try:
            cmx_rep = json.loads(r_cmx_i.stdout)
            det_fmt = cmx_rep.get("detected_format", "") if isinstance(cmx_rep, dict) else ""
            if not isinstance(cmx_rep, dict) or "cmx" not in json.dumps(det_fmt).lower():
                failures.append(f"W2 REM-07: CMX inspect detected_format unexpected: {cmx_rep}")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 REM-07: CMX inspect invalid JSON: {exc}")

    r_cmx_c = run_cmd([str(ctx.cli_bin), "convert", str(cmx_in), str(cmx_coree)], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_cmx_c.returncode != 0 or not cmx_coree.is_file():
        failures.append(f"W2 REM-07: CMX convert -> .coree failed: {r_cmx_c.stderr}")
    else:
        try:
            inspect_coree_zip(cmx_coree)
        except _PARSE_EXCS as exc:
            failures.append(f"W2 REM-07: CMX converted .coree malformed: {exc}")

    r_cpt = run_cmd([str(ctx.cli_bin), "render", str(cpt_in), str(cpt_png), "--dpi", "72"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
    if r_cpt.returncode != 0 or not cpt_png.is_file():
        failures.append(f"W2 REM-07: CPT render failed: {r_cpt.stderr}")
    else:
        try:
            if parse_png_info(cpt_png.read_bytes())["non_white_pixels"] < 50:
                failures.append("W2 REM-07: CPT render produced blank PNG")
        except _PARSE_EXCS as exc:
            failures.append(f"W2 REM-07: CPT rendered PNG malformed: {exc}")

    # 4. Real external .cdr test pack gate (MND-04) + CMX/CPT source gates
    real_cdr_dir = ctx.repo_root / "crates" / "coree-cdr" / "tests" / "fixtures_real_cdr"
    real_cdrs = sorted(real_cdr_dir.glob("*.cdr")) if real_cdr_dir.is_dir() else []
    if not real_cdr_dir.is_dir() or not real_cdrs:
        failures.append("W2 MND-04: crates/coree-cdr/tests/fixtures_real_cdr/ directory missing or empty")
    else:
        for cdr_file in real_cdrs[:5]:
            ri = run_cmd([str(ctx.cli_bin), "inspect", str(cdr_file), "--json"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
            if ri.returncode != 0:
                failures.append(f"W2 MND-04: inspect {cdr_file.name} failed: {ri.stderr}")
                continue
            try:
                rep = json.loads(ri.stdout)
                pc = rep.get("page_count") if isinstance(rep, dict) else None
                oc = rep.get("object_count") if isinstance(rep, dict) else None
                if not isinstance(pc, int) or isinstance(pc, bool) or not isinstance(oc, int) or isinstance(oc, bool) or pc < 1 or oc < 1:
                    failures.append(f"W2 MND-04: real CDR {cdr_file.name} inspect empty scene (page_count={pc}, object_count={oc})")
            except _PARSE_EXCS as exc:
                failures.append(f"W2 MND-04: inspect {cdr_file.name} invalid JSON: {exc}")

    cmx_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-cdr/src/cmx.rs"))
    cpt_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-cdr/src/cpt.rs"))
    if not re.search(r"(?:(?<![A-Za-z0-9])(?i:ixlr)|Ixlr|(?<![A-Z])IXLR)(?![a-z])", cmx_code) or not re.search(r"(?:(?<![A-Za-z0-9])(?i:ixpg)|Ixpg|(?<![A-Z])IXPG)(?![a-z])", cmx_code):
        failures.append("W2 REM-07: cmx.rs missing ixlr/ixpg index table handling")
    if not re.search(r"\bCPTObject\b", cpt_code):
        failures.append("W2 REM-07: cpt.rs missing CPTObject block support")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]))

    return TestOutcome("PASS", f"CDR migration (real pack={len(real_cdrs)}, 20B loda, CDRO ZCF, <0.01mm, PowerClip, CP1256, UnknownChunkMap, CMX/CPT) verified")


# ---------------------------------------------------------------------------
# Workload 6: High-Resolution 300+ MB / >2048px 16-bit Multi-Layer PSD Pipeline
# ---------------------------------------------------------------------------

def _test_t4_photo16_psd(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-PHOTO16-PSD")
    failures: list[str] = []

    # 1. Runtime >300 MiB 4096x4096 16-bit multi-layer + layer mask (-2) + Spot PSD pipeline FIRST
    large_psd = work / "large_4096_16bit.psd"
    psd_size = _stream_write_16bit_multilayer_psd(large_psd, width=4096, height=4096)
    if psd_size < 300 * 1024 * 1024:
        return TestOutcome("ERROR", f"Internal error: generated PSD size {psd_size} < 300 MiB")

    try:
        hdr_info = _inspect_psd_header_and_layers(large_psd)
        if hdr_info["width"] != 4096 or hdr_info["height"] != 4096 or hdr_info["depth"] != 16 or hdr_info["channels"] < 5 or hdr_info["layer_count"] != 2 or not hdr_info["has_layer_mask_channel"]:
            failures.append(f"W4 IMP-06: generated 4096x4096 PSD header/layers mismatch: {hdr_info}")
    except _PARSE_EXCS as exc:
        failures.append(f"W4 IMP-06: bounded PSD header check failed: {exc}")

    r_ins = run_cmd([str(ctx.cli_bin), "inspect", str(large_psd), "--json"], cwd=work, timeout=45.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_ins.returncode != 0:
        failures.append(f"W4 IMP-06: inspect on >300 MiB 4096x4096 16-bit PSD failed: {r_ins.stderr}")
    else:
        rss_err = _check_rss(r_ins, 786_432, "T4-W4-PHOTO16-PSD inspect")
        if rss_err:
            failures.append(rss_err)

    out_coree = work / "large_psd.coree"
    r_conv = run_cmd([str(ctx.cli_bin), "convert", str(large_psd), str(out_coree)], cwd=work, timeout=65.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_conv.returncode != 0 or not out_coree.is_file():
        failures.append(f"W4 IMP-06: convert >300 MiB 4096x4096 16-bit PSD -> .coree failed: {r_conv.stderr}")
    else:
        rss_err2 = _check_rss(r_conv, 786_432, "T4-W4-PHOTO16-PSD convert")
        if rss_err2:
            failures.append(rss_err2)
        try:
            _, converted_doc = inspect_coree_zip(out_coree)
            with zipfile.ZipFile(out_coree, "r") as zf:
                info_by_name = _validate_zip_infolist(zf)
                budget = [_ZIP_MAX_TOTAL_BYTES]
                if "assets/manifest.json" in info_by_name:
                    manifest_entries = json.loads(
                        _bounded_zip_read_info(zf, info_by_name["assets/manifest.json"], remaining_budget=budget).decode("utf-8")
                    )
                    asset_list = manifest_entries if isinstance(manifest_entries, list) else []
                else:
                    doc_assets = converted_doc.get("assets", {})
                    asset_list = list(doc_assets.values()) if isinstance(doc_assets, dict) else []

                full_res_entries = [
                    a for a in asset_list
                    if isinstance(a, dict) and a.get("width") == 4096 and a.get("height") == 4096
                ]
                if len(full_res_entries) < 2:
                    dims = [(a.get("width"), a.get("height")) for a in asset_list if isinstance(a, dict)]
                    failures.append(f"W4 IMP-06: expected >= 2 4096x4096 PSD layers in assets/manifest.json, got dims={dims}")
                else:
                    for a in full_res_entries:
                        ap = a.get("path")
                        if not isinstance(ap, str) or ap not in info_by_name:
                            failures.append(f"W4 IMP-06: assets/manifest.json path {ap!r} missing in .coree ZIP")
                        else:
                            with zf.open(info_by_name[ap], "r") as afh:
                                png_hdr = afh.read(24)
                            if len(png_hdr) < 24 or png_hdr[:8] != b"\x89PNG\r\n\x1a\n" or png_hdr[12:16] != b"IHDR":
                                failures.append(f"W4 IMP-06: {ap} is not a valid PNG")
                            else:
                                pw, ph = struct.unpack(">II", png_hdr[16:24])
                                if pw != 4096 or ph != 4096:
                                    failures.append(f"W4 IMP-06: {ap} IHDR {pw}x{ph} != 4096x4096")

            objs = converted_doc.get("objects", {}) if isinstance(converted_doc.get("objects"), dict) else {}
            raster_names = [
                o.get("name")
                for o in objs.values()
                if isinstance(o, dict) and isinstance(o.get("payload"), dict) and "Raster" in o["payload"]
            ]
            if len(raster_names) < 2 or "BaseLayer" not in raster_names or "HighlightLayer" not in raster_names:
                failures.append(f"W4 IMP-06: expected >= 2 distinct Raster layers (BaseLayer, HighlightLayer), got {raster_names}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 IMP-06: converted .coree inspection failed: {exc}")

    out_png = work / "large_psd_render.png"
    r_ren = run_cmd([str(ctx.cli_bin), "render", str(large_psd), str(out_png), "--dpi", "72"], cwd=work, timeout=65.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_ren.returncode != 0 or not out_png.is_file():
        failures.append(f"W4 IMP-06: render 4096x4096 16-bit PSD failed: {r_ren.stderr}")
    else:
        rss_err3 = _check_rss(r_ren, 786_432, "T4-W4-PHOTO16-PSD render")
        if rss_err3:
            failures.append(rss_err3)
        try:
            png_info = parse_png_info(out_png.read_bytes())
            if png_info["non_white_pixels"] <= 50 or png_info["luma_stddev"] <= 1.0:
                failures.append(f"W4 IMP-06: 4096x4096 16-bit PSD rendered blank/flat: {png_info}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 IMP-06: rendered PSD PNG malformed: {exc}")

    # 2. Compact multi-layer/mask/spot PSD round-trip export & dissect_psd verification
    compact_psd = work / "compact_mask_spot.psd"
    _stream_write_16bit_multilayer_psd(compact_psd, width=16, height=16)
    try:
        cmp_in = dissect_psd(compact_psd.read_bytes())
        if cmp_in["channels"] < 5 or cmp_in["depth"] != 16 or cmp_in["layer_mask_len"] <= 0:
            failures.append(f"W4 ADD-06: compact mask/spot PSD dissector mismatch: {cmp_in}")
    except _PARSE_EXCS as exc:
        failures.append(f"W4 ADD-06: compact PSD dissector failed: {exc}")

    exp_psd = work / "exported_multi.psd"
    r_exp = run_cmd([str(ctx.cli_bin), "convert", str(compact_psd), str(exp_psd)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_exp.returncode != 0 or not exp_psd.is_file():
        failures.append(f"W4 ADD-06: convert -> .psd export failed: {r_exp.stderr}")
    else:
        try:
            exp_info = dissect_psd(exp_psd.read_bytes())
            exp_layers = _inspect_psd_header_and_layers(exp_psd)
            if (
                exp_info["width"] <= 0
                or exp_info["height"] <= 0
                or exp_info["depth"] not in (8, 16)
                or exp_info["channels"] < 4
                or exp_info["layer_mask_len"] <= 0
                or exp_layers["layer_count"] < 2
            ):
                failures.append(f"W4 ADD-06: exported PSD missing multi-layer/alpha/mask fidelity: dissect={exp_info}, layers={exp_layers}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 ADD-06: exported PSD failed dissect_psd: {exc}")

    # 3. Photo Studio ChannelMixer + ToneCurve + Spot render diff check (ADD-12, IMP-06)
    cm_doc = _base_doc_template(ctx)
    asset_uuid = "77777777-7777-4777-8777-777777777777"
    raw_px = bytes([220, 40, 20, 255] * (16 * 16))
    cm_doc["assets"] = {
        asset_uuid: {
            "id": asset_uuid, "width": 16, "height": 16,
            "rgba_bytes": base64.b64encode(raw_px).decode("ascii"),
            "source_dpi": 72.0, "color_mode": "Rgb",
        }
    }
    base_outline = dict(cm_doc["objects"]["3"]["outline"])
    spot_badge = {"Spot": {"name": "SpotCut", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.5, 0.0], "alpha": 1.0}}
    cm_doc["objects"] = {
        "8": {
            "id": 8, "name": "Photo Studio Raster",
            "payload": {"Raster": {"asset_id": asset_uuid, "width_mm": 60.0, "height_mm": 60.0, "filters": [], "crop_rect": None}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": "None", "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
        "9": {
            "id": 9, "name": "Spot Accent",
            "payload": {"Shape": {"Rectangle": {"width": 20.0, "height": 20.0, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 100.0, 20.0]},
            "fill": {"Solid": spot_badge}, "outline": {**base_outline, "width_mm": 0.0},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        },
    }
    cm_doc["pages"][0]["layers"][0]["objects"] = [8, 9]
    cm_doc["pages"][0]["layers"][1]["objects"] = []
    cm_doc["next_id"] = 10

    cm_id_in = work / "photo_id.coree"
    cm_id_png = work / "photo_id.png"
    _write_coree_zip_from_doc(cm_id_in, "Photo Identity", cm_doc)
    r_cm_id = run_cmd([str(ctx.cli_bin), "render", str(cm_id_in), str(cm_id_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

    cm_doc["objects"]["8"]["payload"]["Raster"]["filters"] = [
        {"ChannelMixer": {"red": [0.0, 1.0, 0.0], "green": [1.0, 0.0, 0.0], "blue": [0.0, 0.0, 1.0], "monochrome": False}},
        {"ToneCurve": {"points": [[0.0, 0.0], [0.5, 0.85], [1.0, 1.0]]}},
    ]
    cm_fx_in = work / "photo_fx.coree"
    cm_fx_png = work / "photo_fx.png"
    _write_coree_zip_from_doc(cm_fx_in, "Photo ChannelMixer ToneCurve", cm_doc)
    r_cm_fx = run_cmd([str(ctx.cli_bin), "render", str(cm_fx_in), str(cm_fx_png), "--dpi", "72"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cm_id.returncode != 0 or r_cm_fx.returncode != 0 or not cm_id_png.is_file() or not cm_fx_png.is_file():
        failures.append(f"W4 ADD-12: Photo Studio ChannelMixer/ToneCurve render failed: {r_cm_id.stderr} {r_cm_fx.stderr}")
    else:
        try:
            cm_diff = compare_png_pixels(cm_id_png, cm_fx_png)
            if cm_diff["mean_abs_diff"] <= 5.0:
                failures.append(f"W4 ADD-12: ChannelMixer+ToneCurve produced no visual shift vs identity: {cm_diff}")
            pw, ph, fx_rgba = _decode_png_rgba_bytes_stdlib(cm_fx_png.read_bytes())
            cx_px = max(0, min(pw - 1, int(round((50.0 / float(cm_doc["pages"][0]["width_mm"])) * pw))))
            cy_px = max(0, min(ph - 1, int(round((50.0 / float(cm_doc["pages"][0]["height_mm"])) * ph))))
            off = (cy_px * pw + cx_px) * 4
            mr, mg, mb, ma = fx_rgba[off], fx_rgba[off + 1], fx_rgba[off + 2], fx_rgba[off + 3]
            if not (mg > mr + 80 and mg > 160 and mb < 80 and ma > 200):
                failures.append(f"W4 ADD-12: ChannelMixer+ToneCurve R<->G swap did not produce green-dominant center pixel: rgba={(mr, mg, mb, ma)}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 ADD-12: Photo Studio PNG comparison failed: {exc}")

    # 4. Source checks for >2048px PSD clamp removal, PSD writer, and 16-bit CoW RasterAsset
    psd_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-io/src/psd.rs"))
    if re.search(r"\bMAX_LAYER_OUTPUT_DIM\s*:\s*(?:u32|usize)\s*=\s*2_?048\b", psd_code):
        failures.append("W4 IMP-06: psd.rs still clamps MAX_LAYER_OUTPUT_DIM = 2_048 (>2048px layers downscaled)")
    if not re.search(r"(?<![A-Za-z0-9])(?i:(?:export|write)_?psd)(?![a-z])|(?:Export|Write)(?:Psd|PSD)(?![a-z])", psd_code):
        failures.append("W4 ADD-06: psd.rs missing PSD export writer")

    re_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-render/src/raster_engine.rs"))
    has_16bit_cow = bool(
        re.search(r"(?:(?<![A-Za-z0-9])(?i:cow|copy_?on_?write)|CopyOnWrite|Cow|(?<![A-Z])COW)(?![a-z])|\bArc\s*<", re_code)
        and re.search(r"(?:(?<![A-Za-z0-9])(?i:u16_?pixels|channels_?u16|pixel_?depth|bit_?16|rgb16|rgba16|cmyk16|16_?bit)|U16Pixels|ChannelsU16|PixelDepth|Bit16|Rgb16|Rgba16|Cmyk16|(?<![A-Z])(?:RGB16|RGBA16|CMYK16))(?![a-z])|\bVec\s*<\s*u16\s*>", re_code)
    )
    if not has_16bit_cow:
        failures.append("W3/W4 IMP-06: raster_engine.rs missing 16-bit tiled CoW RasterAsset pipeline")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"psd_size_bytes": psd_size, "max_rss_kb": r_conv.max_rss_kb})

    return TestOutcome(
        "PASS",
        f"4096x4096 16-bit multi-layer+mask+spot ({psd_size // (1024 * 1024)} MiB) PSD inspect/convert/render + .psd export + ChannelMixer/ToneCurve + RSS={r_conv.max_rss_kb} KiB verified",
        metrics={"psd_size_bytes": psd_size, "max_rss_kb": r_conv.max_rss_kb},
    )


# ---------------------------------------------------------------------------
# Workload 7: Variable-Data 10,000-Row Bilingual Arabic/Latin Print Merge
# ---------------------------------------------------------------------------

def _test_t4_print_merge_10k(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-PRINT-MERGE-10K")
    failures: list[str] = []

    tpl_path = work / "merge_template.coree"
    tpl_path.write_bytes(build_print_merge_template_coree_bytes())

    csv_path = work / "merge_10k.csv"
    with csv_path.open("w", encoding="utf-8", newline="") as fh:
        fh.write("Name,Code,City\n")
        for idx in range(1, 10_001):
            if idx == 1:
                fh.write("Alice-00001,QR-00001,Cairo\n")
            elif idx == 10_000:
                fh.write("زيد 10000,QR-10000,جدة\n")
            elif idx % 2 == 0:
                fh.write(f"سارة-{idx:05d},QR-{idx:05d},الرياض\n")
            else:
                fh.write(f"User-{idx:05d},QR-{idx:05d},Alexandria\n")

    # 1. Bounded-timeout live 10k print-merge execution FIRST
    out_pdf = work / "merged_10k.pdf"
    r_pm = run_cmd(
        [str(ctx.cli_bin), "print-merge", str(tpl_path), str(csv_path), str(out_pdf)],
        cwd=work, timeout=35.0, timeout_scale=ctx.timeout_scale, measure_rss=True,
    )
    if r_pm.timed_out:
        failures.append(f"W4 IMP-07: print-merge 10,000 rows timed out after {r_pm.duration_ms:.0f} ms (non-streaming O(N^2) bottleneck)")
    elif r_pm.returncode != 0 or not out_pdf.is_file():
        failures.append(f"W4 IMP-07: print-merge 10,000 rows failed: {r_pm.stderr}")
    else:
        rss_err = _check_rss(r_pm, 1_048_576, "T4-W4-PRINT-MERGE-10K")
        if rss_err:
            failures.append(rss_err)

        pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
        if not pinfo["ok"] or pinfo.get("fields", {}).get("Pages") != "10000":
            failures.append(f"W4 IMP-07: 10k print-merge PDF Pages != 10000: {pinfo.get('fields')}")

        r_t1 = run_cmd(["pdftotext", "-enc", "UTF-8", "-f", "1", "-l", "1", str(out_pdf), "-"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        r_t10k = run_cmd(["pdftotext", "-enc", "UTF-8", "-f", "10000", "-l", "10000", str(out_pdf), "-"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        t1_txt = _strip_bidi_controls(r_t1.stdout)
        t10k_txt = _strip_bidi_controls(r_t10k.stdout)
        if r_t1.returncode != 0 or "Alice-00001" not in t1_txt or "Cairo" not in t1_txt:
            failures.append(f"W4 IMP-07: page 1 text mismatch in 10k merge PDF: {r_t1.stdout[:200]!r}")
        if r_t10k.returncode != 0 or any(tok not in t10k_txt for tok in ("زيد", "10000", "جدة")):
            failures.append(f"W4 IMP-07: page 10000 Arabic text mismatch in 10k merge PDF: {r_t10k.stdout[:200]!r}")

        gs_p1 = work / "gs_merge_p1.png"
        gs_p10k = work / "gs_merge_p10k.png"
        r_gs1 = run_cmd(
            ["gs", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-dFirstPage=1", "-dLastPage=1", "-sDEVICE=png16m", "-r72", f"-sOutputFile={gs_p1}", str(out_pdf)],
            cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale,
        )
        r_gs10k = run_cmd(
            ["gs", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-dFirstPage=10000", "-dLastPage=10000", "-sDEVICE=png16m", "-r72", f"-sOutputFile={gs_p10k}", str(out_pdf)],
            cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale,
        )
        if r_gs1.returncode != 0 or not gs_p1.is_file() or r_gs10k.returncode != 0 or not gs_p10k.is_file():
            failures.append(f"W4 IMP-07: Ghostscript failed to render 10k merge page 1 or 10000: {r_gs1.stderr} {r_gs10k.stderr}")
        else:
            try:
                p1_info = parse_png_info(gs_p1.read_bytes())
                p10k_info = parse_png_info(gs_p10k.read_bytes())
                if p1_info["non_white_pixels"] < 50 or p10k_info["non_white_pixels"] < 50:
                    failures.append(f"W4 IMP-07: 10k merge gs page 1 or 10000 blank: p1={p1_info}, p10k={p10k_info}")
                # Verify QR barcode region [20..50]mm x [60..90]mm on 150x120mm page varies between page 1 & page 10000
                w1, h1, rgba1 = _decode_png_rgba_bytes_stdlib(gs_p1.read_bytes())
                w2, h2, rgba2 = _decode_png_rgba_bytes_stdlib(gs_p10k.read_bytes())
                if (w1, h1) != (w2, h2):
                    failures.append(f"W4 ADD-05: 10k merge page 1 ({w1}x{h1}) vs page 10000 ({w2}x{h2}) size mismatch")
                else:
                    x0, x1 = int(0.14 * w1), int(0.33 * w1)
                    y0, y1 = int(0.51 * h1), int(0.74 * h1)
                    qr_diff_px = 0
                    qr_dark_px = 0
                    for yy in range(y0, y1):
                        for xx in range(x0, x1):
                            off = (yy * w1 + xx) * 4
                            if rgba1[off] < 200 or rgba1[off + 1] < 200 or rgba1[off + 2] < 200:
                                qr_dark_px += 1
                            if abs(rgba1[off] - rgba2[off]) + abs(rgba1[off + 1] - rgba2[off + 1]) + abs(rgba1[off + 2] - rgba2[off + 2]) > 30:
                                qr_diff_px += 1
                    if qr_dark_px < 10 or qr_diff_px <= 10:
                        failures.append(f"W4 ADD-05: QR barcode region did not vary between page 1 and page 10000 (dark={qr_dark_px}, diff_px={qr_diff_px})")
            except _PARSE_EXCS as exc:
                failures.append(f"W4 IMP-07: 10k merge gs PNG verification failed: {exc}")

    # 2. Verify streaming print_merge.rs implementation
    pm_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-prod/src/print_merge.rs"))
    pm_fns = _extract_all_brace_blocks(pm_code, r"\bfn\s+(\w+)\b")
    stream_rx = r"(?:(?<![A-Za-z0-9])(?i:stream(?:ing|ed|s)?)|Stream(?:ing|ed|s)?|(?<![A-Z])STREAM(?:ING|ED|S)?)(?![a-z])"
    if not re.search(stream_rx, pm_code) or not any(
        not _is_trivial_stub_body(b) and re.search(stream_rx, m.group(1) + " " + b) for m, b in pm_fns
    ):
        failures.append("W4 IMP-07: print_merge.rs lacks streaming 10k-row print merge pipeline")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"max_rss_kb": r_pm.max_rss_kb, "duration_ms": round(r_pm.duration_ms, 1)})

    return TestOutcome(
        "PASS",
        f"10,000-row bilingual Arabic/Latin streaming print-merge PDF (10000 pages, QR variation, {r_pm.duration_ms:.0f} ms, RSS={r_pm.max_rss_kb} KiB) verified",
        metrics={"max_rss_kb": r_pm.max_rss_kb, "duration_ms": round(r_pm.duration_ms, 1)},
    )


# ---------------------------------------------------------------------------
# Workload 8: 10,000+ Object Scene + Rhai DOM/CQL + >=60 FPS + <=2ms Autosave
# ---------------------------------------------------------------------------

def _test_t4_scene10k_bench(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-SCENE10K-BENCH")
    failures: list[str] = []

    # 1. Generate 10,000-object scene at runtime in scratch dir & exercise live CLI render/macro/CQL
    doc = _base_doc_template(ctx)
    base_outline = dict(doc["objects"]["3"]["outline"])
    cut_spot = {"Spot": {"name": "CutContour", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.0, 0.0], "alpha": 1.0}}
    cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    magenta = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}

    objects: dict[str, Any] = {}
    obj_ids: list[int] = []
    for idx in range(10_000):
        oid = idx + 3
        gx, gy = idx % 100, idx // 100
        obj_ids.append(oid)
        objects[str(oid)] = {
            "id": oid,
            "name": "CutContour Target" if idx == 0 else f"TileRect_{idx}",
            "payload": {"Shape": {"Rectangle": {"width": 1.4, "height": 1.8, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 10.0 + gx * 1.9, 10.0 + gy * 2.7]},
            "fill": {"Solid": cyan if (idx % 2 == 0) else magenta},
            "outline": {**base_outline, "color": cut_spot if idx == 0 else base_outline["color"], "width_mm": 0.1},
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
    doc["objects"] = objects
    doc["pages"][0]["layers"] = [
        {"id": 2, "name": "Grid10K", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": obj_ids}
    ]
    doc["next_id"] = 10_003

    scene_in = work / "scene_10k.coree"
    _write_coree_zip_from_doc(scene_in, "Scene 10K", doc)
    del objects, obj_ids, doc

    scene_png = work / "scene_10k.png"
    r_ren = run_cmd([str(ctx.cli_bin), "render", str(scene_in), str(scene_png), "--dpi", "72"], cwd=work, timeout=60.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
    if r_ren.returncode != 0 or not scene_png.is_file():
        failures.append(f"W3 IMP-12: 10k-object render failed: {r_ren.stderr}")
    else:
        rss_err = _check_rss(r_ren, 786_432, "T4-W4-SCENE10K-BENCH render")
        if rss_err:
            failures.append(rss_err)
        try:
            if parse_png_info(scene_png.read_bytes())["non_white_pixels"] < 100:
                failures.append("W3 IMP-12: 10k-object scene rendered blank")
        except _PARSE_EXCS as exc:
            failures.append(f"W3 IMP-12: 10k-object rendered PNG malformed: {exc}")

    rhai_script = _tier4_fixture(ctx, "scene_dom_recolor.rhai")
    macro_out = work / "scene_10k_macro.coree"
    r_mac = run_cmd([str(ctx.cli_bin), "run-macro", str(scene_in), str(rhai_script), str(macro_out)], cwd=work, timeout=45.0, timeout_scale=ctx.timeout_scale)
    if r_mac.returncode != 0 or "DOM_SHAPES=10000" not in r_mac.stdout or not macro_out.is_file():
        failures.append(f"W4 ADD-11: 10k-object Rhai Scene DOM macro failed (render RSS={r_ren.max_rss_kb} KiB): rc={r_mac.returncode} out={r_mac.stdout[:200]} err={r_mac.stderr[:200]}")
    else:
        try:
            _, mutated_doc = inspect_coree_zip(macro_out)
            mut_objs = mutated_doc.get("objects", {}) if isinstance(mutated_doc.get("objects"), dict) else {}
            bad_fills: list[int] = []
            for oid in range(3, 13):
                m_obj = mut_objs.get(str(oid))
                m_fill = m_obj.get("fill") if isinstance(m_obj, dict) else None
                m_solid = m_fill.get("Solid") if isinstance(m_fill, dict) else None
                m_rgba = m_solid.get("Rgba") if isinstance(m_solid, dict) else None
                try:
                    mr = float(m_rgba.get("r", -1.0)) if isinstance(m_rgba, dict) else -1.0
                    mg = float(m_rgba.get("g", -1.0)) if isinstance(m_rgba, dict) else -1.0
                    mb = float(m_rgba.get("b", -1.0)) if isinstance(m_rgba, dict) else -1.0
                except (TypeError, ValueError):
                    mr = mg = mb = -1.0
                if abs(mr - (0x11 / 255.0)) > 0.02 or abs(mg - (0x22 / 255.0)) > 0.02 or abs(mb - (0x33 / 255.0)) > 0.02:
                    bad_fills.append(oid)
            if bad_fills:
                failures.append(f"W4 ADD-11: Rhai Scene DOM macro did not persist #112233 fill mutation on object IDs {bad_fills}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 ADD-11: Rhai macro output .coree malformed: {exc}")

    gate = require_cli_subcommand(ctx, "cql", "W4")
    if gate is not None:
        failures.append(gate.message)
    else:
        r_cql = run_cmd([str(ctx.cli_bin), "cql", str(scene_in), "--query", "@name == 'CutContour Target'"], cwd=work, timeout=20.0, timeout_scale=ctx.timeout_scale)
        if (
            r_cql.returncode != 0
            or "CutContour Target" not in r_cql.stdout
            or "TileRect_1" in r_cql.stdout
            or re.search(r"\b0\s+match", r_cql.stdout, re.I)
        ):
            failures.append(f"W4 ADD-11: cql query on 10k-object scene failed or matched wrong objects: {r_cql.stdout[:200]} {r_cql.stderr[:200]}")

    # 2. Architecture & benches/ presence gates
    benches_dir = ctx.repo_root / "benches"
    cargo_toml = (ctx.repo_root / "Cargo.toml").read_text(encoding="utf-8", errors="replace")
    bench_files = sorted(benches_dir.glob("*.rs")) if benches_dir.is_dir() else []
    has_benches = bool(bench_files and "[[bench]]" in cargo_toml)
    if not has_benches:
        failures.append(f"W1.D/W3 IMP-12: benches/*.rs or [[bench]] in Cargo.toml not yet present (10k render RSS={r_ren.max_rss_kb} KiB)")

    backend_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-render/src/backend.rs"))
    if not re.search(r"\bstruct\s+WgpuBackend\b", backend_code) or not re.search(r"impl\s+CanvasBackend\s+for\s+WgpuBackend\b", backend_code):
        failures.append("W3 IMP-01: WgpuBackend implementing CanvasBackend missing in backend.rs")

    tc_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-render/src/tile_cache.rs"))
    if not re.search(r"\bstruct\s+WorldTileKey\b", tc_code):
        failures.append("W3 IMP-02: WorldTileKey persistent world-space DirtyTileCache missing in tile_cache.rs")

    auto_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-storage/src/autosave.rs"))
    db_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-storage/src/db.rs"))
    if not re.search(r"(?:(?<![A-Za-z0-9])(?i:differential|delta|background|spawn)|Differential|Delta|Background|Spawn|(?<![A-Z])DELTA)(?![a-z])", auto_code) or not re.search(r"(?:(?<![A-Za-z0-9])(?i:quarantine)|Quarantine|(?<![A-Z])QUARANTINE)(?![a-z])", db_code):
        failures.append("W1.D IMP-07: async differential autosave or corrupt SQLite quarantine missing")

    # 3. Execute offline prebuilt bench binary (never touch Cargo.lock when snapshot-isolated)
    fps_vals: list[float] = []
    autosave_ms_vals: list[float] = []
    if has_benches:
        search_deps = [
            ctx.cli_bin.parent / "deps",
            ctx.repo_root / "target" / "release" / "deps",
            ctx.repo_root / "target" / "debug" / "deps",
        ]
        bench_logs: list[str] = []
        in_repo_target = _cli_in_repo_target(ctx)
        for bf in bench_files:
            stem = bf.stem
            candidates: list[Path] = []
            for ddir in search_deps:
                if ddir.is_dir():
                    for p in ddir.glob(f"{stem}-*"):
                        if p.is_file() and os.access(p, os.X_OK) and not p.suffix:
                            candidates.append(p)
            exec_bin = max(candidates, key=lambda p: p.stat().st_mtime) if candidates else None
            if exec_bin is not None:
                rb = run_cmd([str(exec_bin), "--bench"], cwd=work, timeout=120.0, timeout_scale=ctx.timeout_scale)
            elif in_repo_target:
                rb = run_cmd(
                    ["cargo", "bench", "--offline", "--locked", "--bench", stem],
                    cwd=ctx.repo_root, env=_cargo_env(), timeout=180.0, timeout_scale=ctx.timeout_scale,
                )
            else:
                failures.append(f"W3 IMP-12: prebuilt bench binary for '{stem}' missing under target/*/deps (skipping cargo bench under snapshot isolation)")
                continue
            if rb.returncode != 0:
                failures.append(f"W3 IMP-12: offline bench '{stem}' failed: {rb.stderr[-400:]}")
            else:
                bench_logs.append(rb.stdout)

        if bench_logs:
            combined_bench = "\n".join(bench_logs)
            fps_vals = _extract_bench_fps(combined_bench)
            autosave_ms_vals = _extract_bench_autosave_ms(combined_bench)
            if not fps_vals or min(fps_vals) < 60.0 or any(v < 60.0 for v in fps_vals):
                failures.append(f"W3 IMP-12: 10k-object render benchmark failed >= 60 FPS gate (found={fps_vals})")
            if not autosave_ms_vals or max(autosave_ms_vals) > 2.0:
                failures.append(f"W1.D/W3 IMP-07/IMP-12: differential autosave benchmark failed <= 2 ms gate (found={autosave_ms_vals})")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"max_rss_kb": r_ren.max_rss_kb})

    min_fps = min(fps_vals) if fps_vals else 0.0
    auto_ms = max(autosave_ms_vals) if autosave_ms_vals else 0.0
    return TestOutcome(
        "PASS",
        f"10k scene + Rhai DOM (#112233) + CQL + WgpuBackend/WorldTileKey + min {min_fps:.1f} FPS + {auto_ms:.2f} ms autosave verified",
        metrics={"max_rss_kb": r_ren.max_rss_kb, "fps": min_fps, "autosave_ms": auto_ms},
    )


# ---------------------------------------------------------------------------
# Workload 9: Potrace Logo + Coons MeshFill + InnerShadow + Live ICC Proofing
# ---------------------------------------------------------------------------

def _test_t4_potrace_coons_icc(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W4-POTRACE-COONS-ICC")
    logo_png = _tier4_fixture(ctx, "logo_seed.png")
    if not logo_png.is_file():
        return TestOutcome("ERROR", f"Missing Tier 4 seed fixture: {logo_png}")

    failures: list[str] = []

    # 1. Topological Potrace CLI trace -> closed SVG + .coree, rendered & checked for 0 white seam pixels
    traced_svg = work / "logo_traced.svg"
    traced_coree = work / "logo_traced.coree"
    r_svg = run_cmd([str(ctx.cli_bin), "trace", str(logo_png), str(traced_svg), "--mode", "outline", "--colors", "4"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    r_cor = run_cmd([str(ctx.cli_bin), "trace", str(logo_png), str(traced_coree), "--mode", "outline", "--colors", "4"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if r_svg.returncode != 0 or r_cor.returncode != 0 or not traced_svg.is_file() or not traced_coree.is_file():
        failures.append(f"W4 IMP-05: trace logo_seed.png failed: {r_svg.stderr} {r_cor.stderr}")
    else:
        try:
            svg_root = ET.fromstring(traced_svg.read_text(encoding="utf-8"))
            paths = [el.attrib.get("d", "") for el in svg_root.iter() if el.tag.split("}", 1)[-1] == "path" and el.attrib.get("d", "").strip()]
            if len(paths) < 2 or not all(("Z" in d or "z" in d) for d in paths):
                failures.append(f"W4 IMP-05: Potrace SVG expected >= 2 closed paths, got {len(paths)}")
        except _PARSE_EXCS as exc:
            failures.append(f"W4 IMP-05: Potrace SVG malformed XML: {exc}")

        # Render traced SVG and traced .coree BEFORE modifying traced_coree
        traced_svg_png = work / "logo_traced_svg.png"
        traced_cor_png = work / "logo_traced_cor.png"
        r_rs = run_cmd([str(ctx.cli_bin), "render", str(traced_svg), str(traced_svg_png), "--dpi", "300"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        r_rc = run_cmd([str(ctx.cli_bin), "render", str(traced_coree), str(traced_cor_png), "--dpi", "300"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r_rs.returncode != 0 or not traced_svg_png.is_file():
            failures.append(f"W4 IMP-05: render logo_traced.svg failed: {r_rs.stderr}")
        else:
            try:
                rw, rh, r_rgba = _decode_png_rgba_bytes_stdlib(traced_svg_png.read_bytes())
                if rw < 32 or rh < 32:
                    failures.append(f"W4 IMP-05: rendered traced SVG too small: {rw}x{rh}")
                else:
                    def _px(px: int, py: int) -> tuple[int, int, int, int]:
                        o = (py * rw + px) * 4
                        return r_rgba[o], r_rgba[o + 1], r_rgba[o + 2], r_rgba[o + 3]

                    # logo_seed.png: outer corners white, left (0.25w, 0.25h) cyan (b>r), right (0.75w, 0.25h) crimson (r>b), center (0.5w, 0.5h) navy (b>r, dark)
                    for cx_m, cy_m in ((0, 0), (rw - 1, 0), (0, rh - 1), (rw - 1, rh - 1)):
                        cpx = _px(cx_m, cy_m)
                        if not (cpx[0] >= 245 and cpx[1] >= 245 and cpx[2] >= 245):
                            failures.append(f"W4 IMP-05: traced SVG outer corner ({cx_m},{cy_m}) not white: {cpx}")
                            break
                    left_px = _px(rw // 4, rh // 4)
                    right_px = _px((3 * rw) // 4, rh // 4)
                    center_px = _px(rw // 2, rh // 2)
                    if not (left_px[2] > left_px[0] + 60 and left_px[1] > 100):
                        failures.append(f"W4 IMP-05: traced SVG left cyan region color mismatch: {left_px}")
                    if not (right_px[0] > right_px[2] + 60 and right_px[1] < 120):
                        failures.append(f"W4 IMP-05: traced SVG right crimson region color mismatch: {right_px}")
                    if not (center_px[2] > center_px[0] and center_px[0] < 90 and center_px[1] < 90):
                        failures.append(f"W4 IMP-05: traced SVG center navy emblem color mismatch: {center_px}")
                    # 0 white seam pixels along shared vertical boundary x = rw//2 inside emblem y in [0.22*rh .. 0.78*rh]
                    cx = rw // 2
                    white_seam = 0
                    for yy in range(int(0.22 * rh), int(0.78 * rh)):
                        for dx in (-1, 0, 1):
                            pr, pg, pb, _pa = _px(cx + dx, yy)
                            if pr >= 245 and pg >= 245 and pb >= 245:
                                white_seam += 1
                    if white_seam > 0:
                        failures.append(f"W4 IMP-05: Potrace logo_traced.svg left {white_seam} white seam gap pixels along shared boundary")
            except _PARSE_EXCS as exc:
                failures.append(f"W4 IMP-05: traced SVG PNG decode failed: {exc}")

        if r_rc.returncode != 0 or not traced_cor_png.is_file():
            failures.append(f"W4 IMP-05: render logo_traced.coree failed: {r_rc.stderr}")
        else:
            try:
                if parse_png_info(traced_cor_png.read_bytes())["non_white_pixels"] < 100:
                    failures.append("W4 IMP-05: logo_traced.coree rendered blank")
            except _PARSE_EXCS as exc:
                failures.append(f"W4 IMP-05: logo_traced.coree PNG malformed: {exc}")

    # 2. Runtime Coons MeshFill (handles + alpha=0.25 vs straight/opaque) + ICC + isolated InnerShadow & Envelope diffs
    try:
        doc = _base_doc_template(ctx)
    except _PARSE_EXCS as exc:
        return TestOutcome("ERROR", f"Base doc template failed: {exc}")

    page_w_mm = 120.0
    page_h_mm = 120.0
    doc["pages"][0]["width_mm"] = page_w_mm
    doc["pages"][0]["height_mm"] = page_h_mm
    doc["pages"][0]["bleed_mm"] = 0.0
    base_outline = {
        "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}},
        "width_mm": 0.0, "is_hairline": False, "cap": "Butt", "join": "Miter", "miter_limit": 4.0,
        "dash_pattern": [], "start_arrow": "None", "end_arrow": "None", "behind_fill": False,
        "scale_with_object": False, "overprint": False, "calligraphy_angle_deg": 0.0, "calligraphy_stretch": 1.0,
    }
    straight_opaque_mesh = {
        "MeshFill": {
            "rows": 2, "cols": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "alpha": 1.0},
                {"row": 0, "col": 1, "pos": {"x": 80.0, "y": 0.0}, "color": {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "alpha": 1.0},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 80.0}, "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 1.0, "k": 0.0, "a": 1.0}}, "alpha": 1.0},
                {"row": 1, "col": 1, "pos": {"x": 80.0, "y": 80.0}, "color": {"Cmyk": {"c": 0.2, "m": 0.8, "y": 0.2, "k": 0.0, "a": 1.0}}, "alpha": 1.0},
            ],
        }
    }
    coons_mesh = {
        "MeshFill": {
            "rows": 2, "cols": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "handle_e": {"x": 20.0, "y": 15.0}, "handle_s": {"x": 15.0, "y": 20.0}, "alpha": 1.0},
                {"row": 0, "col": 1, "pos": {"x": 80.0, "y": 0.0}, "color": {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}, "handle_w": {"x": -20.0, "y": 15.0}, "handle_s": {"x": -15.0, "y": 20.0}, "alpha": 1.0},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 80.0}, "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 1.0, "k": 0.0, "a": 1.0}}, "handle_n": {"x": 15.0, "y": -20.0}, "handle_e": {"x": 20.0, "y": -15.0}, "alpha": 1.0},
                {"row": 1, "col": 1, "pos": {"x": 80.0, "y": 80.0}, "color": {"Cmyk": {"c": 0.2, "m": 0.8, "y": 0.2, "k": 0.0, "a": 1.0}}, "handle_n": {"x": -15.0, "y": -20.0}, "handle_w": {"x": -20.0, "y": -15.0}, "alpha": 0.25},
            ],
        }
    }
    cmyk_swatches: list[tuple[int, str, float, float, float, float, tuple[float, float, float, float]]] = [
        (91, "ICC Ref Swatch Cyan", 102.0, 20.0, 12.0, 12.0, (0.65, 0.15, 0.0, 0.0)),
        (92, "ICC Ref Swatch Orange", 102.0, 40.0, 12.0, 12.0, (0.0, 0.60, 0.80, 0.0)),
    ]
    doc["objects"] = {
        "90": {
            "id": 90, "name": "Coons Badge",
            "payload": {"Shape": {"Rectangle": {"width": 80.0, "height": 80.0, "corner_radii": [4.0, 4.0, 4.0, 4.0], "corner_style": "Rounded", "lock_corners": True}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, 20.0, 20.0]},
            "fill": straight_opaque_mesh, "outline": base_outline,
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
    }
    for oid, sname, sx, sy, sw, sh, (sc, sm, syy, sk) in cmyk_swatches:
        doc["objects"][str(oid)] = {
            "id": oid, "name": sname,
            "payload": {"Shape": {"Rectangle": {"width": sw, "height": sh, "corner_radii": [0.0, 0.0, 0.0, 0.0], "corner_style": "Rounded", "lock_corners": False}}},
            "transform": {"m": [1.0, 0.0, 0.0, 1.0, sx, sy]},
            "fill": {"Solid": {"Cmyk": {"c": sc, "m": sm, "y": syy, "k": sk, "a": 1.0}}},
            "outline": base_outline,
            "lens": None, "effects": [], "blend_mode": "Normal", "opacity": 1.0,
            "visible": True, "locked": False, "printable": True, "style_id": None, "unknown_chunks": [],
        }
    doc["pages"][0]["layers"] = [
        {"id": 2, "name": "Layer 1", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [90, 91, 92]}
    ]
    doc["next_id"] = 93

    straight_in = work / "coons_straight_opaque.coree"
    straight_png = work / "coons_straight_opaque.png"
    _write_coree_zip_from_doc(straight_in, "Coons Straight Opaque", doc)
    r_str = run_cmd([str(ctx.cli_bin), "render", str(straight_in), str(straight_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

    doc["objects"]["90"]["fill"] = coons_mesh
    ctrl_in = work / "coons_ctrl.coree"
    ctrl_png = work / "coons_ctrl.png"
    _write_coree_zip_from_doc(ctrl_in, "Coons Control", doc)
    r_ctrl = run_cmd([str(ctx.cli_bin), "render", str(ctrl_in), str(ctrl_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

    if r_str.returncode != 0 or r_ctrl.returncode != 0 or not straight_png.is_file() or not ctrl_png.is_file():
        failures.append(f"W3 IMP-04: Coons MeshFill render failed: {r_str.stderr} {r_ctrl.stderr}")
    else:
        try:
            mesh_diff = compare_png_pixels(straight_png, ctrl_png)
            if mesh_diff["mean_abs_diff"] <= 0.2:
                failures.append(f"W3 IMP-04: Coons tangent handles + node alpha=0.25 rendered identically to straight/opaque control: {mesh_diff}")
        except _PARSE_EXCS as exc:
            failures.append(f"W3 IMP-04: Coons MeshFill PNG diff failed: {exc}")

    icc_path = _pick_icc_profile(ctx, ("FOGRA39L_coated.icc", "IFRA26S_2004_newsprint.icc", "default_cmyk.icc"))
    icc_diff: dict[str, Any] = {}
    fx_diff: dict[str, Any] = {}
    max_ciede2000: Optional[float] = None
    swatch_des: dict[str, float] = {}
    if icc_path is None:
        failures.append("W3/W4 REQ-02: Missing CMYK ICC profile under /usr/share/color/icc/")
    else:
        icc_bytes = icc_path.read_bytes()
        doc["custom_icc_profile"] = base64.b64encode(icc_bytes).decode("ascii")
        doc["custom_icc_name"] = icc_path.name
        icc_only_in = work / "coons_icc_only.coree"
        icc_only_png = work / "coons_icc_only.png"
        _write_coree_zip_from_doc(icc_only_in, "Coons ICC Proof", doc)
        r_icc = run_cmd([str(ctx.cli_bin), "render", str(icc_only_in), str(icc_only_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

        inner_shadow_eff = {"InnerShadow": {"dx_mm": 3.0, "dy_mm": 3.0, "blur_mm": 4.0, "opacity": 0.7, "color": {"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, "feather_direction": "Inside"}}
        envelope_eff = {"Envelope": {"mode": "SingleArc", "cage": [{"x": 0.0, "y": 0.0}, {"x": 80.0, "y": 6.0}, {"x": 80.0, "y": 80.0}, {"x": 0.0, "y": 74.0}]}}

        shadow_doc = json.loads(json.dumps(doc))
        shadow_doc["objects"]["90"]["effects"] = [inner_shadow_eff]
        shadow_in = work / "coons_shadow_only.coree"
        shadow_png = work / "coons_shadow_only.png"
        _write_coree_zip_from_doc(shadow_in, "Coons InnerShadow Only", shadow_doc)
        r_sh = run_cmd([str(ctx.cli_bin), "render", str(shadow_in), str(shadow_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

        env_doc = json.loads(json.dumps(doc))
        env_doc["objects"]["90"]["effects"] = [envelope_eff]
        env_in = work / "coons_env_only.coree"
        env_png = work / "coons_env_only.png"
        _write_coree_zip_from_doc(env_in, "Coons Envelope Only", env_doc)
        r_env = run_cmd([str(ctx.cli_bin), "render", str(env_in), str(env_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

        doc["objects"]["90"]["effects"] = [inner_shadow_eff, envelope_eff]
        fx_in = work / "coons_fx_icc.coree"
        fx_png = work / "coons_fx_icc.png"
        _write_coree_zip_from_doc(fx_in, "Coons InnerShadow Envelope ICC", doc)
        r_fx = run_cmd([str(ctx.cli_bin), "render", str(fx_in), str(fx_png), "--dpi", "150"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)

        if r_icc.returncode != 0 or not icc_only_png.is_file():
            failures.append(f"W3 REQ-02: Coons ICC proof render failed: {r_icc.stderr}")
        else:
            if ctrl_png.is_file():
                try:
                    icc_diff = compare_png_pixels(ctrl_png, icc_only_png)
                    if icc_diff["mean_abs_diff"] <= 0.1:
                        failures.append(f"W3 REQ-02: FOGRA39L ICC soft-proofing had no effect on Coons MeshFill render: {icc_diff}")
                except _PARSE_EXCS as exc:
                    failures.append(f"W3 REQ-02: ICC PNG comparison failed: {exc}")

            try:
                icc_oracle = IccProfileOracle(icc_bytes)
                ipw, iph, icc_rgba = _decode_png_rgba_bytes_stdlib(icc_only_png.read_bytes())
                if ipw < 60 or iph < 60 or abs((ipw / iph) - (page_w_mm / page_h_mm)) > 0.05:
                    failures.append(f"W3/W4 REQ-02: ICC proof PNG dimensions {ipw}x{iph} mismatch page {page_w_mm}x{page_h_mm}mm")
                else:
                    local_max_de = 0.0
                    radius_px = 2
                    for _oid, sname, sx, sy, sw, sh, cmyk in cmyk_swatches:
                        sw_px = (sw / page_w_mm) * ipw
                        sh_px = (sh / page_h_mm) * iph
                        if sx < 0.0 or sy < 0.0 or sx + sw > page_w_mm or sy + sh > page_h_mm or sw_px < 10.0 or sh_px < 10.0:
                            failures.append(f"W3/W4 REQ-02: swatch {sname} ({sx},{sy} {sw}x{sh}mm -> {sw_px:.1f}x{sh_px:.1f}px) invalid for AA-free sampling")
                            break
                        cx_px = int(round(((sx + 0.5 * sw) / page_w_mm) * ipw))
                        cy_px = int(round(((sy + 0.5 * sh) / page_h_mm) * iph))
                        sr = sg = sb = sa = cnt = 0
                        for dy in range(-radius_px, radius_px + 1):
                            yy = max(0, min(iph - 1, cy_px + dy))
                            for dx in range(-radius_px, radius_px + 1):
                                xx = max(0, min(ipw - 1, cx_px + dx))
                                off = (yy * ipw + xx) * 4
                                sr += icc_rgba[off]
                                sg += icc_rgba[off + 1]
                                sb += icc_rgba[off + 2]
                                sa += icc_rgba[off + 3]
                                cnt += 1
                        avg_r, avg_g, avg_b, avg_a = sr / cnt, sg / cnt, sb / cnt, sa / cnt
                        if avg_a < 250.0:
                            failures.append(f"W3/W4 REQ-02: ICC swatch {sname} interior alpha={avg_a:.1f} < 250")
                            break
                        ren_lab = srgb_to_lab_d50((avg_r / 255.0, avg_g / 255.0, avg_b / 255.0))
                        ref_lab = srgb_to_lab_d50(icc_oracle.cmyk_to_srgb(cmyk, intent=1, bpc=True))
                        de = ciede2000(ren_lab, ref_lab)
                        swatch_des[sname] = round(de, 3)
                        local_max_de = max(local_max_de, de)
                    del icc_rgba
                    if swatch_des:
                        max_ciede2000 = local_max_de
                        if max_ciede2000 >= 1.0:
                            failures.append(
                                f"W3/W4 REQ-02: {icc_path.name} rendered CMYK swatches exceed CIEDE2000 < 1.0 "
                                f"(max_ciede2000={max_ciede2000:.3f}, per_swatch={swatch_des})"
                            )
            except _PARSE_EXCS as exc:
                failures.append(f"W3/W4 REQ-02: ICC CIEDE2000 swatch verification failed: {exc}")

        if r_sh.returncode != 0 or not shadow_png.is_file():
            failures.append(f"W3 ADD-01: InnerShadow render failed: {r_sh.stderr}")
        elif icc_only_png.is_file():
            try:
                sh_diff = compare_png_pixels(icc_only_png, shadow_png)
                if sh_diff["mean_abs_diff"] <= 0.2:
                    failures.append(f"W3 ADD-01: InnerShadow produced no visual change vs icc_only_png: {sh_diff}")
            except _PARSE_EXCS as exc:
                failures.append(f"W3 ADD-01: InnerShadow PNG comparison failed: {exc}")

        if r_env.returncode != 0 or not env_png.is_file():
            failures.append(f"W3 ADD-01: Envelope render failed: {r_env.stderr}")
        elif icc_only_png.is_file():
            try:
                env_diff = compare_png_pixels(icc_only_png, env_png)
                if env_diff["mean_abs_diff"] <= 0.2:
                    failures.append(f"W3 ADD-01: Envelope produced no visual change vs icc_only_png: {env_diff}")
            except _PARSE_EXCS as exc:
                failures.append(f"W3 ADD-01: Envelope PNG comparison failed: {exc}")

        if r_fx.returncode != 0 or not fx_png.is_file():
            failures.append(f"W3/W4 ADD-01: Coons + InnerShadow + Envelope + ICC render failed: {r_fx.stderr}")
        elif icc_only_png.is_file():
            try:
                fx_diff = compare_png_pixels(icc_only_png, fx_png)
                if fx_diff["mean_abs_diff"] <= 0.2:
                    failures.append(f"W3/W4 ADD-01: InnerShadow + Envelope produced no visual change vs icc_only_png: {fx_diff}")
            except _PARSE_EXCS as exc:
                failures.append(f"W3/W4 ADD-01: Combined fx vs icc_only PNG comparison failed: {exc}")

    # 3. Source checks for Potrace, InnerShadow, Coons MeshNode handles/alpha, and ICC routing
    tr_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-render/src/trace.rs"))
    if not re.search(r"(?:(?<![A-Za-z0-9])(?i:potrace)|Potrace|(?<![A-Z])POTRACE)(?![a-z])", tr_code):
        failures.append("W4 IMP-05: trace.rs does not implement topological Potrace")

    eff_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-core/src/effects.rs"))
    style_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-core/src/style.rs"))
    color_code = _strip_cfg_test_modules(_read_rust(ctx, "crates/coree-core/src/color.rs"))
    if not re.search(r"\bInnerShadow\b", eff_code):
        failures.append("W3 ADD-01: effects.rs missing Effect::InnerShadow")
    if not re.search(r"\bhandle_n\b", style_code) or not re.search(r"\balpha\b", style_code):
        failures.append("W3 IMP-04: style.rs MeshNode missing Coons tangent handles / node alpha")
    if not re.search(r"\bcmyk_to_rgb_icc\b", color_code):
        failures.append("W3 REQ-02: color.rs missing ICC-routed CMYK conversion")

    out_metrics: dict[str, Any] = {
        "icc_diff": round(float(icc_diff.get("mean_abs_diff", 0.0)), 2),
        "fx_diff": round(float(fx_diff.get("mean_abs_diff", 0.0)), 2),
    }
    if max_ciede2000 is not None:
        out_metrics["max_ciede2000"] = round(float(max_ciede2000), 3)
        out_metrics["swatch_ciede2000"] = swatch_des

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics=out_metrics)

    return TestOutcome(
        "PASS",
        f"Potrace zero-gap SVG/coree + Coons MeshFill (handles+alpha) + InnerShadow + Envelope vs icc_only + ICC ({icc_path.name if icc_path else 'none'}, icc_diff={icc_diff.get('mean_abs_diff', 0.0):.2f}, fx_diff={fx_diff.get('mean_abs_diff', 0.0):.2f}, max_ciede2000={max_ciede2000:.3f}) verified",
        metrics=out_metrics,
    )


# ---------------------------------------------------------------------------
# Workload 10: Deterministic Offline fuzz/ Gate & CLI Stress Fuzzing
# ---------------------------------------------------------------------------

def _test_t4_fuzz_offline(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T4-W3-FUZZ-OFFLINE")
    failures: list[str] = []

    # Part A: Deterministic opaque-box CLI stress fuzzing across 10 parser families (0 panics / 0 OOM)
    rng = random.Random(0xC04EE_F022)
    seeds: list[tuple[str, str, bytes]] = [
        ("cdr_riff", ".cdr", build_cdr_riff_with_unknown_xdoc()),
        ("cdr_x6", ".cdr", build_cdr_x6_bytes()),
        ("coree", ".coree", build_coree_bytes()),
        ("cmx", ".cmx", build_cmx_bytes()),
        ("cpt", ".cpt", build_cpt_bytes()),
        ("pdf", ".pdf", build_pdf_bytes()),
        ("svg", ".svg", build_svg_bytes()),
        ("psd", ".psd", build_psd_bytes()),
        ("dxf", ".dxf", build_dxf_bytes()),
        ("eps", ".eps", build_eps_bytes()),
    ]
    cases_run = 0
    max_rss_seen = 0
    for tag, ext, base_bytes in seeds:
        muts: list[tuple[str, bytes]] = [
            ("trunc", base_bytes[: max(4, len(base_bytes) // 3)]),
            ("flip", bytes(b ^ (0xFF if i in (8, 12, 20, len(base_bytes) // 2) else 0) for i, b in enumerate(base_bytes))),
            ("rand", bytes(rng.getrandbits(8) if (i >= 8 and rng.random() < 0.15) else b for i, b in enumerate(base_bytes))),
        ]
        for label, payload in muts:
            fpath = work / f"fuzz_{tag}_{label}{ext}"
            fpath.write_bytes(payload)
            res = run_cmd([str(ctx.cli_bin), "inspect", str(fpath), "--json"], cwd=work, timeout=10.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
            cases_run += 1
            if res.timed_out:
                failures.append(f"W3 MND-04: CLI inspect timed out on mutated {fpath.name}")
                break
            if res.returncode < 0 or res.returncode == 101 or "panicked at" in res.stderr:
                failures.append(f"W3 MND-04: CLI panic/signal crash on mutated {fpath.name} (rc={res.returncode}): {res.stderr[:250]}")
                break
            if res.max_rss_kb is not None:
                max_rss_seen = max(max_rss_seen, res.max_rss_kb)
                if res.max_rss_kb > 524_288:
                    failures.append(f"W3 MND-04: CLI OOM/RSS breach ({res.max_rss_kb} KiB) on {fpath.name}")
                    break
            if tag in ("coree", "cdr_x6", "svg", "dxf") and label == "flip":
                r_ren = run_cmd([str(ctx.cli_bin), "render", str(fpath), str(work / f"out_{tag}.png"), "--dpi", "48"], cwd=work, timeout=10.0, timeout_scale=ctx.timeout_scale, measure_rss=True)
                cases_run += 1
                if r_ren.timed_out or r_ren.returncode < 0 or r_ren.returncode == 101 or "panicked at" in r_ren.stderr:
                    failures.append(f"W3 MND-04: CLI render crash/timeout on mutated {fpath.name} (rc={r_ren.returncode})")
                    break
                if r_ren.max_rss_kb is not None:
                    max_rss_seen = max(max_rss_seen, r_ren.max_rss_kb)

    # Part B: Offline fuzz/ manifest, comment-stripped target coverage & non-trivial fuzz_target! bodies
    fuzz_manifest = ctx.repo_root / "fuzz" / "Cargo.toml"
    fuzz_targets_dir = ctx.repo_root / "fuzz" / "fuzz_targets"
    target_rs: list[Path] = []
    if not fuzz_manifest.is_file() or not fuzz_targets_dir.is_dir():
        failures.append(f"W1.D/W3 MND-04: fuzz/Cargo.toml or fuzz/fuzz_targets/ missing (CLI stress {cases_run} mutated ops clean, max_rss={max_rss_seen} KiB)")
    else:
        target_rs = sorted(fuzz_targets_dir.glob("*.rs"))
        if len(target_rs) < 4:
            failures.append(f"W3 MND-04: expected >= 4 fuzz targets under fuzz/fuzz_targets/, found {len(target_rs)}")
        else:
            stripped_targets = [
                (p.name, strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace")))
                for p in target_rs
            ]
            joined_targets = "\n".join(code for _, code in stripped_targets)
            for parser_kw in ("cdr", "pdf", "svg", "psd"):
                kw_pat = rf"(?:(?<![A-Za-z0-9])(?i:{parser_kw})|{parser_kw.capitalize()}|(?<![A-Z]){parser_kw.upper()})(?![a-z])"
                if not re.search(kw_pat, joined_targets):
                    failures.append(f"W3 MND-04: fuzz/fuzz_targets/*.rs missing non-comment target covering '{parser_kw}'")
            for fname, code in stripped_targets:
                ft_blocks = _extract_all_brace_blocks(code, r"\bfuzz_target!\s*\(\s*\|[^|]*\|")
                if not ft_blocks or any(_is_trivial_stub_body(body) for _m, body in ft_blocks):
                    failures.append(f"W3 MND-04: fuzz target {fname} missing non-trivial fuzz_target! body")
            if _cli_in_repo_target(ctx):
                fc = run_cmd(
                    ["cargo", "check", "--offline", "--locked", "--manifest-path", str(fuzz_manifest)],
                    cwd=ctx.repo_root, env=_cargo_env(), timeout=120.0, timeout_scale=ctx.timeout_scale,
                )
                if fc.returncode != 0:
                    failures.append(f"W3 MND-04: cargo check fuzz/Cargo.toml failed: {fc.stderr[-400:]}")

    if failures:
        return TestOutcome("FAIL", "; ".join(failures[:3]), metrics={"cli_fuzz_cases": cases_run, "max_rss_kb": max_rss_seen})

    return TestOutcome(
        "PASS",
        f"fuzz/ ({len(target_rs)} targets) + deterministic CLI fuzz ({cases_run} mutated ops across 10 formats, 0 panics, max_rss={max_rss_seen} KiB) verified",
        metrics={"fuzz_targets": len(target_rs), "cli_fuzz_cases": cases_run, "max_rss_kb": max_rss_seen},
    )


# ---------------------------------------------------------------------------
# Public TierModule Interface
# ---------------------------------------------------------------------------

def get_tests() -> list[TestCase]:
    return [
        TestCase(
            id="T4-W4-BROCHURE-PDFX",
            tier=TIER,
            title="Commercial CMYK Brochure + Bilingual Arabic/English PDF/X-1a + CID Subset",
            roadmap_ids=("REQ-03", "REQ-02", "ADD-10", "IMP-10"),
            feature_refs=("FEAT-07", "FEAT-36", "FEAT-38", "FEAT-75", "FEAT-83", "EXT-F05"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_brochure_pdfx,
        ),
        TestCase(
            id="T4-W4-PACKAGING-FEFCO",
            tier=TIER,
            title="Die-Cut Packaging Box (30+ FEFCO, 2D/GS1 Barcodes, Spot Separations, DXF)",
            roadmap_ids=("ADD-05", "REQ-05", "REQ-07"),
            feature_refs=("FEAT-76", "FEAT-77", "FEAT-79", "EXT-F06", "EXT-F07"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_packaging_fefco,
        ),
        TestCase(
            id="T4-W5-BOOKLET-64P",
            tier=TIER,
            title="64-Page Facing-Pages Saddle-Stitch / PerfectBound Booklet + Collect RSS Gate",
            roadmap_ids=("REQ-05", "ADD-07", "REQ-07", "IMP-12"),
            feature_refs=("FEAT-07", "FEAT-09", "FEAT-10", "FEAT-74", "FEAT-80", "FEAT-83"),
            target_wave="W5",
            smoke=False,
            run=_test_t4_booklet_64p,
        ),
        TestCase(
            id="T4-W4-LASER-NESTING",
            tier=TIER,
            title="Industrial Laser / Plotter Nesting (--kerf Cleanup, GRBL/Ruida, DXF/PLT/EPS)",
            roadmap_ids=("REM-09", "REM-08", "ADD-05"),
            feature_refs=("FEAT-84", "FEAT-85", "EXT-F06", "EXT-F08"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_laser_nesting,
        ),
        TestCase(
            id="T4-W2-CDR-MIGRATION",
            tier=TIER,
            title="Legacy .cdr/.cmx/.cpt Migration (Real Pack, 20B loda, CDRO ZCF, CP1256, PowerClip, UnknownChunks)",
            roadmap_ids=("MND-03", "MND-04", "REQ-06", "REM-05", "REM-06", "REM-07", "ADD-03"),
            feature_refs=("FEAT-42", "FEAT-81", "FEAT-82", "EXT-F03", "EXT-F04", "EXT-F05"),
            target_wave="W2",
            smoke=False,
            run=_test_t4_cdr_migration,
        ),
        TestCase(
            id="T4-W4-PHOTO16-PSD",
            tier=TIER,
            title="High-Res 300+ MB / 4096x4096 16-bit Multi-Layer+Mask+Spot PSD Pipeline & RSS Gate",
            roadmap_ids=("IMP-06", "ADD-06", "ADD-12", "IMP-12"),
            feature_refs=("FEAT-64", "FEAT-65", "FEAT-69", "FEAT-86"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_photo16_psd,
        ),
        TestCase(
            id="T4-W4-PRINT-MERGE-10K",
            tier=TIER,
            title="Variable-Data 10,000-Row Bilingual Arabic/Latin Streaming Print Merge",
            roadmap_ids=("IMP-07", "ADD-05", "IMP-12"),
            feature_refs=("FEAT-78", "FEAT-79", "EXT-F05"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_print_merge_10k,
        ),
        TestCase(
            id="T4-W4-SCENE10K-BENCH",
            tier=TIER,
            title="10,000+ Object Scene + Rhai DOM/CQL + >=60 FPS Wgpu/Tiles + <=2ms Autosave",
            roadmap_ids=("IMP-01", "IMP-02", "IMP-07", "IMP-12", "ADD-11"),
            feature_refs=("FEAT-13", "FEAT-14", "EXT-F10", "EXT-F11", "EXT-F12"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_scene10k_bench,
        ),
        TestCase(
            id="T4-W4-POTRACE-COONS-ICC",
            tier=TIER,
            title="Potrace Brand Logo + Coons MeshFill + InnerShadow + Envelope + ICC Proofing",
            roadmap_ids=("IMP-05", "IMP-04", "ADD-01", "REQ-02"),
            feature_refs=("FEAT-33", "FEAT-36", "FEAT-43", "FEAT-48", "FEAT-70", "FEAT-71"),
            target_wave="W4",
            smoke=False,
            run=_test_t4_potrace_coons_icc,
        ),
        TestCase(
            id="T4-W3-FUZZ-OFFLINE",
            tier=TIER,
            title="Deterministic Offline fuzz/ Targets & CLI Mutation Stress (0 Panics / 0 OOM)",
            roadmap_ids=("MND-04",),
            feature_refs=("FEAT-81", "FEAT-82", "FEAT-83", "FEAT-84", "FEAT-86"),
            target_wave="W3",
            smoke=False,
            run=_test_t4_fuzz_offline,
        ),
    ]
