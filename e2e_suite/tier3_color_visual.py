"""Tier 3: Cross-feature pairwise, ICC v2/v4 + CIEDE2000 colorimetry, and external visual oracles.

Pure Python 3.14+ stdlib (zero pip, zero PIL dependency in the ICC parser itself).
Reuses `e2e_suite.harness` helpers for binary discovery, subprocess+RSS, PNG/PDF/GS/Poppler.
"""

from __future__ import annotations

import base64
import json
import math
import re
import shutil
import struct
import xml.etree.ElementTree as ET
import zipfile
from pathlib import Path
from typing import Any, Optional

from e2e_suite.harness import (
    RunContext,
    TestCase,
    TestOutcome,
    _compute_png_alpha_stats,
    _decode_png_rgba_bytes_stdlib,
    _default_outline,
    _default_text_style,
    _encode_png_rgba_stdlib,
    _wrap_coree_zip,
    build_blank_white_png,
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
)

TIER = 3
TIER_NAME = "Cross-Feature Pairwise, ICC & Visual Oracles"

_PARSE_EXCEPTIONS = (ValueError, KeyError, TypeError, struct.error, json.JSONDecodeError, ET.ParseError, zipfile.BadZipFile)
_IO_PARSE_EXCEPTIONS = (*_PARSE_EXCEPTIONS, OSError, UnicodeDecodeError)

_D50_WHITE = (0.96422, 1.0, 0.82521)
_LAB_EPSILON = 216.0 / 24389.0
_LAB_KAPPA = 24389.0 / 27.0

_SRGB_TO_XYZ_D50 = (
    (0.4360747, 0.3850649, 0.1430804),
    (0.2225045, 0.7168786, 0.0606169),
    (0.0139322, 0.0971045, 0.7141733),
)
_XYZ_D50_TO_SRGB = (
    (3.1338561, -1.6168667, -0.4906146),
    (-0.9787684, 1.9161415, 0.0334540),
    (0.0719453, -0.2289914, 1.4052427),
)


# ---------------------------------------------------------------------------
# Pure-Python 3.14+ Stdlib ICC v2/v4 Parser & CIEDE2000 Colorimetry Oracle
# ---------------------------------------------------------------------------

def _clamp01(v: float) -> float:
    if not math.isfinite(v):
        return 0.0
    return 0.0 if v < 0.0 else (1.0 if v > 1.0 else v)


def _s15fixed16(buf: bytes, off: int) -> float:
    return struct.unpack_from(">i", buf, off)[0] / 65536.0


def _mat3_mul_vec(m: tuple[tuple[float, float, float], ...], v: tuple[float, float, float]) -> tuple[float, float, float]:
    return (
        m[0][0] * v[0] + m[0][1] * v[1] + m[0][2] * v[2],
        m[1][0] * v[0] + m[1][1] * v[1] + m[1][2] * v[2],
        m[2][0] * v[0] + m[2][1] * v[1] + m[2][2] * v[2],
    )


def _mat3_invert(m: tuple[tuple[float, float, float], ...]) -> Optional[tuple[tuple[float, float, float], ...]]:
    c00 = m[1][1] * m[2][2] - m[1][2] * m[2][1]
    c01 = -(m[1][0] * m[2][2] - m[1][2] * m[2][0])
    c02 = m[1][0] * m[2][1] - m[1][1] * m[2][0]
    det = m[0][0] * c00 + m[0][1] * c01 + m[0][2] * c02
    if not math.isfinite(det) or abs(det) < 1e-12:
        return None
    inv_det = 1.0 / det
    c10 = -(m[0][1] * m[2][2] - m[0][2] * m[2][1])
    c11 = m[0][0] * m[2][2] - m[0][2] * m[2][0]
    c12 = -(m[0][0] * m[2][1] - m[0][1] * m[2][0])
    c20 = m[0][1] * m[1][2] - m[0][2] * m[1][1]
    c21 = -(m[0][0] * m[1][2] - m[0][2] * m[1][0])
    c22 = m[0][0] * m[1][1] - m[0][1] * m[1][0]
    return (
        (c00 * inv_det, c10 * inv_det, c20 * inv_det),
        (c01 * inv_det, c11 * inv_det, c21 * inv_det),
        (c02 * inv_det, c12 * inv_det, c22 * inv_det),
    )


def _srgb_to_linear(c: float) -> float:
    c = _clamp01(c)
    return c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4


def _linear_to_srgb(c: float) -> float:
    c = _clamp01(c)
    return 12.92 * c if c <= 0.0031308 else _clamp01(1.055 * (c ** (1.0 / 2.4)) - 0.055)


def srgb_to_xyz_d50(rgb: tuple[float, float, float]) -> tuple[float, float, float]:
    return _mat3_mul_vec(
        _SRGB_TO_XYZ_D50,
        (_srgb_to_linear(rgb[0]), _srgb_to_linear(rgb[1]), _srgb_to_linear(rgb[2])),
    )


def xyz_d50_to_srgb(xyz: tuple[float, float, float]) -> tuple[float, float, float]:
    lin = _mat3_mul_vec(_XYZ_D50_TO_SRGB, xyz)
    return (_linear_to_srgb(lin[0]), _linear_to_srgb(lin[1]), _linear_to_srgb(lin[2]))


def xyz_d50_to_lab(xyz: tuple[float, float, float]) -> tuple[float, float, float]:
    def _f(t: float) -> float:
        t = max(0.0, t)
        return t ** (1.0 / 3.0) if t > _LAB_EPSILON else (_LAB_KAPPA * t + 16.0) / 116.0

    fx = _f(xyz[0] / _D50_WHITE[0])
    fy = _f(xyz[1] / _D50_WHITE[1])
    fz = _f(xyz[2] / _D50_WHITE[2])
    l = max(0.0, min(100.0, 116.0 * fy - 16.0))
    return (l, 500.0 * (fx - fy), 200.0 * (fy - fz))


def lab_to_xyz_d50(lab: tuple[float, float, float]) -> tuple[float, float, float]:
    l = max(0.0, min(100.0, lab[0]))
    a, b = lab[1], lab[2]
    fy = (l + 16.0) / 116.0
    fx = fy + a / 500.0
    fz = fy - b / 200.0

    def _finv(t: float) -> float:
        t3 = t * t * t
        return t3 if t3 > _LAB_EPSILON else (116.0 * t - 16.0) / _LAB_KAPPA

    x = max(0.0, _finv(fx) * _D50_WHITE[0])
    y = max(0.0, (fy * fy * fy if l > _LAB_KAPPA * _LAB_EPSILON else l / _LAB_KAPPA) * _D50_WHITE[1])
    z = max(0.0, _finv(fz) * _D50_WHITE[2])
    return (x, y, z)


def srgb_to_lab_d50(rgb: tuple[float, float, float]) -> tuple[float, float, float]:
    return xyz_d50_to_lab(srgb_to_xyz_d50(rgb))


def ciede2000(
    lab1: tuple[float, float, float],
    lab2: tuple[float, float, float],
    k_l: float = 1.0,
    k_c: float = 1.0,
    k_h: float = 1.0,
) -> float:
    """Sharma, Wu, Dalal (2005) CIEDE2000 color-difference formula."""
    l1, a1, b1 = lab1
    l2, a2, b2 = lab2
    c1 = math.hypot(a1, b1)
    c2 = math.hypot(a2, b2)
    c_bar = 0.5 * (c1 + c2)
    c_bar7 = c_bar ** 7
    pow25_7 = 25.0 ** 7
    g = 0.5 * (1.0 - math.sqrt(c_bar7 / (c_bar7 + pow25_7))) if c_bar > 0.0 else 0.5

    a1_p = (1.0 + g) * a1
    a2_p = (1.0 + g) * a2
    c1_p = math.hypot(a1_p, b1)
    c2_p = math.hypot(a2_p, b2)

    h1_p = (math.degrees(math.atan2(b1, a1_p)) % 360.0) if (c1_p > 1e-12) else 0.0
    h2_p = (math.degrees(math.atan2(b2, a2_p)) % 360.0) if (c2_p > 1e-12) else 0.0

    dl_p = l2 - l1
    dc_p = c2_p - c1_p

    if c1_p * c2_p <= 1e-12:
        dh_p = 0.0
    else:
        diff_h = h2_p - h1_p
        if abs(diff_h) <= 180.0:
            dh_p = diff_h
        elif diff_h > 180.0:
            dh_p = diff_h - 360.0
        else:
            dh_p = diff_h + 360.0

    d_big_h_p = 2.0 * math.sqrt(c1_p * c2_p) * math.sin(math.radians(dh_p * 0.5))

    l_bar_p = 0.5 * (l1 + l2)
    c_bar_p = 0.5 * (c1_p + c2_p)
    if c1_p * c2_p <= 1e-12:
        h_bar_p = h1_p + h2_p
    else:
        sum_h = h1_p + h2_p
        if abs(h1_p - h2_p) <= 180.0:
            h_bar_p = 0.5 * sum_h
        elif sum_h < 360.0:
            h_bar_p = 0.5 * (sum_h + 360.0)
        else:
            h_bar_p = 0.5 * (sum_h - 360.0)

    t = (
        1.0
        - 0.17 * math.cos(math.radians(h_bar_p - 30.0))
        + 0.24 * math.cos(math.radians(2.0 * h_bar_p))
        + 0.32 * math.cos(math.radians(3.0 * h_bar_p + 6.0))
        - 0.20 * math.cos(math.radians(4.0 * h_bar_p - 63.0))
    )
    d_theta = 30.0 * math.exp(-(((h_bar_p - 275.0) / 25.0) ** 2))
    c_bar_p7 = c_bar_p ** 7
    r_c = 2.0 * math.sqrt(c_bar_p7 / (c_bar_p7 + pow25_7)) if c_bar_p > 0.0 else 0.0
    l_m50_sq = (l_bar_p - 50.0) ** 2
    s_l = 1.0 + (0.015 * l_m50_sq) / math.sqrt(20.0 + l_m50_sq)
    s_c = 1.0 + 0.045 * c_bar_p
    s_h = 1.0 + 0.015 * c_bar_p * t
    r_t = -math.sin(math.radians(2.0 * d_theta)) * r_c

    tl = dl_p / (k_l * s_l)
    tc = dc_p / (k_c * s_c)
    th = d_big_h_p / (k_h * s_h)
    return math.sqrt(max(0.0, tl * tl + tc * tc + th * th + r_t * tc * th))


class _IccCurve:
    """ICC `curv` or `para` 1D transfer curve with forward & inverse evaluation."""

    def __init__(self, kind: str, data: Any) -> None:
        self.kind = kind
        self.data = data

    def eval(self, x: float) -> float:
        x = _clamp01(x)
        if self.kind == "identity":
            return x
        if self.kind == "gamma":
            return 0.0 if (x <= 0.0 and self.data <= 0.0) else (x ** self.data)
        if self.kind == "lut":
            tbl = self.data
            n = len(tbl)
            if n == 0:
                return x
            if n == 1:
                return tbl[0]
            pos = x * (n - 1)
            i0 = int(pos)
            if i0 >= n - 1:
                return tbl[-1]
            t = pos - i0
            return tbl[i0] * (1.0 - t) + tbl[i0 + 1] * t
        if self.kind == "para":
            ft, params = self.data
            if ft == 0:
                g = params[0]
                return 0.0 if (x <= 0.0 and g <= 0.0) else (x ** g)
            if ft == 1:
                g, a, b = params
                base = a * x + b
                return (base ** g) if (a != 0.0 and x >= -b / a and base > 0.0) else 0.0
            if ft == 2:
                g, a, b, c = params
                base = a * x + b
                return ((base ** g) + c) if (a != 0.0 and x >= -b / a and base > 0.0) else c
            if ft == 3:
                g, a, b, c, d = params
                if x < d:
                    return c * x
                base = a * x + b
                return (base ** g) if base > 0.0 else 0.0
            if ft == 4:
                g, a, b, c, d, e, f = params
                if x < d:
                    return c * x + f
                base = a * x + b
                return ((base ** g) + e) if base > 0.0 else e
        return x

    def eval_inv(self, y: float) -> float:
        y = _clamp01(y)
        if self.kind == "identity":
            return y
        if self.kind == "gamma":
            return y ** (1.0 / self.data) if self.data > 1e-9 else y
        lo, hi = 0.0, 1.0
        for _ in range(40):
            mid = 0.5 * (lo + hi)
            if self.eval(mid) < y:
                lo = mid
            else:
                hi = mid
        return 0.5 * (lo + hi)


def _parse_trc_at(buf: bytes, off: int) -> tuple[_IccCurve, int]:
    if off + 12 > len(buf):
        raise ValueError("Truncated ICC TRC curve")
    sig = buf[off:off + 4]
    if sig == b"curv":
        n = struct.unpack_from(">I", buf, off + 8)[0]
        if n == 0:
            return _IccCurve("identity", None), 12
        if n == 1:
            if off + 14 > len(buf):
                raise ValueError("Truncated ICC curv gamma")
            gamma = struct.unpack_from(">H", buf, off + 12)[0] / 256.0
            return _IccCurve("gamma", gamma), 14
        end = off + 12 + 2 * n
        if end > len(buf):
            raise ValueError("Truncated ICC curv table")
        tbl = [v / 65535.0 for v in struct.unpack_from(f">{n}H", buf, off + 12)]
        return _IccCurve("lut", tbl), 12 + 2 * n
    if sig == b"para":
        ft = struct.unpack_from(">H", buf, off + 8)[0]
        counts = {0: 1, 1: 3, 2: 4, 3: 5, 4: 7}
        if ft not in counts:
            raise ValueError(f"Unsupported ICC para functionType {ft}")
        cnt = counts[ft]
        end = off + 12 + 4 * cnt
        if end > len(buf):
            raise ValueError("Truncated ICC para params")
        params = [_s15fixed16(buf, off + 12 + 4 * i) for i in range(cnt)]
        return _IccCurve("para", (ft, params)), 12 + 4 * cnt
    raise ValueError(f"Unsupported ICC TRC signature {sig!r}")


def _eval_1d_table(tbl: list[float], x: float) -> float:
    x = _clamp01(x)
    n = len(tbl)
    if n == 0:
        return x
    if n == 1:
        return tbl[0]
    pos = x * (n - 1)
    i0 = int(pos)
    if i0 >= n - 1:
        return tbl[-1]
    t = pos - i0
    return tbl[i0] * (1.0 - t) + tbl[i0 + 1] * t


def _interp_clut_3d_tetrahedral(clut: list[float], grid: tuple[int, int, int], out_ch: int, x0: float, x1: float, x2: float) -> list[float]:
    """Sakamoto 3D tetrahedral interpolation on a (g0, g1, g2, out_ch) CLUT."""
    g0, g1, g2 = grid
    if g0 < 1 or g1 < 1 or g2 < 1 or out_ch < 1:
        raise ValueError(f"Invalid 3D CLUT dimensions grid={grid}, out_ch={out_ch}")
    if len(clut) < g0 * g1 * g2 * out_ch:
        raise ValueError("Truncated 3D CLUT buffer")
    s0 = _clamp01(x0) * (g0 - 1)
    s1 = _clamp01(x1) * (g1 - 1)
    s2 = _clamp01(x2) * (g2 - 1)
    i0 = min(g0 - 2, int(s0)) if g0 > 1 else 0
    i1 = min(g1 - 2, int(s1)) if g1 > 1 else 0
    i2 = min(g2 - 2, int(s2)) if g2 > 1 else 0
    fx = s0 - i0 if g0 > 1 else 0.0
    fy = s1 - i1 if g1 > 1 else 0.0
    fz = s2 - i2 if g2 > 1 else 0.0
    j0 = min(g0 - 1, i0 + 1)
    j1 = min(g1 - 1, i1 + 1)
    j2 = min(g2 - 1, i2 + 1)

    def _idx(a: int, b: int, c: int) -> int:
        return ((a * g1 + b) * g2 + c) * out_ch

    p000 = _idx(i0, i1, i2)
    p111 = _idx(j0, j1, j2)
    if fx >= fy:
        if fy >= fz:
            pa, pb = _idx(j0, i1, i2), _idx(j0, j1, i2)
            w0, wa, wb, w1 = 1.0 - fx, fx - fy, fy - fz, fz
        elif fx >= fz:
            pa, pb = _idx(j0, i1, i2), _idx(j0, i1, j2)
            w0, wa, wb, w1 = 1.0 - fx, fx - fz, fz - fy, fy
        else:
            pa, pb = _idx(i0, i1, j2), _idx(j0, i1, j2)
            w0, wa, wb, w1 = 1.0 - fz, fz - fx, fx - fy, fy
    else:
        if fz >= fy:
            pa, pb = _idx(i0, i1, j2), _idx(i0, j1, j2)
            w0, wa, wb, w1 = 1.0 - fz, fz - fy, fy - fx, fx
        elif fz >= fx:
            pa, pb = _idx(i0, j1, i2), _idx(i0, j1, j2)
            w0, wa, wb, w1 = 1.0 - fy, fy - fz, fz - fx, fx
        else:
            pa, pb = _idx(i0, j1, i2), _idx(j0, j1, i2)
            w0, wa, wb, w1 = 1.0 - fy, fy - fx, fx - fz, fz

    return [
        w0 * clut[p000 + ch] + wa * clut[pa + ch] + wb * clut[pb + ch] + w1 * clut[p111 + ch]
        for ch in range(out_ch)
    ]


def _interp_clut_4d_quadrilinear(
    clut: list[float],
    grid: tuple[int, int, int, int],
    out_ch: int,
    x0: float,
    x1: float,
    x2: float,
    x3: float,
) -> list[float]:
    """4D 16-corner quadrilinear interpolation for CMYK CLUTs."""
    g0, g1, g2, g3 = grid
    if g0 < 1 or g1 < 1 or g2 < 1 or g3 < 1 or out_ch < 1:
        raise ValueError(f"Invalid 4D CLUT dimensions grid={grid}, out_ch={out_ch}")
    if len(clut) < g0 * g1 * g2 * g3 * out_ch:
        raise ValueError("Truncated 4D CLUT buffer")
    coords = (_clamp01(x0), _clamp01(x1), _clamp01(x2), _clamp01(x3))
    gs = (g0, g1, g2, g3)
    lo: list[int] = []
    hi: list[int] = []
    frac: list[float] = []
    for x, g in zip(coords, gs):
        if g <= 1:
            lo.append(0)
            hi.append(0)
            frac.append(0.0)
        else:
            s = x * (g - 1)
            i = min(g - 2, int(s))
            lo.append(i)
            hi.append(i + 1)
            frac.append(s - i)

    out = [0.0] * out_ch
    for mask in range(16):
        w = 1.0
        a = hi[0] if (mask & 1) else lo[0]
        w *= frac[0] if (mask & 1) else (1.0 - frac[0])
        b = hi[1] if (mask & 2) else lo[1]
        w *= frac[1] if (mask & 2) else (1.0 - frac[1])
        c = hi[2] if (mask & 4) else lo[2]
        w *= frac[2] if (mask & 4) else (1.0 - frac[2])
        d = hi[3] if (mask & 8) else lo[3]
        w *= frac[3] if (mask & 8) else (1.0 - frac[3])
        base = (((a * g1 + b) * g2 + c) * g3 + d) * out_ch
        for ch in range(out_ch):
            out[ch] += w * clut[base + ch]
    return out


def _interp_clut_nd(clut: list[float], grid: list[int], out_ch: int, vals: list[float]) -> list[float]:
    if out_ch < 1 or not grid or any(g < 1 for g in grid):
        raise ValueError(f"Invalid CLUT dimensions grid={grid}, out_ch={out_ch}")
    expected = out_ch
    for g in grid:
        expected *= g
    if len(clut) < expected:
        raise ValueError("Truncated ND CLUT buffer")
    ndim = len(grid)
    if ndim == 3:
        return _interp_clut_3d_tetrahedral(clut, (grid[0], grid[1], grid[2]), out_ch, vals[0], vals[1], vals[2])
    if ndim == 4:
        return _interp_clut_4d_quadrilinear(
            clut, (grid[0], grid[1], grid[2], grid[3]), out_ch, vals[0], vals[1], vals[2], vals[3]
        )
    lo: list[int] = []
    hi: list[int] = []
    frac: list[float] = []
    for x, g in zip(vals, grid):
        if g <= 1:
            lo.append(0)
            hi.append(0)
            frac.append(0.0)
        else:
            s = _clamp01(x) * (g - 1)
            i = min(g - 2, int(s))
            lo.append(i)
            hi.append(i + 1)
            frac.append(s - i)
    out = [0.0] * out_ch
    for mask in range(1 << ndim):
        w = 1.0
        idx = 0
        for dim in range(ndim):
            bit = (mask >> dim) & 1
            coord = hi[dim] if bit else lo[dim]
            w *= frac[dim] if bit else (1.0 - frac[dim])
            idx = idx * grid[dim] + coord
        base = idx * out_ch
        for ch in range(out_ch):
            out[ch] += w * clut[base + ch]
    return out


class _IccLut:
    """ICC `mft1` (8-bit) or `mft2` (16-bit) multi-dimensional LUT."""

    def __init__(self, tag: bytes) -> None:
        if len(tag) < 48:
            raise ValueError("Truncated mft1/mft2 LUT header")
        sig = tag[0:4]
        self.is_16bit = sig == b"mft2"
        self.in_ch = tag[8]
        self.out_ch = tag[9]
        self.grid = tag[10]
        if self.in_ch < 1 or self.in_ch > 16 or self.out_ch < 1 or self.grid < 1:
            raise ValueError(f"Invalid mft LUT dimensions in={self.in_ch} out={self.out_ch} grid={self.grid}")
        self.matrix = (
            (_s15fixed16(tag, 12), _s15fixed16(tag, 16), _s15fixed16(tag, 20)),
            (_s15fixed16(tag, 24), _s15fixed16(tag, 28), _s15fixed16(tag, 32)),
            (_s15fixed16(tag, 36), _s15fixed16(tag, 40), _s15fixed16(tag, 44)),
        )
        if sig == b"mft1":
            in_entries = 256
            out_entries = 256
            cursor = 48
            fmt = "B"
            scale = 255.0
            elem_sz = 1
        elif sig == b"mft2":
            if len(tag) < 52:
                raise ValueError("Truncated mft2 LUT header")
            in_entries, out_entries = struct.unpack_from(">HH", tag, 48)
            cursor = 52
            fmt = "H"
            scale = 65535.0
            elem_sz = 2
        else:
            raise ValueError(f"Unsupported mft signature {sig!r}")

        in_len = self.in_ch * in_entries
        clut_len = (self.grid ** self.in_ch) * self.out_ch
        out_len = self.out_ch * out_entries
        need = cursor + (in_len + clut_len + out_len) * elem_sz
        if need > len(tag):
            raise ValueError(f"Truncated {sig!r} LUT: need {need}, got {len(tag)}")

        def _read_floats(count: int, off: int) -> list[float]:
            if count == 0:
                return []
            return [v / scale for v in struct.unpack_from(f">{count}{fmt}", tag, off)]

        self.in_tables = [
            _read_floats(in_entries, cursor + ch * in_entries * elem_sz)
            for ch in range(self.in_ch)
        ]
        clut_off = cursor + in_len * elem_sz
        self.clut = _read_floats(clut_len, clut_off)
        out_off = clut_off + clut_len * elem_sz
        self.out_tables = [
            _read_floats(out_entries, out_off + ch * out_entries * elem_sz)
            for ch in range(self.out_ch)
        ]

    def eval(self, inputs: list[float], apply_matrix: bool = False) -> list[float]:
        if len(inputs) < self.in_ch:
            raise ValueError(f"Expected >= {self.in_ch} input channels for mft LUT, got {len(inputs)}")
        vals = [_clamp01(v) for v in inputs[:self.in_ch]]
        if apply_matrix and self.in_ch == 3:
            v0, v1, v2 = _mat3_mul_vec(self.matrix, (vals[0], vals[1], vals[2]))
            vals = [_clamp01(v0), _clamp01(v1), _clamp01(v2)]
        vals = [_eval_1d_table(self.in_tables[i], vals[i]) for i in range(self.in_ch)]
        grid_dims = [self.grid] * self.in_ch
        clut_out = _interp_clut_nd(self.clut, grid_dims, self.out_ch, vals)
        return [_eval_1d_table(self.out_tables[i], clut_out[i]) for i in range(self.out_ch)]


class _IccV4Lut:
    """ICC v4 `mAB ` / `mBA ` multi-process LUT."""

    def __init__(self, tag: bytes) -> None:
        if len(tag) < 32:
            raise ValueError("Truncated mAB/mBA LUT header")
        self.is_mba = tag[0:4] == b"mBA "
        self.in_ch = tag[8]
        self.out_ch = tag[9]
        if self.in_ch < 1 or self.in_ch > 16 or self.out_ch < 1:
            raise ValueError(f"Invalid mAB/mBA dimensions in={self.in_ch} out={self.out_ch}")
        b_off, mat_off, m_off, clut_off, a_off = struct.unpack_from(">IIIII", tag, 12)

        def _read_curve_seq(offset: int, count: int) -> Optional[list[_IccCurve]]:
            if offset == 0:
                return None
            pos = offset
            curves: list[_IccCurve] = []
            for _ in range(count):
                c, raw_len = _parse_trc_at(tag, pos)
                curves.append(c)
                pos += (raw_len + 3) & ~3
            return curves

        if self.is_mba:
            # mBA: B (in_ch) -> Matrix -> M (in_ch) -> CLUT -> A (out_ch)
            self.b_curves = _read_curve_seq(b_off, self.in_ch)
            self.m_curves = _read_curve_seq(m_off, self.in_ch)
            self.a_curves = _read_curve_seq(a_off, self.out_ch)
        else:
            # mAB: A (in_ch) -> CLUT -> M (out_ch) -> Matrix -> B (out_ch)
            self.a_curves = _read_curve_seq(a_off, self.in_ch)
            self.m_curves = _read_curve_seq(m_off, self.out_ch)
            self.b_curves = _read_curve_seq(b_off, self.out_ch)

        self.matrix = None
        if mat_off != 0:
            if mat_off + 48 > len(tag):
                raise ValueError("Truncated mAB/mBA matrix")
            vals = [_s15fixed16(tag, mat_off + 4 * i) for i in range(12)]
            self.matrix = (
                ((vals[0], vals[1], vals[2]), (vals[3], vals[4], vals[5]), (vals[6], vals[7], vals[8])),
                (vals[9], vals[10], vals[11]),
            )

        self.clut = None
        if clut_off != 0:
            if clut_off + 20 > len(tag):
                raise ValueError("Truncated mAB/mBA CLUT header")
            grid = [tag[clut_off + i] for i in range(self.in_ch)]
            if any(g < 1 for g in grid):
                raise ValueError(f"Invalid mAB/mBA CLUT grid {grid}")
            precision = tag[clut_off + 16]
            if precision not in (1, 2):
                raise ValueError(f"Unsupported mAB/mBA CLUT precision {precision}")
            total = self.out_ch
            for g in grid:
                total *= g
            if clut_off + 20 + total * precision > len(tag):
                raise ValueError(
                    f"Truncated mAB/mBA CLUT payload: need {clut_off + 20 + total * precision}, got {len(tag)}"
                )
            fmt = "B" if precision == 1 else "H"
            scale = 255.0 if precision == 1 else 65535.0
            clut_vals = [v / scale for v in struct.unpack_from(f">{total}{fmt}", tag, clut_off + 20)]
            self.clut = (grid, clut_vals)

    def eval(self, inputs: list[float]) -> list[float]:
        if len(inputs) < self.in_ch:
            raise ValueError(f"Expected >= {self.in_ch} input channels for mAB/mBA LUT, got {len(inputs)}")
        vals = [_clamp01(v) for v in inputs[:self.in_ch]]
        if not self.is_mba:
            if self.a_curves is not None:
                vals = [c.eval(v) for c, v in zip(self.a_curves, vals)]
            if self.clut is not None:
                grid, clut_vals = self.clut
                vals = _interp_clut_nd(clut_vals, grid, self.out_ch, vals)
            if self.m_curves is not None:
                vals = [c.eval(v) for c, v in zip(self.m_curves, vals)]
            if self.matrix is not None and len(vals) == 3:
                mat, bias = self.matrix
                mv = _mat3_mul_vec(mat, (vals[0], vals[1], vals[2]))
                vals = [_clamp01(mv[0] + bias[0]), _clamp01(mv[1] + bias[1]), _clamp01(mv[2] + bias[2])]
            if self.b_curves is not None:
                vals = [c.eval(v) for c, v in zip(self.b_curves, vals)]
            return vals

        if self.b_curves is not None:
            vals = [c.eval(v) for c, v in zip(self.b_curves, vals)]
        if self.matrix is not None and len(vals) == 3:
            mat, bias = self.matrix
            mv = _mat3_mul_vec(mat, (vals[0], vals[1], vals[2]))
            vals = [_clamp01(mv[0] + bias[0]), _clamp01(mv[1] + bias[1]), _clamp01(mv[2] + bias[2])]
        if self.m_curves is not None:
            vals = [c.eval(v) for c, v in zip(self.m_curves, vals)]
        if self.clut is not None:
            grid, clut_vals = self.clut
            vals = _interp_clut_nd(clut_vals, grid, self.out_ch, vals)
        if self.a_curves is not None:
            vals = [c.eval(v) for c, v in zip(self.a_curves, vals)]
        return vals


class IccProfileOracle:
    """Pure-Python 3.14+ stdlib ICC v2/v4 profile parser and PCS evaluator."""

    def __init__(self, data: bytes) -> None:
        if len(data) < 132:
            raise ValueError(f"ICC profile too short ({len(data)} bytes)")
        if data[36:40] != b"acsp":
            raise ValueError(f"Invalid ICC magic at offset 36: {data[36:40]!r}")
        self.profile_size, _cmm, self.version_major, ver_min_byte = struct.unpack_from(">I4sBB", data, 0)
        if self.profile_size < 132 or self.profile_size > len(data):
            raise ValueError(f"Invalid ICC profile_size {self.profile_size} (file len {len(data)})")
        self.version_minor = ver_min_byte >> 4
        self.version_bugfix = ver_min_byte & 0x0F
        self.profile_class = data[12:16].decode("ascii", errors="replace")
        self.color_space = data[16:20].decode("ascii", errors="replace")
        self.pcs = data[20:24].decode("ascii", errors="replace")
        self.intent = struct.unpack_from(">I", data, 64)[0] & 0xFFFF
        self.illuminant = (_s15fixed16(data, 68), _s15fixed16(data, 72), _s15fixed16(data, 76))

        tag_count = struct.unpack_from(">I", data, 128)[0]
        if tag_count > 1024 or 132 + tag_count * 12 > self.profile_size:
            raise ValueError(f"Invalid ICC tag_count {tag_count}")
        self.tags: dict[str, bytes] = {}
        for i in range(tag_count):
            base = 132 + i * 12
            sig = data[base:base + 4].decode("latin-1")
            off, size = struct.unpack_from(">II", data, base + 4)
            if off < 132 or off + size > self.profile_size:
                raise ValueError(f"ICC tag {sig!r} out of bounds: off={off}, size={size}")
            self.tags[sig] = data[off:off + size]

        self.wtpt = self._parse_xyz("wtpt")
        self.bkpt = self._parse_xyz("bkpt")
        self.chad = self._parse_sf32_mat3("chad")
        self.r_xyz = self._parse_xyz("rXYZ")
        self.g_xyz = self._parse_xyz("gXYZ")
        self.b_xyz = self._parse_xyz("bXYZ")
        self.r_trc = self._parse_trc("rTRC")
        self.g_trc = self._parse_trc("gTRC")
        self.b_trc = self._parse_trc("bTRC")
        self.k_trc = self._parse_trc("kTRC")

        self.a2b: dict[int, _IccLut | _IccV4Lut] = {}
        self.b2a: dict[int, _IccLut | _IccV4Lut] = {}
        for idx in (0, 1, 2):
            a_sig = f"A2B{idx}"
            if a_sig in self.tags:
                self.a2b[idx] = self._parse_lut(self.tags[a_sig])
            b_sig = f"B2A{idx}"
            if b_sig in self.tags:
                self.b2a[idx] = self._parse_lut(self.tags[b_sig])

    def _parse_xyz(self, sig: str) -> Optional[tuple[float, float, float]]:
        raw = self.tags.get(sig)
        if raw is None or len(raw) < 20 or raw[0:4] != b"XYZ ":
            return None
        return (_s15fixed16(raw, 8), _s15fixed16(raw, 12), _s15fixed16(raw, 16))

    def _parse_sf32_mat3(self, sig: str) -> Optional[tuple[tuple[float, float, float], ...]]:
        raw = self.tags.get(sig)
        if raw is None or len(raw) < 44 or raw[0:4] != b"sf32":
            return None
        vals = [_s15fixed16(raw, 8 + 4 * i) for i in range(9)]
        return (
            (vals[0], vals[1], vals[2]),
            (vals[3], vals[4], vals[5]),
            (vals[6], vals[7], vals[8]),
        )

    def _parse_trc(self, sig: str) -> Optional[_IccCurve]:
        raw = self.tags.get(sig)
        if raw is None:
            return None
        c, _ = _parse_trc_at(raw, 0)
        return c

    def _parse_lut(self, raw: bytes) -> _IccLut | _IccV4Lut:
        sig = raw[0:4]
        if sig in (b"mft1", b"mft2"):
            return _IccLut(raw)
        if sig in (b"mAB ", b"mBA "):
            return _IccV4Lut(raw)
        raise ValueError(f"Unsupported ICC LUT tag signature {sig!r}")

    def _decode_pcs_to_xyz(self, vals: list[float], is_mft2: bool) -> tuple[float, float, float]:
        if len(vals) < 3:
            raise ValueError(f"Expected >= 3 PCS channels, got {len(vals)}")
        if self.pcs == "XYZ ":
            scale = 65535.0 / 32768.0
            return (vals[0] * scale, vals[1] * scale, vals[2] * scale)
        if is_mft2:
            l = max(0.0, min(100.0, vals[0] * (65535.0 / 65280.0) * 100.0))
            a = vals[1] * (65535.0 / 256.0) - 128.0
            b = vals[2] * (65535.0 / 256.0) - 128.0
        else:
            l = max(0.0, min(100.0, vals[0] * 100.0))
            a = vals[1] * 255.0 - 128.0
            b = vals[2] * 255.0 - 128.0
        return lab_to_xyz_d50((l, a, b))

    def _select_lut(self, table: dict[int, _IccLut | _IccV4Lut], intent: int) -> Optional[_IccLut | _IccV4Lut]:
        mapped = 1 if intent == 3 else intent
        return table.get(mapped) or table.get(1) or table.get(0) or table.get(2)

    def _apply_abs_colorimetric(self, xyz: tuple[float, float, float], intent: int) -> tuple[float, float, float]:
        if intent != 3:
            return xyz
        # Per ICC.1:2010 / LittleCMS2, v4 wtpt is already D50-adapted alongside chad;
        # Absolute Colorimetric scales PCS XYZ directly by wtpt / D50_WHITE.
        wp = self.wtpt
        if wp is None:
            return xyz
        return (
            xyz[0] * (wp[0] / _D50_WHITE[0]),
            xyz[1] * (wp[1] / _D50_WHITE[1]),
            xyz[2] * (wp[2] / _D50_WHITE[2]),
        )

    def detect_black_point(self, intent: int = 1) -> tuple[float, float, float]:
        """ISO 18619 / Adobe BPC source black point detection in PCS XYZ (D50)."""
        if self.color_space == "CMYK":
            b2a_lut = self._select_lut(self.b2a, intent)
            if b2a_lut is not None:
                if self.pcs == "XYZ ":
                    bp_pcs = [0.0, 0.0, 0.0]
                else:
                    is_16 = isinstance(b2a_lut, _IccLut) and b2a_lut.is_16bit
                    zero_ab = (32768.0 / 65535.0) if is_16 else (128.0 / 255.0)
                    bp_pcs = [0.0, zero_ab, zero_ab]
                cmyk_bp = b2a_lut.eval(bp_pcs)
                if len(cmyk_bp) >= 4:
                    return self.cmyk_to_pcs_xyz(
                        (cmyk_bp[0], cmyk_bp[1], cmyk_bp[2], cmyk_bp[3]),
                        intent=intent,
                        bpc=False,
                    )
            if self.bkpt is not None:
                return self.bkpt
            return self.cmyk_to_pcs_xyz((1.0, 1.0, 1.0, 1.0), intent=intent, bpc=False)
        if self.color_space == "RGB ":
            if self.bkpt is not None:
                return self.bkpt
            return self.rgb_to_pcs_xyz((0.0, 0.0, 0.0), intent=intent, bpc=False)
        if self.color_space == "GRAY":
            if self.bkpt is not None:
                return self.bkpt
            g0 = self.gray_to_pcs_xyz(0.0, intent=intent, bpc=False)
            g1 = self.gray_to_pcs_xyz(1.0, intent=intent, bpc=False)
            return g0 if g0[1] <= g1[1] else g1
        return self.bkpt if self.bkpt is not None else (0.0, 0.0, 0.0)

    def _apply_bpc(
        self,
        xyz: tuple[float, float, float],
        intent: int,
        bpc: bool,
        dst_bp_y: float = 0.0,
    ) -> tuple[float, float, float]:
        """ISO 18619 linear XYZ scaling relative to D50 white point."""
        if not bpc or intent == 3:
            return xyz
        src_bp = self.detect_black_point(intent=intent)
        src_y = max(0.0, min(0.99, src_bp[1]))
        dst_y = max(0.0, min(0.99, dst_bp_y))
        if abs(src_y - dst_y) < 1e-9:
            return xyz
        scale = (1.0 - dst_y) / (1.0 - src_y)
        offset = 1.0 - scale
        return (
            xyz[0] * scale + _D50_WHITE[0] * offset,
            xyz[1] * scale + _D50_WHITE[1] * offset,
            xyz[2] * scale + _D50_WHITE[2] * offset,
        )

    def cmyk_to_pcs_xyz(
        self,
        cmyk: tuple[float, float, float, float],
        intent: int = 1,
        bpc: bool = False,
    ) -> tuple[float, float, float]:
        if len(cmyk) != 4:
            raise ValueError(f"Expected 4 CMYK channels, got {len(cmyk)}")
        lut = self._select_lut(self.a2b, intent)
        if lut is None:
            raise ValueError("CMYK ICC profile missing A2B* LUT")
        if lut.in_ch != 4:
            raise ValueError(f"CMYK LUT channel mismatch: expected 4, got {lut.in_ch}")
        if isinstance(lut, _IccLut):
            out = lut.eval(list(cmyk), apply_matrix=False)
            xyz = self._decode_pcs_to_xyz(out, lut.is_16bit)
        else:
            out = lut.eval(list(cmyk))
            xyz = self._decode_pcs_to_xyz(out, False)
        xyz = self._apply_bpc(xyz, intent, bpc)
        return self._apply_abs_colorimetric(xyz, intent)

    def rgb_to_pcs_xyz(
        self,
        rgb: tuple[float, float, float],
        intent: int = 1,
        bpc: bool = False,
    ) -> tuple[float, float, float]:
        if len(rgb) != 3:
            raise ValueError(f"Expected 3 RGB channels, got {len(rgb)}")
        if self.r_xyz and self.g_xyz and self.b_xyz and self.r_trc and self.g_trc and self.b_trc:
            lr = self.r_trc.eval(rgb[0])
            lg = self.g_trc.eval(rgb[1])
            lb = self.b_trc.eval(rgb[2])
            xyz = (
                self.r_xyz[0] * lr + self.g_xyz[0] * lg + self.b_xyz[0] * lb,
                self.r_xyz[1] * lr + self.g_xyz[1] * lg + self.b_xyz[1] * lb,
                self.r_xyz[2] * lr + self.g_xyz[2] * lg + self.b_xyz[2] * lb,
            )
            xyz = self._apply_bpc(xyz, intent, bpc)
            return self._apply_abs_colorimetric(xyz, intent)
        lut = self._select_lut(self.a2b, intent)
        if lut is None:
            raise ValueError("RGB ICC profile missing matrix/TRC and A2B* LUT")
        if lut.in_ch != 3:
            raise ValueError(f"RGB LUT channel mismatch: expected 3, got {lut.in_ch}")
        if isinstance(lut, _IccLut):
            out = lut.eval(list(rgb), apply_matrix=(self.pcs == "XYZ "))
            xyz = self._decode_pcs_to_xyz(out, lut.is_16bit)
        else:
            out = lut.eval(list(rgb))
            xyz = self._decode_pcs_to_xyz(out, False)
        xyz = self._apply_bpc(xyz, intent, bpc)
        return self._apply_abs_colorimetric(xyz, intent)

    def gray_to_pcs_xyz(self, gray: float, intent: int = 1, bpc: bool = False) -> tuple[float, float, float]:
        if self.k_trc is None:
            raise ValueError("GRAY ICC profile missing kTRC tag")
        y = self.k_trc.eval(gray)
        xyz = (y * _D50_WHITE[0], y * _D50_WHITE[1], y * _D50_WHITE[2])
        xyz = self._apply_bpc(xyz, intent, bpc)
        return self._apply_abs_colorimetric(xyz, intent)

    def cmyk_to_srgb(
        self,
        cmyk: tuple[float, float, float, float],
        intent: int = 1,
        bpc: bool = False,
    ) -> tuple[float, float, float]:
        return xyz_d50_to_srgb(self.cmyk_to_pcs_xyz(cmyk, intent=intent, bpc=bpc))

    def cmyk_to_lab_d50(
        self,
        cmyk: tuple[float, float, float, float],
        intent: int = 1,
        bpc: bool = False,
    ) -> tuple[float, float, float]:
        return xyz_d50_to_lab(self.cmyk_to_pcs_xyz(cmyk, intent=intent, bpc=bpc))

    def rgb_to_lab_d50(
        self,
        rgb: tuple[float, float, float],
        intent: int = 1,
        bpc: bool = False,
    ) -> tuple[float, float, float]:
        return xyz_d50_to_lab(self.rgb_to_pcs_xyz(rgb, intent=intent, bpc=bpc))


# ---------------------------------------------------------------------------
# Deterministic Tier 3 Offline Fixture Builders & On-Disk Cache
# ---------------------------------------------------------------------------

_FIXED_DOC_ID = "33333333-3333-4333-8333-333333333333"
_FIXED_PAL_ID = "44444444-4444-4444-8444-444444444444"
_FIXED_TIME = "2026-11-22T00:00:00Z"


def _make_base_doc(title: str, width_mm: float = 120.0, height_mm: float = 120.0, bleed_mm: float = 0.0, color_mode: str = "Cmyk") -> dict[str, Any]:
    return {
        "id": _FIXED_DOC_ID,
        "title": title,
        "color_mode": color_mode,
        "default_unit": "Millimeter",
        "dpi": 300.0,
        "pages": [
            {
                "id": 1,
                "name": "Page 1",
                "width_mm": width_mm,
                "height_mm": height_mm,
                "bleed_mm": bleed_mm,
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
                        "objects": [],
                    }
                ],
            }
        ],
        "active_page_index": 0,
        "multipage_view": False,
        "objects": {},
        "assets": {},
        "palettes": [],
        "document_palette": {
            "id": _FIXED_PAL_ID,
            "name": "Document Palette",
            "color_mode": color_mode,
            "is_read_only": False,
            "swatches": [],
        },
        "color_styles": [],
        "object_styles": [],
        "symbols": {},
        "guides": [],
        "grid": {"visible": False, "snap": False, "spacing_x_mm": 10.0, "spacing_y_mm": 10.0, "subdivisions": 2, "show_dots": False},
        "perspective_grid": {"enabled": False, "mode": "TwoPoint", "horizon_y_mm": 60.0, "vanishing_points": [{"x": -20.0, "y": 60.0}, {"x": 140.0, "y": 60.0}], "density": 12, "opacity": 0.35},
        "metadata": {"author": "coree-e2e-t3", "subject": title, "keywords": ["tier3", "visual"], "notes": "Deterministic Tier 3 fixture", "created_at": _FIXED_TIME, "modified_at": _FIXED_TIME, "source_format": None, "app_version": "0.3.1"},
        "unknown_doc_chunks": [],
        "custom_icc_profile": None,
        "custom_icc_name": None,
        "next_id": 100,
    }


def _make_obj(
    oid: int,
    name: str,
    payload: dict[str, Any],
    *,
    tx: float = 0.0,
    ty: float = 0.0,
    fill: Any = "None",
    outline: Optional[dict[str, Any]] = None,
    lens: Any = None,
    effects: Optional[list[dict[str, Any]]] = None,
    opacity: float = 1.0,
) -> dict[str, Any]:
    black = {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}
    return {
        "id": oid,
        "name": name,
        "payload": payload,
        "transform": {"m": [1.0, 0.0, 0.0, 1.0, tx, ty]},
        "fill": fill,
        "outline": outline if outline is not None else _default_outline(black, 0.0, False),
        "lens": lens,
        "effects": effects or [],
        "blend_mode": "Normal",
        "opacity": opacity,
        "visible": True,
        "locked": False,
        "printable": True,
        "style_id": None,
        "unknown_chunks": [],
    }


def _rect_payload(w: float, h: float, radii: tuple[float, float, float, float] = (0.0, 0.0, 0.0, 0.0), corner_style: str = "Rounded") -> dict[str, Any]:
    return {
        "Shape": {
            "Rectangle": {
                "width": w,
                "height": h,
                "corner_radii": list(radii),
                "corner_style": corner_style,
                "lock_corners": False,
            }
        }
    }


def _ellipse_payload(rx: float, ry: float, mode: str = "Full", start_deg: float = 0.0, end_deg: float = 360.0) -> dict[str, Any]:
    return {
        "Shape": {
            "Ellipse": {
                "rx": rx,
                "ry": ry,
                "mode": mode,
                "start_deg": start_deg,
                "end_deg": end_deg,
            }
        }
    }


def build_icc_swatches_doc() -> dict[str, Any]:
    """120x80mm 6-swatch colorimetric calibration target (bleed=0)."""
    doc = _make_base_doc("Tier3 ICC Swatches", width_mm=120.0, height_mm=80.0, bleed_mm=0.0, color_mode="Cmyk")
    swatches = [
        (10, "InGamut Cyan", 10.0, 10.0, {"Cmyk": {"c": 0.65, "m": 0.15, "y": 0.0, "k": 0.0, "a": 1.0}}),
        (11, "InGamut Magenta", 45.0, 10.0, {"Cmyk": {"c": 0.0, "m": 0.85, "y": 0.05, "k": 0.0, "a": 1.0}}),
        (12, "Warm Orange", 80.0, 10.0, {"Cmyk": {"c": 0.0, "m": 0.60, "y": 0.80, "k": 0.0, "a": 1.0}}),
        (13, "Deep Warm CMYK", 10.0, 45.0, {"Cmyk": {"c": 0.4, "m": 0.7, "y": 0.2, "k": 0.1, "a": 1.0}}),
        (14, "Rich RGB Blue", 45.0, 45.0, {"Rgba": {"r": 0.15, "g": 0.45, "b": 0.90, "a": 1.0}}),
        (15, "Lab Teal", 80.0, 45.0, {"Lab": {"l": 55.0, "a_ch": -25.0, "b_ch": -10.0, "alpha": 1.0}}),
    ]
    for oid, name, x, y, col in swatches:
        doc["objects"][str(oid)] = _make_obj(oid, name, _rect_payload(28.0, 24.0), tx=x, ty=y, fill={"Solid": col})
        doc["pages"][0]["layers"][0]["objects"].append(oid)
    return doc


def build_pairwise_master_doc() -> dict[str, Any]:
    """120x120mm pairwise card covering Color Models x Shapes x Fills x Outlines."""
    doc = _make_base_doc("Tier3 Pairwise Master", width_mm=120.0, height_mm=120.0, bleed_mm=0.0, color_mode="Cmyk")
    c_cyan = {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    c_mag = {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}
    c_hsl = {"Hsl": {"h": 140.0, "s": 0.75, "l": 0.45, "a": 1.0}}
    c_gray = {"Gray": {"v": 0.35, "a": 1.0}}
    c_spot = {"Spot": {"name": "SpotOrange", "tint": 0.85, "fallback_cmyk": [0.0, 0.55, 0.95, 0.0], "alpha": 1.0}}
    c_lab = {"Lab": {"l": 60.0, "a_ch": 40.0, "b_ch": 20.0, "alpha": 1.0}}

    items = [
        (10, "Chamfer Linear", _rect_payload(30.0, 24.0, (4.0, 4.0, 4.0, 4.0), "Chamfered"), 10.0, 10.0,
         {"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 30.0, "y": 24.0}, "stops": [{"offset": 0.0, "color": c_cyan}, {"offset": 1.0, "color": c_mag}]}}),
        (11, "Scallop Conical", _rect_payload(30.0, 24.0, (5.0, 5.0, 5.0, 5.0), "Scalloped"), 46.0, 10.0,
         {"ConicalGradient": {"center": {"x": 15.0, "y": 12.0}, "angle_deg": 30.0, "stops": [{"offset": 0.0, "color": c_hsl}, {"offset": 0.5, "color": c_spot}, {"offset": 1.0, "color": c_hsl}]}}),
        (12, "Pie SquareGrad", _ellipse_payload(14.0, 12.0, "Pie", 25.0, 310.0), 96.0, 22.0,
         {"SquareGradient": {"center": {"x": 0.0, "y": 0.0}, "size": 24.0, "angle_deg": 15.0, "stops": [{"offset": 0.0, "color": c_lab}, {"offset": 1.0, "color": c_gray}]}}),
        (13, "Arc Radial", _ellipse_payload(14.0, 12.0, "Arc", 20.0, 260.0), 25.0, 58.0,
         {"RadialGradient": {"center": {"x": 0.0, "y": 0.0}, "radius": 14.0, "focal": {"x": -4.0, "y": -4.0}, "stops": [{"offset": 0.0, "color": c_spot}, {"offset": 1.0, "color": c_cyan}]}}),
        (14, "TwoColorPattern Rect", _rect_payload(30.0, 24.0), 46.0, 46.0,
         {"TwoColorPattern": {"fg": c_mag, "bg": c_hsl, "pattern_name": "DiagonalStripe", "tile_w": 6.0, "tile_h": 6.0}}),
        (15, "PostScript Hatch", _rect_payload(30.0, 24.0), 82.0, 46.0,
         {"TextureFill": {"preset": "ps:hatch", "seed": 7, "fg": c_cyan, "bg": c_gray}}),
        (16, "MeshFill Rect", _rect_payload(50.0, 30.0), 10.0, 80.0,
         {"MeshFill": {"rows": 2, "cols": 2, "nodes": [
             {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Rgba": {"r": 1.0, "g": 0.1, "b": 0.1, "a": 1.0}}},
             {"row": 0, "col": 1, "pos": {"x": 50.0, "y": 0.0}, "color": {"Rgba": {"r": 0.1, "g": 0.9, "b": 0.2, "a": 1.0}}},
             {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 30.0}, "color": {"Rgba": {"r": 0.1, "g": 0.2, "b": 0.95, "a": 1.0}}},
             {"row": 1, "col": 1, "pos": {"x": 50.0, "y": 30.0}, "color": {"Rgba": {"r": 0.95, "g": 0.85, "b": 0.1, "a": 1.0}}},
         ]}}),
    ]
    outline = _default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.4, False)
    for oid, name, payload, tx, ty, fill in items:
        doc["objects"][str(oid)] = _make_obj(oid, name, payload, tx=tx, ty=ty, fill=fill, outline=outline)
        doc["pages"][0]["layers"][0]["objects"].append(oid)
    return doc


def build_visual_oracle_card_doc() -> dict[str, Any]:
    """100x100mm (bleed=3mm) RGB card for coree-cli vs Ghostscript vs Poppler pdftoppm parity."""
    doc = _make_base_doc("Tier3 Visual Oracle Card", width_mm=100.0, height_mm=100.0, bleed_mm=3.0, color_mode="Rgb")
    c_blue = {"Rgba": {"r": 0.1, "g": 0.45, "b": 0.85, "a": 1.0}}
    c_orange = {"Rgba": {"r": 0.9, "g": 0.35, "b": 0.1, "a": 1.0}}
    c_blk = {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}
    outline = _default_outline(c_blk, 0.6, False)
    doc["objects"] = {
        "10": _make_obj(10, "Card Rect", _rect_payload(56.0, 38.0, (4.0, 4.0, 4.0, 4.0)), tx=15.0, ty=16.0, fill={"Solid": c_blue}, outline=outline),
        "11": _make_obj(11, "Card Circle", _ellipse_payload(16.0, 16.0), tx=68.0, ty=62.0, fill={"Solid": c_orange}, outline=outline),
        "12": _make_obj(
            12,
            "Card Label",
            {"ArtisticText": {"runs": [{"text": "coree T3", "style": _default_text_style("Noto Sans", 16.0)}], "direction": "Ltr", "align": "Left"}},
            tx=18.0,
            ty=76.0,
            fill="None",
            outline=_default_outline(c_blk, 0.0, False),
        ),
    }
    doc["objects"]["12"]["payload"]["ArtisticText"]["runs"][0]["style"]["fill"] = c_blk
    doc["pages"][0]["layers"][0]["objects"] = [10, 11, 12]
    return doc


def build_effects_baseline_card_doc() -> dict[str, Any]:
    """100x100mm (bleed=0mm) clean single-object baseline card for effect isolation."""
    doc = _make_base_doc("Tier3 Effects Baseline", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    doc["objects"] = {
        "10": _make_obj(
            10,
            "Primary Target",
            _rect_payload(36.0, 30.0),
            tx=28.0,
            ty=30.0,
            fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.45, "b": 0.85, "a": 1.0}}},
            outline=_default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 0.5, False),
        )
    }
    doc["pages"][0]["layers"][0]["objects"] = [10]
    return doc


def build_potrace_adjacent_png_bytes() -> bytes:
    """64x64 deterministic multi-color adjacent-region PNG (touching red & blue blocks on white)."""
    w, h = 64, 64
    buf = bytearray(w * h * 4)
    for y in range(h):
        for x in range(w):
            off = (y * w + x) * 4
            if 8 <= y < 56 and 8 <= x < 32:
                buf[off:off + 4] = bytes((220, 40, 40, 255))
            elif 8 <= y < 56 and 32 <= x < 56:
                buf[off:off + 4] = bytes((30, 80, 220, 255))
            else:
                buf[off:off + 4] = bytes((255, 255, 255, 255))
    return _encode_png_rgba_stdlib(w, h, buf)


def build_centerline_lineart_png_bytes() -> bytes:
    """64x64 deterministic black line-art cross/L-stroke on white for CenterlineTrace."""
    w, h = 64, 64
    buf = bytearray([255, 255, 255, 255] * (w * h))
    for y in range(h):
        for x in range(w):
            if (10 <= x <= 54 and 29 <= y <= 34) or (29 <= x <= 34 and 10 <= y <= 54):
                off = (y * w + x) * 4
                buf[off:off + 4] = bytes((10, 10, 10, 255))
    return _encode_png_rgba_stdlib(w, h, buf)


def ensure_tier3_fixtures(fixtures_dir: Path) -> dict[str, Path]:
    """Create/verify deterministic offline Tier 3 fixtures under `e2e_suite/fixtures/tier3/`."""
    fixtures_dir.mkdir(parents=True, exist_ok=True)
    files: dict[str, bytes] = {
        "pairwise_master.coree": _wrap_coree_zip("Tier3 Pairwise Master", build_pairwise_master_doc()),
        "icc_swatches.coree": _wrap_coree_zip("Tier3 ICC Swatches", build_icc_swatches_doc()),
        "visual_oracle_card.coree": _wrap_coree_zip("Tier3 Visual Oracle Card", build_visual_oracle_card_doc()),
        "effects_baseline_card.coree": _wrap_coree_zip("Tier3 Effects Baseline", build_effects_baseline_card_doc()),
        "potrace_adjacent.png": build_potrace_adjacent_png_bytes(),
        "centerline_cross.png": build_centerline_lineart_png_bytes(),
    }
    out: dict[str, Path] = {}
    for name, payload in files.items():
        p = fixtures_dir / name
        if not p.is_file() or p.read_bytes() != payload:
            p.write_bytes(payload)
        out[name] = p
    return out


def _write_coree(dst: Path, title: str, doc: dict[str, Any]) -> None:
    dst.write_bytes(_wrap_coree_zip(title, doc))


def _t3_fixture(ctx: RunContext, name: str) -> Path:
    t3_dir = ctx.fixtures_dir / "tier3"
    fixtures = ensure_tier3_fixtures(t3_dir)
    return fixtures[name]


def _read_rust_stripped(ctx: RunContext, rel_path: str) -> str:
    p = ctx.repo_root / rel_path
    files: list[Path] = []
    if p.is_file():
        files.append(p)
    if rel_path.endswith(".rs"):
        mod_dir = p.with_suffix("")
        if mod_dir.is_dir():
            files.extend(sorted(mod_dir.rglob("*.rs")))
    if not files:
        return ""
    return "\n".join(
        strip_rust_comments_and_strings(f.read_text(encoding="utf-8", errors="replace"))
        for f in files
    )


def _extract_brace_block(src: str, header_rx: str) -> Optional[str]:
    m = re.search(header_rx, src)
    if not m:
        return None
    brace_start = src.find("{", m.start())
    if brace_start < 0:
        return None
    depth = 0
    for idx in range(brace_start, len(src)):
        ch = src[idx]
        if ch == "{":
            depth += 1
        elif ch == "}":
            depth -= 1
            if depth == 0:
                return src[brace_start:idx + 1]
    return None


def _has_empty_match_arm(src: str, arm_rx: str) -> bool:
    return bool(re.search(rf"{arm_rx}\b[^}}]*?=>\s*(?:\{{\s*\}}|\(\s*\)\s*,)", src))


def _is_trivial_stub_body(body: str) -> bool:
    inner = body.strip("{} \t\r\n")
    cleaned = re.sub(r"(?:let\s+_\s*=\s*[^;]+;\s*)+", "", inner).strip()
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


def _render_coree(ctx: RunContext, coree_path: Path, png_path: Path, work: Path, *, dpi: int = 72, view: str = "enhanced", extra_args: Optional[list[str]] = None) -> Optional[str]:
    cmd = [str(ctx.cli_bin), "render", str(coree_path), str(png_path), "--dpi", str(dpi), "--view", view]
    if extra_args:
        cmd.extend(extra_args)
    res = run_cmd(cmd, cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if res.returncode != 0 or not png_path.is_file():
        return f"render failed (rc={res.returncode}): {res.stderr.strip()}"
    return None


def _sample_rgba_box_avg(png_bytes: bytes, page_w_mm: float, page_h_mm: float, x_mm: float, y_mm: float, radius_px: int = 2) -> tuple[float, float, float, float]:
    pw, ph, buf = _decode_png_rgba_bytes_stdlib(png_bytes)
    cx = max(0, min(pw - 1, int(round((x_mm / page_w_mm) * pw))))
    cy = max(0, min(ph - 1, int(round((y_mm / page_h_mm) * ph))))
    sr = sg = sb = sa = cnt = 0
    for dy in range(-radius_px, radius_px + 1):
        yy = max(0, min(ph - 1, cy + dy))
        for dx in range(-radius_px, radius_px + 1):
            xx = max(0, min(pw - 1, cx + dx))
            off = (yy * pw + xx) * 4
            sr += buf[off]
            sg += buf[off + 1]
            sb += buf[off + 2]
            sa += buf[off + 3]
            cnt += 1
    del buf
    return (sr / cnt, sg / cnt, sb / cnt, sa / cnt)


# ---------------------------------------------------------------------------
# BASELINE TESTS (smoke=True, target_wave="baseline") — Must PASS on v0.3.1
# ---------------------------------------------------------------------------

def _test_t3_base_icc_parser_and_ciede2000(ctx: RunContext) -> TestOutcome:
    if ctx.icc_dir is None:
        return TestOutcome("FAIL", "System ICC directory /usr/share/color/icc/ not found")

    # 1. Verify Sharma-Wu-Dalal (2005) CIEDE2000 mathematical reference pair
    de_ref = ciede2000((50.0, 2.6772, -79.7751), (50.0, 0.0, -82.7485))
    if abs(de_ref - 2.0425) > 0.0005:
        return TestOutcome("FAIL", f"CIEDE2000 reference check failed: got {de_ref:.6f}, expected 2.0425")

    # 2. Parse system ICC v2 & v4 profiles (mft1, mft2, para, curv, chad) + verify mAB/mBA
    required_profiles = [
        ("colord/FOGRA39L_coated.icc", "CMYK", 2),
        ("colord/SWOP_TR003_coated_3.icc", "CMYK", 2),
        ("colord/AdobeRGB1998.icc", "RGB ", 4),
        ("colord/sRGB.icc", "RGB ", 4),
        ("ghostscript/default_cmyk.icc", "CMYK", 2),
        ("ghostscript/default_rgb.icc", "RGB ", 2),
        ("ghostscript/default_gray.icc", "GRAY", 2),
    ]
    parsed: dict[str, IccProfileOracle] = {}
    for rel, expected_cs, expected_maj in required_profiles:
        p = ctx.icc_dir / rel
        if not p.is_file():
            return TestOutcome("FAIL", f"Missing required system ICC profile: {p}")
        try:
            prof = IccProfileOracle(p.read_bytes())
        except _PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"Failed to parse system ICC {rel}: {exc}")
        if prof.color_space != expected_cs or prof.version_major != expected_maj:
            return TestOutcome("FAIL", f"ICC {rel} metadata mismatch: cs={prof.color_space!r} ver={prof.version_major}")
        parsed[rel] = prof

    # Sweep all non-nmcl system ICC files (50/52)
    sweep_parsed = 0
    for p in sorted(ctx.icc_dir.rglob("*.icc")):
        raw_icc = p.read_bytes()
        if len(raw_icc) >= 16 and raw_icc[12:16] == b"nmcl":
            continue
        try:
            IccProfileOracle(raw_icc)
            sweep_parsed += 1
        except _PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"Failed to parse system ICC {p.name}: {exc}")
    if sweep_parsed < 45:
        return TestOutcome("FAIL", f"Expected >= 45 system ICC profiles parsed, got {sweep_parsed}")

    # Verify spec-compliant ICC v4 mAB / mBA multi-process LUT evaluation (identity 2x2x2 CLUT + curves + matrix)
    def _build_identity_v4_lut_tag(sig: bytes) -> bytes:
        hdr = bytearray(32)
        hdr[0:4] = sig
        hdr[8] = 3
        hdr[9] = 3
        b_off = 32
        mat_off = b_off + 36
        clut_off = mat_off + 48
        struct.pack_into(">IIIII", hdr, 12, b_off, mat_off, 0, clut_off, 0)
        curves = (b"curv\x00\x00\x00\x00\x00\x00\x00\x00") * 3  # 3x12=36 bytes
        mat = bytearray(48)
        for idx, val in enumerate((1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0)):
            struct.pack_into(">i", mat, idx * 4, int(round(val * 65536.0)))
        clut_hdr = bytearray(20)
        clut_hdr[0:3] = b"\x02\x02\x02"
        clut_hdr[16] = 2  # 16-bit
        clut_pts = bytearray()
        for r in (0, 65535):
            for g in (0, 65535):
                for b in (0, 65535):
                    clut_pts.extend(struct.pack(">HHH", r, g, b))
        return bytes(hdr) + curves + bytes(mat) + bytes(clut_hdr) + bytes(clut_pts)

    for vsig in (b"mAB ", b"mBA "):
        vlut = _IccV4Lut(_build_identity_v4_lut_tag(vsig))
        vout = vlut.eval([0.25, 0.5, 0.75])
        if any(abs(a - b) > 1e-4 for a, b in zip(vout, (0.25, 0.5, 0.75))):
            return TestOutcome("FAIL", f"ICC v4 {vsig!r} LUT evaluation failed: got {vout}")

    # Verify v4 chad tag parsed on colord/sRGB.icc & AdobeRGB1998.icc
    if parsed["colord/sRGB.icc"].chad is None or parsed["colord/AdobeRGB1998.icc"].chad is None:
        return TestOutcome("FAIL", "Expected v4 chad sf32 matrix on colord/sRGB.icc and AdobeRGB1998.icc")

    # Verify sRGB.icc (1,1,1) -> D50 white (DE < 0.2) for intent=1 & intent=3, and (0,0,0) -> L* < 0.1
    srgb_prof = parsed["colord/sRGB.icc"]
    white_lab = srgb_prof.rgb_to_lab_d50((1.0, 1.0, 1.0), intent=1)
    white_abs_lab = srgb_prof.rgb_to_lab_d50((1.0, 1.0, 1.0), intent=3)
    black_lab = srgb_prof.rgb_to_lab_d50((0.0, 0.0, 0.0), intent=1)
    de_srgb_w = ciede2000(white_lab, (100.0, 0.0, 0.0))
    de_srgb_abs = ciede2000(white_abs_lab, (100.0, 0.0, 0.0))
    if de_srgb_w >= 0.2 or de_srgb_abs >= 0.1 or black_lab[0] >= 0.1:
        return TestOutcome("FAIL", f"sRGB.icc endpoint check failed: white_de={de_srgb_w:.4f}, abs_de={de_srgb_abs:.4f}, black_L={black_lab[0]:.4f}")

    # Verify FOGRA39L & SWOP CMYK paper white (0,0,0,0) relative colorimetric -> near D50 white (DE < 1.5),
    # and Adobe/ICC BPC shadow black lift on (0,0,0,1)
    for cmyk_name in ("colord/FOGRA39L_coated.icc", "colord/SWOP_TR003_coated_3.icc", "ghostscript/default_cmyk.icc"):
        pw_lab = parsed[cmyk_name].cmyk_to_lab_d50((0.0, 0.0, 0.0, 0.0), intent=1)
        de_pw = ciede2000(pw_lab, (100.0, 0.0, 0.0))
        if de_pw >= 2.5 or pw_lab[0] < 95.0:
            return TestOutcome("FAIL", f"{cmyk_name} paper white failed: lab={pw_lab}, de={de_pw:.4f}")
    k_nob = parsed["colord/FOGRA39L_coated.icc"].cmyk_to_lab_d50((0.0, 0.0, 0.0, 1.0), intent=1, bpc=False)
    k_bpc = parsed["colord/FOGRA39L_coated.icc"].cmyk_to_lab_d50((0.0, 0.0, 0.0, 1.0), intent=1, bpc=True)
    if k_bpc[0] >= k_nob[0] - 2.0:
        return TestOutcome("FAIL", f"FOGRA39L BPC failed to map black point toward L*=0: nob={k_nob}, bpc={k_bpc}")

    # Verify FOGRA39L vs SWOP diverge on CMYK cyan (1,0,0,0) & grayscale default_gray.icc
    de_fogra_swop = ciede2000(
        parsed["colord/FOGRA39L_coated.icc"].cmyk_to_lab_d50((1.0, 0.0, 0.0, 0.0), intent=1),
        parsed["colord/SWOP_TR003_coated_3.icc"].cmyk_to_lab_d50((1.0, 0.0, 0.0, 0.0), intent=1),
    )
    gray_white = xyz_d50_to_lab(parsed["ghostscript/default_gray.icc"].gray_to_pcs_xyz(1.0, intent=1))
    if de_fogra_swop <= 0.5 or gray_white[0] < 99.0:
        return TestOutcome("FAIL", f"FOGRA/SWOP divergence or Gray TRC failed: de={de_fogra_swop:.3f}, gray_L={gray_white[0]:.2f}")

    return TestOutcome(
        "PASS",
        f"Pure-Python ICC v2/v4 parser ({sweep_parsed} system profiles + mAB/mBA LUTs) + CIEDE2000 (ref={de_ref:.4f}, FOGRA/SWOP cyan dE={de_fogra_swop:.2f}) verified",
        metrics={"ciede2000_ref": round(de_ref, 4), "fogra_swop_cyan_de": round(de_fogra_swop, 3), "icc_profiles_parsed": sweep_parsed},
    )


def _test_t3_base_pairwise_colors_shapes_fills(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-BASE-PAIRWISE-FILLS")
    src = _t3_fixture(ctx, "pairwise_master.coree")
    master_png = work / "pairwise_master.png"
    err = _render_coree(ctx, src, master_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"Pairwise master render failed: {err}")
    try:
        info = parse_png_info(master_png.read_bytes())
    except _PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"Pairwise master PNG malformed: {exc}")
    if info["non_white_pixels"] < 500 or info["unique_colors_sample"] <= 16 or info["luma_stddev"] <= 5.0:
        return TestOutcome("FAIL", f"Pairwise master rendered flat/empty: {info}")

    # Verify individual pairwise variations (Rectangle corner styles, Ellipse Arc/Pie, Fills) differ semantically
    _, base_doc = inspect_coree_zip(src)
    base_doc["objects"] = {"10": dict(base_doc["objects"]["10"])}
    base_doc["objects"]["10"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 30.0, 30.0]}
    base_doc["objects"]["10"]["payload"] = _rect_payload(60.0, 60.0, (12.0, 12.0, 12.0, 12.0), "Rounded")
    base_doc["objects"]["10"]["fill"] = {"Solid": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}}
    base_doc["pages"][0]["layers"][0]["objects"] = [10]

    variants: list[tuple[str, dict[str, Any], dict[str, Any]]] = [
        ("rounded_solid", _rect_payload(60.0, 60.0, (12.0, 12.0, 12.0, 12.0), "Rounded"), {"Solid": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}}),
        ("chamfer_linear", _rect_payload(60.0, 60.0, (12.0, 12.0, 12.0, 12.0), "Chamfered"), {"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 60.0, "y": 60.0}, "stops": [{"offset": 0.0, "color": {"Lab": {"l": 65.0, "a_ch": -40.0, "b_ch": 10.0, "alpha": 1.0}}}, {"offset": 1.0, "color": {"Spot": {"name": "SpotMagenta", "tint": 1.0, "fallback_cmyk": [0.0, 1.0, 0.0, 0.0], "alpha": 1.0}}}]}}),
        ("scallop_conical", _rect_payload(60.0, 60.0, (12.0, 12.0, 12.0, 12.0), "Scalloped"), {"ConicalGradient": {"center": {"x": 30.0, "y": 30.0}, "angle_deg": 0.0, "stops": [{"offset": 0.0, "color": {"Hsl": {"h": 210.0, "s": 0.9, "l": 0.5, "a": 1.0}}}, {"offset": 0.5, "color": {"Gray": {"v": 0.2, "a": 1.0}}}, {"offset": 1.0, "color": {"Hsl": {"h": 210.0, "s": 0.9, "l": 0.5, "a": 1.0}}}]}}),
        ("ellipse_pie_square", _ellipse_payload(28.0, 28.0, "Pie", 30.0, 300.0), {"SquareGradient": {"center": {"x": 0.0, "y": 0.0}, "size": 50.0, "angle_deg": 20.0, "stops": [{"offset": 0.0, "color": {"Rgba": {"r": 1.0, "g": 0.8, "b": 0.0, "a": 1.0}}}, {"offset": 1.0, "color": {"Cmyk": {"c": 0.8, "m": 0.1, "y": 0.9, "k": 0.0, "a": 1.0}}}]}}),
        ("ellipse_arc_ps", _ellipse_payload(28.0, 28.0, "Arc", 20.0, 240.0), {"TextureFill": {"preset": "ps:halftone_dots", "seed": 1, "fg": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.8, "a": 1.0}}, "bg": {"Rgba": {"r": 0.95, "g": 0.95, "b": 0.95, "a": 1.0}}}}),
    ]
    pngs: dict[str, Path] = {}
    for label, payload, fill in variants:
        vdoc = json.loads(json.dumps(base_doc))
        vdoc["objects"]["10"]["payload"] = payload
        vdoc["objects"]["10"]["fill"] = fill
        if label.startswith("ellipse"):
            vdoc["objects"]["10"]["transform"] = {"m": [1.0, 0.0, 0.0, 1.0, 60.0, 60.0]}
        in_p = work / f"{label}.coree"
        out_p = work / f"{label}.png"
        _write_coree(in_p, label, vdoc)
        rerr = _render_coree(ctx, in_p, out_p, work, dpi=72)
        if rerr:
            return TestOutcome("FAIL", f"Pairwise variant {label} render failed: {rerr}")
        vinfo = parse_png_info(out_p.read_bytes())
        if vinfo["non_white_pixels"] < 80:
            return TestOutcome("FAIL", f"Pairwise variant {label} rendered blank: {vinfo}")
        pngs[label] = out_p

    # Verify pairwise distinctness across all pairs
    keys = list(pngs.keys())
    min_diff = 999.0
    for i in range(len(keys)):
        for j in range(i + 1, len(keys)):
            cmp = compare_png_pixels(pngs[keys[i]], pngs[keys[j]])
            min_diff = min(min_diff, cmp["mean_abs_diff"])
            if cmp["mean_abs_diff"] <= 0.5:
                return TestOutcome("FAIL", f"Pairwise variants {keys[i]} vs {keys[j]} indistinguishable: {cmp}")

    return TestOutcome(
        "PASS",
        f"Pairwise ColorModels x ParametricShapes x Fills (5 variants + master card, min_pair_diff={min_diff:.2f}) passed",
        metrics={"master_unique_colors": info["unique_colors_sample"], "min_pair_diff": round(min_diff, 2)},
    )


def _test_t3_base_outline_pen_pairwise(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-BASE-OUTLINE-PEN")
    doc = _make_base_doc("Tier3 Outline Pen", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    line_path = {
        "Path": {
            "subpaths": [
                {
                    "nodes": [
                        {"pos": {"x": 15.0, "y": 50.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                        {"pos": {"x": 85.0, "y": 50.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                    ],
                    "closed": False,
                }
            ]
        }
    }
    base_ol = _default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 3.0, False)
    doc["objects"] = {"10": _make_obj(10, "Stroke Probe", line_path, fill="None", outline=base_ol)}
    doc["pages"][0]["layers"][0]["objects"] = [10]

    cases = {
        "solid": base_ol,
        "dashed_arrows": {**base_ol, "dash_pattern": [6.0, 4.0], "start_arrow": "Circle", "end_arrow": "Triangle", "cap": "Round"},
        "calligraphy": {**base_ol, "width_mm": 6.0, "calligraphy_angle_deg": 45.0, "calligraphy_stretch": 0.25},
        "hairline": {**base_ol, "width_mm": 0.1, "is_hairline": True},
    }
    rendered: dict[str, Path] = {}
    for name, ol in cases.items():
        d = json.loads(json.dumps(doc))
        d["objects"]["10"]["outline"] = ol
        in_p = work / f"ol_{name}.coree"
        out_p = work / f"ol_{name}.png"
        _write_coree(in_p, f"Outline {name}", d)
        err = _render_coree(ctx, in_p, out_p, work, dpi=96)
        if err:
            return TestOutcome("FAIL", f"Outline {name} render failed: {err}")
        info = parse_png_info(out_p.read_bytes())
        if info["non_white_pixels"] < 30:
            return TestOutcome("FAIL", f"Outline {name} rendered blank: {info}")
        rendered[name] = out_p

    # Also test behind_fill=True vs False on a filled rectangle with thick red stroke
    rect_doc = _make_base_doc("Tier3 BehindFill", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    thick_red = _default_outline({"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}, 8.0, False)
    rect_doc["objects"] = {"10": _make_obj(10, "Filled Box", _rect_payload(40.0, 40.0), tx=30.0, ty=30.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.3, "b": 0.9, "a": 1.0}}}, outline=thick_red)}
    rect_doc["pages"][0]["layers"][0]["objects"] = [10]
    for bf in (False, True):
        rd = json.loads(json.dumps(rect_doc))
        rd["objects"]["10"]["outline"]["behind_fill"] = bf
        in_p = work / f"bf_{bf}.coree"
        out_p = work / f"bf_{bf}.png"
        _write_coree(in_p, f"BehindFill {bf}", rd)
        err = _render_coree(ctx, in_p, out_p, work, dpi=96)
        if err:
            return TestOutcome("FAIL", f"Outline behind_fill={bf} render failed: {err}")
        rendered[f"bf_{bf}"] = out_p

    diffs = {
        "solid_vs_dashed_arrows": compare_png_pixels(rendered["solid"], rendered["dashed_arrows"])["mean_abs_diff"],
        "solid_vs_calligraphy": compare_png_pixels(rendered["solid"], rendered["calligraphy"])["mean_abs_diff"],
        "solid_vs_hairline": compare_png_pixels(rendered["solid"], rendered["hairline"])["mean_abs_diff"],
        "behind_fill_diff": compare_png_pixels(rendered["bf_False"], rendered["bf_True"])["mean_abs_diff"],
    }
    for k, val in diffs.items():
        if val <= 0.25:
            return TestOutcome("FAIL", f"OutlineStyle check {k} showed no visual difference ({val:.4f})")

    return TestOutcome("PASS", f"OutlinePen pairwise (dashes, arrowheads, calligraphy, hairline, behind_fill) verified", metrics={k: round(v, 2) for k, v in diffs.items()})


def _test_t3_base_blockshadow_distort_effects(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-BASE-EFFECTS")
    src = _t3_fixture(ctx, "effects_baseline_card.coree")
    ctrl_png = work / "ctrl.png"
    err = _render_coree(ctx, src, ctrl_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"Effects baseline control render failed: {err}")
    _, base_doc = inspect_coree_zip(src)

    # 1. BlockShadow (FEAT-44): solid extruded dark shadow offset (+12, +12) mm
    bs_doc = json.loads(json.dumps(base_doc))
    bs_doc["objects"]["10"]["effects"] = [{
        "BlockShadow": {
            "dx_mm": 12.0,
            "dy_mm": 12.0,
            "color": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.1, "a": 1.0}},
            "simplify": False,
            "overprint": False,
        }
    }]
    bs_in = work / "block_shadow.coree"
    bs_png = work / "block_shadow.png"
    _write_coree(bs_in, "BlockShadow", bs_doc)
    err = _render_coree(ctx, bs_in, bs_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"BlockShadow render failed: {err}")
    bs_cmp = compare_png_pixels(ctrl_png, bs_png)
    if bs_cmp["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", f"BlockShadow produced no visual change vs control: {bs_cmp}")
    # Sample shadow region at (68, 64) mm (outside rect [28..64, 30..60], inside offset [40..76, 42..72])
    sh_rgba = _sample_rgba_box_avg(bs_png.read_bytes(), 100.0, 100.0, 68.0, 64.0)
    if sh_rgba[0] > 80 or sh_rgba[1] > 80 or sh_rgba[2] > 80:
        return TestOutcome("FAIL", f"BlockShadow expected dark extruded pixels at (68,64)mm, got rgba={sh_rgba}")

    # 2. Distort (FEAT-47): PushPull, Zipper (fractional frequency so sin(idx*freq*pi) != 0), Twister
    dist_diffs: dict[str, float] = {}
    for dkind, dfreq in (("PushPull", 4.0), ("Zipper", 1.5), ("Twister", 4.0)):
        ddoc = json.loads(json.dumps(base_doc))
        ddoc["objects"]["10"]["effects"] = [{
            "Distort": {
                "kind": dkind,
                "amplitude": 8.0,
                "frequency": dfreq,
                "center": {"x": 18.0, "y": 15.0},
            }
        }]
        din = work / f"distort_{dkind}.coree"
        dpng = work / f"distort_{dkind}.png"
        _write_coree(din, f"Distort {dkind}", ddoc)
        err = _render_coree(ctx, din, dpng, work, dpi=72)
        if err:
            return TestOutcome("FAIL", f"Distort::{dkind} render failed: {err}")
        dcmp = compare_png_pixels(ctrl_png, dpng)
        if dcmp["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", f"Distort::{dkind} produced no visual deformation vs control: {dcmp}")
        dist_diffs[dkind] = round(float(dcmp["mean_abs_diff"]), 2)

    return TestOutcome(
        "PASS",
        f"BlockShadow (FEAT-44, diff={bs_cmp['mean_abs_diff']:.2f}) & Distort PushPull/Zipper/Twister (FEAT-47) verified",
        metrics={"block_shadow_diff": round(float(bs_cmp["mean_abs_diff"]), 2), "distort_diffs": dist_diffs},
    )


def _test_t3_base_eleven_lenses_and_viewpoint(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-BASE-LENSES")
    doc = _make_base_doc("Tier3 Lenses", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    # Cyan rect on left [10..55, 20..80], Magenta ellipse on right [75, 50], lens [15..85, 15..85]
    doc["objects"] = {
        "10": _make_obj(10, "Cyan Block", _rect_payload(45.0, 60.0), tx=10.0, ty=20.0, fill={"Solid": {"Rgba": {"r": 0.0, "g": 0.9, "b": 0.95, "a": 1.0}}}),
        "11": _make_obj(11, "Magenta Disc", _ellipse_payload(18.0, 18.0), tx=72.0, ty=50.0, fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.7, "a": 1.0}}}),
        "12": _make_obj(12, "Lens Window", _rect_payload(70.0, 70.0), tx=15.0, ty=15.0, fill="None", outline=_default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 0.3, False)),
    }
    doc["pages"][0]["layers"][0]["objects"] = [10, 11, 12]

    ctrl_in = work / "lens_ctrl.coree"
    ctrl_png = work / "lens_ctrl.png"
    _write_coree(ctrl_in, "Lens Control", doc)
    err = _render_coree(ctx, ctrl_in, ctrl_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"Lens control render failed: {err}")

    lenses: list[tuple[str, str, Any]] = [
        ("FEAT-52", "Brighten", {"Brighten": {"rate_pct": 60.0}}),
        ("FEAT-53", "ColorAdd", {"ColorAdd": {"color": {"Rgba": {"r": 0.9, "g": 0.2, "b": 0.1, "a": 1.0}}, "rate_pct": 65.0}}),
        ("FEAT-54", "ColorLimit", {"ColorLimit": {"color": {"Rgba": {"r": 0.2, "g": 0.9, "b": 0.3, "a": 1.0}}, "rate_pct": 85.0}}),
        ("FEAT-55", "CustomColorMap", {"CustomColorMap": {"from_color": {"Rgba": {"r": 0.1, "g": 0.0, "b": 0.4, "a": 1.0}}, "to_color": {"Rgba": {"r": 1.0, "g": 0.9, "b": 0.2, "a": 1.0}}, "palette_mode": "ForwardRainbow"}}),
        ("FEAT-56", "FishEye", {"FishEye": {"rate_pct": 180.0}}),
        ("FEAT-57", "HeatMap", {"HeatMap": {"palette_rotation_deg": 45.0}}),
        ("FEAT-58", "Invert", "Invert"),
        ("FEAT-59", "Magnify", {"Magnify": {"amount": 2.5}}),
        ("FEAT-60", "TintedGrayscale", {"TintedGrayscale": {"tint": {"Rgba": {"r": 0.35, "g": 0.2, "b": 0.1, "a": 1.0}}}}),
        ("FEAT-61", "Transparency", {"Transparency": {"color": {"Rgba": {"r": 0.95, "g": 0.85, "b": 0.1, "a": 1.0}}, "rate_pct": 60.0}}),
        ("FEAT-62", "Wireframe", {"Wireframe": {"override_outline": {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, "override_fill": {"Rgba": {"r": 1.0, "g": 0.95, "b": 0.4, "a": 1.0}}}}),
    ]
    diffs: dict[str, float] = {}
    lens_bytes_by_name: dict[str, bytes] = {}
    for fid, lname, lkind in lenses:
        ld = json.loads(json.dumps(doc))
        ld["objects"]["12"]["lens"] = {"kind": lkind, "frozen": False, "viewpoint": None, "remove_face": False}
        in_p = work / f"lens_{lname}.coree"
        out_p = work / f"lens_{lname}.png"
        _write_coree(in_p, f"Lens {lname}", ld)
        err = _render_coree(ctx, in_p, out_p, work, dpi=72)
        if err:
            return TestOutcome("FAIL", f"{fid} ({lname}) render failed: {err}")
        raw_out = out_p.read_bytes()
        cmp = compare_png_pixels(ctrl_png, out_p)
        if cmp["mean_abs_diff"] <= 0.2:
            return TestOutcome("FAIL", f"{fid} ({lname}) lens produced no pixel difference vs control: {cmp}")
        diffs[lname] = round(float(cmp["mean_abs_diff"]), 2)
        lens_bytes_by_name[lname] = raw_out

    # Semantic color-channel verification on Invert & Brighten at (35, 50) mm (inside cyan block & lens)
    ctrl_px = _sample_rgba_box_avg(ctrl_png.read_bytes(), 100.0, 100.0, 35.0, 50.0)
    inv_px = _sample_rgba_box_avg(lens_bytes_by_name["Invert"], 100.0, 100.0, 35.0, 50.0)
    br_px = _sample_rgba_box_avg(lens_bytes_by_name["Brighten"], 100.0, 100.0, 35.0, 50.0)
    if inv_px[0] <= ctrl_px[0] + 150 or inv_px[2] >= ctrl_px[2] - 150:
        return TestOutcome("FAIL", f"Invert lens failed semantic cyan->red check: ctrl={ctrl_px}, inv={inv_px}")
    if (br_px[0] + br_px[1] + br_px[2]) <= (ctrl_px[0] + ctrl_px[1] + ctrl_px[2]) + 30:
        return TestOutcome("FAIL", f"Brighten lens failed semantic luminance increase: ctrl={ctrl_px}, bright={br_px}")

    # Viewpoint offset check: Magnify with viewpoint shifted to magenta disc at (72, 50) vs default None
    vp_doc = json.loads(json.dumps(doc))
    vp_doc["objects"]["12"]["lens"] = {"kind": {"Magnify": {"amount": 2.5}}, "frozen": False, "viewpoint": {"x": 72.0, "y": 50.0}, "remove_face": False}
    vp_in = work / "lens_vp.coree"
    vp_png = work / "lens_vp.png"
    _write_coree(vp_in, "Lens Viewpoint", vp_doc)
    err = _render_coree(ctx, vp_in, vp_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"Magnify viewpoint render failed: {err}")
    vp_cmp = compare_png_pixels(work / "lens_Magnify.png", vp_png)
    if vp_cmp["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"Lens viewpoint X/Y offset produced no visual shift: {vp_cmp}")

    return TestOutcome(
        "PASS",
        "All 11 Lenses (FEAT-52..62) semantic channel/luma assertions + viewpoint offset verified",
        metrics={"lens_diffs": diffs, "viewpoint_diff": round(float(vp_cmp["mean_abs_diff"]), 2)},
    )


def _test_t3_base_visual_oracles_pdf_eps_gs_trace(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-BASE-VISUAL-ORACLES")
    src = _t3_fixture(ctx, "visual_oracle_card.coree")

    # 1. Baseline 4 ViewModes (wireframe vs normal vs enhanced vs pixels) + FEAT-05
    view_pngs: dict[str, Path] = {}
    for vm in ("wireframe", "normal", "enhanced", "pixels"):
        vp = work / f"view_{vm}.png"
        err = _render_coree(ctx, src, vp, work, dpi=96, view=vm)
        if err:
            return TestOutcome("FAIL", f"ViewMode {vm} render failed: {err}")
        vinfo = parse_png_info(vp.read_bytes())
        if vinfo["non_white_pixels"] < 50:
            return TestOutcome("FAIL", f"ViewMode {vm} rendered blank: {vinfo}")
        view_pngs[vm] = vp

    wf_info = parse_png_info(view_pngs["wireframe"].read_bytes())
    enh_info = parse_png_info(view_pngs["enhanced"].read_bytes())
    wf_diff = compare_png_pixels(view_pngs["wireframe"], view_pngs["enhanced"])
    if wf_diff["mean_abs_diff"] <= 2.0 or wf_info["non_white_pixels"] >= enh_info["non_white_pixels"]:
        return TestOutcome("FAIL", f"Wireframe vs Enhanced view invariant failed: wf={wf_info['non_white_pixels']}, enh={enh_info['non_white_pixels']}, diff={wf_diff}")

    # 2. PDF export (unmarked vs --crop-marks FEAT-75) + Poppler pdftoppm + Ghostscript -dUseTrimBox vs coree render
    out_pdf = work / "card_unmarked.pdf"
    out_marks_pdf = work / "card_marks.pdf"
    r_u = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_pdf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    r_m = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_marks_pdf), "--cmyk", "--crop-marks"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_u.returncode != 0 or r_m.returncode != 0 or not out_pdf.is_file() or not out_marks_pdf.is_file():
        return TestOutcome("FAIL", f"PDF export failed: {r_u.stderr} {r_m.stderr}")
    marks_struct = parse_pdf_structure(out_marks_pdf.read_bytes())
    if not (marks_struct["has_xref"] and marks_struct["has_eof"] and marks_struct["has_output_intents"]):
        return TestOutcome("FAIL", f"--crop-marks PDF missing xref/EOF/OutputIntents: {marks_struct}")

    ppm_u = run_pdftoppm(out_pdf, work / "ppm_u", dpi=96, timeout_scale=ctx.timeout_scale)
    ppm_m = run_pdftoppm(out_marks_pdf, work / "ppm_m", dpi=96, timeout_scale=ctx.timeout_scale)
    if not ppm_u["ok"] or not ppm_m["ok"] or ppm_m["png_info"]["width"] <= ppm_u["png_info"]["width"]:
        return TestOutcome("FAIL", f"Poppler pdftoppm or --crop-marks (FEAT-75) check failed: u={ppm_u}, m={ppm_m}")

    ppm_trim = work / "ppm_trim.png"
    crop_png_center(ppm_u["out_png"], enh_info["width"], enh_info["height"], ppm_trim)
    ppm_cmp = compare_png_pixels(view_pngs["enhanced"], ppm_trim)
    blank_png = work / "blank.png"
    build_blank_white_png(enh_info["width"], enh_info["height"], blank_png)
    blank_cmp = compare_png_pixels(view_pngs["enhanced"], blank_png)
    if not ppm_cmp["size_match"] or ppm_cmp["mean_abs_diff"] >= 8.0 or ppm_cmp["non_white_iou"] < 0.85:
        return TestOutcome("FAIL", f"coree render vs Poppler pdftoppm TrimBox oracle failed: {ppm_cmp}")
    if ppm_cmp["mean_abs_diff"] >= blank_cmp["mean_abs_diff"] * 0.6:
        return TestOutcome("FAIL", f"Poppler visual oracle failed to beat blank-white control: pdf={ppm_cmp}, blank={blank_cmp}")

    # Ghostscript direct -dUseTrimBox oracle
    gs_trim = work / "gs_trim.png"
    r_gs = run_cmd(
        ["gs", "-q", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-dUseTrimBox", "-sDEVICE=png16m", "-r96", f"-sOutputFile={gs_trim}", str(out_pdf)],
        cwd=work,
        timeout=25.0,
        timeout_scale=ctx.timeout_scale,
    )
    if r_gs.returncode != 0 or not gs_trim.is_file():
        return TestOutcome("FAIL", f"Ghostscript -dUseTrimBox failed: {r_gs.stderr}")
    gs_cmp = compare_png_pixels(view_pngs["enhanced"], gs_trim)
    if not gs_cmp["size_match"] or gs_cmp["mean_abs_diff"] >= 8.0 or gs_cmp["non_white_iou"] < 0.85:
        return TestOutcome("FAIL", f"coree render vs Ghostscript -dUseTrimBox diverged: {gs_cmp}")

    # Ghostscript EPS validation
    out_eps = work / "card.eps"
    r_eps = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_eps)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_eps.returncode != 0 or not out_eps.is_file():
        return TestOutcome("FAIL", f"EPS export failed: {r_eps.stderr}")
    gs_eps = run_ghostscript_render(out_eps, work / "gs_eps.png", dpi=96, device="pngalpha", timeout_scale=ctx.timeout_scale)
    if not gs_eps["ok"] or gs_eps["png_info"]["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"Ghostscript EPS render failed: {gs_eps}")

    # 3. Baseline TextOnPath (FEAT-39) render vs no-text control
    _, tp_doc = inspect_coree_zip(src)
    tp_doc["objects"]["20"] = _make_obj(
        20,
        "Wave Path",
        {"Path": {"subpaths": [{"nodes": [{"pos": {"x": 15.0, "y": 50.0}, "handle_in": None, "handle_out": {"x": 35.0, "y": 20.0}, "kind": "Smooth"}, {"pos": {"x": 85.0, "y": 50.0}, "handle_in": {"x": 65.0, "y": 20.0}, "handle_out": None, "kind": "Smooth"}], "closed": False}]}},
        fill="None",
        outline=_default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.3, False),
    )
    tp_doc["pages"][0]["layers"][0]["objects"].append(20)
    tp_ctrl_in = work / "tp_ctrl.coree"
    tp_ctrl_png = work / "tp_ctrl.png"
    _write_coree(tp_ctrl_in, "TextOnPath Control", tp_doc)
    err = _render_coree(ctx, tp_ctrl_in, tp_ctrl_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"TextOnPath control render failed: {err}")
    tp_doc["objects"]["21"] = _make_obj(
        21,
        "Path Text",
        {"TextOnPath": {"text": {"runs": [{"text": "CURVED PATH TEXT", "style": _default_text_style("Noto Sans", 16.0)}], "direction": "Ltr", "align": "Left"}, "path_id": 20, "offset_mm": 4.0, "distance_mm": 2.0, "flip_vertical": False, "flip_horizontal": False}},
        fill="None",
    )
    tp_doc["pages"][0]["layers"][0]["objects"].append(21)
    tp_in = work / "tp.coree"
    tp_png = work / "tp.png"
    _write_coree(tp_in, "TextOnPath Target", tp_doc)
    err = _render_coree(ctx, tp_in, tp_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"TextOnPath render failed: {err}")
    tp_cmp = compare_png_pixels(tp_ctrl_png, tp_png)
    if tp_cmp["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"TextOnPath (FEAT-39) produced no visual change vs path-only control: {tp_cmp}")

    # 4. Baseline PowerTRACE Quick & Centerline (FEAT-70) -> SVG -> render
    adj_png = _t3_fixture(ctx, "potrace_adjacent.png")
    line_png = _t3_fixture(ctx, "centerline_cross.png")
    for tmode, src_img in (("quick", adj_png), ("centerline", line_png)):
        tr_svg = work / f"trace_{tmode}.svg"
        tr_ren = work / f"trace_{tmode}_ren.png"
        rt = run_cmd([str(ctx.cli_bin), "trace", str(src_img), str(tr_svg), "--mode", tmode, "--colors", "4"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if rt.returncode != 0 or not tr_svg.is_file():
            return TestOutcome("FAIL", f"trace --mode {tmode} failed: {rt.stderr}")
        svg_s = parse_svg_structure(tr_svg.read_bytes())
        if svg_s["vector_elements"] < 1:
            return TestOutcome("FAIL", f"trace --mode {tmode} produced 0 SVG vector elements: {svg_s}")
        err = _render_coree(ctx, tr_svg, tr_ren, work, dpi=96)
        if err or parse_png_info(tr_ren.read_bytes())["non_white_pixels"] < 10:
            return TestOutcome("FAIL", f"trace --mode {tmode} SVG render blank or failed: {err}")

    return TestOutcome(
        "PASS",
        f"ViewModes (FEAT-05), Printers Marks (FEAT-75), TextOnPath (FEAT-39), Quick/Centerline Trace (FEAT-70), Poppler (diff={ppm_cmp['mean_abs_diff']:.2f}, IoU={ppm_cmp['non_white_iou']:.2f}) & GS TrimBox (diff={gs_cmp['mean_abs_diff']:.2f}) verified",
        metrics={
            "pdftoppm_diff": round(float(ppm_cmp["mean_abs_diff"]), 2),
            "pdftoppm_iou": round(float(ppm_cmp["non_white_iou"]), 3),
            "gs_trim_diff": round(float(gs_cmp["mean_abs_diff"]), 2),
        },
    )


# ---------------------------------------------------------------------------
# FUTURE-WAVE TESTS (smoke=False, target_wave="W2".."W5") — Honest FAIL until implemented
# ---------------------------------------------------------------------------

def _test_t3_w2_bspline_artistic_media_clone_symmetry(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W2-BSPLINE-MEDIA-CLONE")
    doc = _make_base_doc("Tier3 W2 BSpline/Media/Clone", width_mm=120.0, height_mm=120.0, bleed_mm=0.0, color_mode="Rgb")
    ctrl_png = work / "ctrl.png"
    ctrl_in = work / "ctrl.coree"
    _write_coree(ctrl_in, "Empty Control", doc)
    err = _render_coree(ctx, ctrl_in, ctrl_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"W2 ADD-02: control render failed: {err}")

    # 1. NodePayload::BSpline (FEAT-18)
    bs_doc = json.loads(json.dumps(doc))
    bs_doc["objects"]["10"] = _make_obj(
        10,
        "BSpline",
        {
            "BSpline": {
                "control_points": [{"x": 15.0, "y": 80.0}, {"x": 40.0, "y": 20.0}, {"x": 75.0, "y": 20.0}, {"x": 100.0, "y": 80.0}],
                "degree": 3,
                "knots": [0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0],
                "closed": False,
            }
        },
        outline=_default_outline({"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}, 2.0, False),
    )
    bs_doc["pages"][0]["layers"][0]["objects"] = [10]
    bs_in = work / "bspline.coree"
    bs_png = work / "bspline.png"
    _write_coree(bs_in, "BSpline", bs_doc)
    err = _render_coree(ctx, bs_in, bs_png, work, dpi=72)
    if err or parse_png_info(bs_png.read_bytes())["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"W2 ADD-02 (FEAT-18): NodePayload::BSpline not supported or blank render: {err}")

    # 2. NodePayload::ArtisticMedia (FEAT-20)
    am_doc = json.loads(json.dumps(doc))
    am_doc["objects"]["11"] = _make_obj(
        11,
        "ArtisticMedia",
        {
            "ArtisticMedia": {
                "spine": {"subpaths": [{"nodes": [{"pos": {"x": 20.0, "y": 30.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"}, {"pos": {"x": 100.0, "y": 90.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"}], "closed": False}]},
                "style": {"Calligraphic": {"width_mm": 6.0, "angle_deg": 45.0}},
            }
        },
        fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.3, "b": 0.85, "a": 1.0}}},
    )
    am_doc["pages"][0]["layers"][0]["objects"] = [11]
    am_in = work / "artistic_media.coree"
    am_png = work / "artistic_media.png"
    _write_coree(am_in, "ArtisticMedia", am_doc)
    err = _render_coree(ctx, am_in, am_png, work, dpi=72)
    if err or parse_png_info(am_png.read_bytes())["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"W2 ADD-02 (FEAT-20): NodePayload::ArtisticMedia not supported or blank render: {err}")

    # 3. NodePayload::Clone + LiveSketch (FEAT-19) + Symmetry (FEAT-26)
    cl_doc = json.loads(json.dumps(doc))
    cl_doc["objects"]["12"] = _make_obj(12, "Master", _rect_payload(20.0, 20.0), tx=15.0, ty=15.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.7, "b": 0.2, "a": 1.0}}})
    cl_doc["objects"]["13"] = _make_obj(13, "Clone", {"Clone": {"master_id": 12, "link_fill": True, "link_outline": True, "link_shape": True}}, tx=65.0, ty=15.0)
    cl_doc["pages"][0]["layers"][0]["objects"] = [12, 13]
    cl_in = work / "clone.coree"
    cl_png = work / "clone.png"
    _write_coree(cl_in, "Clone", cl_doc)
    err = _render_coree(ctx, cl_in, cl_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"W2 ADD-02: NodePayload::Clone render failed: {err}")
    try:
        cl_bytes = cl_png.read_bytes()
        cl_px = _sample_rgba_box_avg(cl_bytes, 120.0, 120.0, 75.0, 25.0, radius_px=1)
        m_px = _sample_rgba_box_avg(cl_bytes, 120.0, 120.0, 25.0, 25.0, radius_px=1)
        gap_px = _sample_rgba_box_avg(cl_bytes, 120.0, 120.0, 50.0, 25.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-02: Clone PNG decode failed: {exc}")
    if not (cl_px[1] > cl_px[0] + 80 and cl_px[1] > cl_px[2] + 60 and cl_px[1] >= 120):
        return TestOutcome("FAIL", f"W2 ADD-02: Clone at (75,25)mm expected green, got {cl_px}")
    if not (m_px[1] > m_px[0] + 80 and m_px[1] >= 120) or min(gap_px[:3]) < 245:
        return TestOutcome("FAIL", f"W2 ADD-02: Clone master/gap invariant failed: master={m_px}, gap={gap_px}")

    # Multi-axis Symmetry visual check (FEAT-26) vs single-rect non-symmetry control
    sym_ctrl_doc = json.loads(json.dumps(doc))
    sym_ctrl_doc["objects"]["14"] = _make_obj(14, "Single Rect", _rect_payload(12.0, 12.0), tx=75.0, ty=55.0, fill={"Solid": {"Rgba": {"r": 0.85, "g": 0.2, "b": 0.5, "a": 1.0}}}, effects=[])
    sym_ctrl_doc["pages"][0]["layers"][0]["objects"] = [14]
    sym_ctrl_in = work / "sym_ctrl.coree"
    sym_ctrl_png = work / "sym_ctrl.png"
    _write_coree(sym_ctrl_in, "Symmetry Ctrl", sym_ctrl_doc)
    if (e := _render_coree(ctx, sym_ctrl_in, sym_ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry control render failed: {e}")

    sym_doc = json.loads(json.dumps(doc))
    sym_doc["objects"]["14"] = _make_obj(14, "Sym Petals", _rect_payload(12.0, 12.0), tx=75.0, ty=55.0, fill={"Solid": {"Rgba": {"r": 0.85, "g": 0.2, "b": 0.5, "a": 1.0}}}, effects=[{"Symmetry": {"axes": 6, "mirror": True, "origin": {"x": 60.0, "y": 60.0}}}])
    sym_doc["pages"][0]["layers"][0]["objects"] = [14]
    sym_in = work / "symmetry.coree"
    sym_png = work / "symmetry.png"
    _write_coree(sym_in, "Symmetry", sym_doc)
    err = _render_coree(ctx, sym_in, sym_png, work, dpi=72)
    if err:
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry render failed: {err}")
    try:
        single_nw = parse_png_info(sym_ctrl_png.read_bytes())["non_white_pixels"]
        sym_nw = parse_png_info(sym_png.read_bytes())["non_white_pixels"]
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry PNG decode failed: {exc}")
    sym_cmp = compare_png_pixels(sym_ctrl_png, sym_png)
    if sym_cmp["mean_abs_diff"] <= 1.0 or sym_nw < 4 * single_nw:
        return TestOutcome("FAIL", f"W2 FEAT-26: Symmetry did not multiply geometry (single={single_nw}, sym={sym_nw}, diff={sym_cmp['mean_abs_diff']:.2f})")

    tools_src = "".join(
        strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace"))
        for p in sorted((ctx.repo_root / "crates" / "coree-tools" / "src").rglob("*.rs"))
    )
    if not re.search(r"\bLiveSketch\b", tools_src) or not re.search(r"\bSmartDrawing\b", tools_src):
        return TestOutcome("FAIL", "W2 FEAT-19: LiveSketch / SmartDrawing tools not yet implemented in crates/coree-tools")

    return TestOutcome("PASS", "W2 BSpline, ArtisticMedia, Clone, LiveSketch/SmartDrawing & Symmetry verified")


def _test_t3_w2_curves_dcel_smartfill_boolean(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W2-DCEL-BOOL")
    # 1. Live compound hollow-ring EvenOdd cutout + smooth cubic OutlineStyle + laser --kerf checks
    ring_doc = _make_base_doc("Tier3 Compound Ring", width_mm=60.0, height_mm=60.0, bleed_mm=0.0, color_mode="Rgb")
    ring_doc["objects"]["10"] = _make_obj(
        10,
        "Hollow Ring",
        {
            "Path": {
                "subpaths": [
                    {
                        "nodes": [
                            {"pos": {"x": 10.0, "y": 10.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 50.0, "y": 10.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 50.0, "y": 50.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 10.0, "y": 50.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                        ],
                        "closed": True,
                    },
                    {
                        "nodes": [
                            {"pos": {"x": 20.0, "y": 20.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 20.0, "y": 40.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 40.0, "y": 40.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            {"pos": {"x": 40.0, "y": 20.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                        ],
                        "closed": True,
                    },
                ]
            }
        },
        fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.3, "b": 0.85, "a": 1.0}}},
    )
    ring_doc["pages"][0]["layers"][0]["objects"] = [10]
    ring_in = work / "ring.coree"
    ring_png = work / "ring.png"
    _write_coree(ring_in, "Compound Ring", ring_doc)
    if (e := _render_coree(ctx, ring_in, ring_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 FEAT-24: compound hollow-ring render failed: {e}")
    try:
        ring_bytes = ring_png.read_bytes()
        center_px = _sample_rgba_box_avg(ring_bytes, 60.0, 60.0, 30.0, 30.0, radius_px=1)
        band_px = _sample_rgba_box_avg(ring_bytes, 60.0, 60.0, 15.0, 30.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 FEAT-24: compound hollow-ring PNG decode failed: {exc}")
    if min(center_px[:3]) < 245 or min(band_px[:3]) >= 200:
        return TestOutcome("FAIL", f"W2 FEAT-24: compound cutout check failed (center={center_px}, ring={band_px})")

    # Smooth cubic curve with dashes, arrowheads & calligraphy vs solid control
    curve_doc = _make_base_doc("Tier3 Smooth Curve", width_mm=80.0, height_mm=80.0, bleed_mm=0.0, color_mode="Rgb")
    curve_payload = {
        "Path": {
            "subpaths": [
                {
                    "nodes": [
                        {"pos": {"x": 12.0, "y": 40.0}, "handle_in": None, "handle_out": {"x": 28.0, "y": 15.0}, "kind": "Smooth"},
                        {"pos": {"x": 68.0, "y": 40.0}, "handle_in": {"x": 52.0, "y": 65.0}, "handle_out": None, "kind": "Smooth"},
                    ],
                    "closed": False,
                }
            ]
        }
    }
    solid_ol = _default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 2.5, False)
    styled_ol = {**solid_ol, "dash_pattern": [6.0, 4.0], "start_arrow": "Circle", "end_arrow": "Triangle", "calligraphy_angle_deg": 45.0, "calligraphy_stretch": 0.25}
    curve_doc["objects"]["10"] = _make_obj(10, "Smooth Curve", curve_payload, fill="None", outline=solid_ol)
    curve_doc["pages"][0]["layers"][0]["objects"] = [10]
    c_solid_in = work / "c_solid.coree"
    c_solid_png = work / "c_solid.png"
    _write_coree(c_solid_in, "Curve Solid", curve_doc)
    if (e := _render_coree(ctx, c_solid_in, c_solid_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 FEAT-15: smooth curve control render failed: {e}")
    curve_doc["objects"]["10"]["outline"] = styled_ol
    c_styled_in = work / "c_styled.coree"
    c_styled_png = work / "c_styled.png"
    _write_coree(c_styled_in, "Curve Styled", curve_doc)
    if (e := _render_coree(ctx, c_styled_in, c_styled_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 FEAT-15: smooth styled curve render failed: {e}")
    if compare_png_pixels(c_solid_png, c_styled_png)["mean_abs_diff"] <= 0.25:
        return TestOutcome("FAIL", "W2 REM-01: smooth curve dash/arrow/calligraphy outline produced no visual diff")

    # Laser --kerf closed SVG check
    star_doc = _make_base_doc("Tier3 Kerf Star", width_mm=80.0, height_mm=80.0, bleed_mm=0.0, color_mode="Rgb")
    star_doc["objects"]["10"] = _make_obj(10, "Star", {"Shape": {"Star": {"points": 5, "outer_radius": 24.0, "inner_ratio": 0.45, "complex": False}}}, tx=40.0, ty=40.0, fill="None", outline=_default_outline({"Rgba": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 0.2, True))
    star_doc["pages"][0]["layers"][0]["objects"] = [10]
    star_in = work / "kerf_star.coree"
    kerf_svg = work / "kerf_out.svg"
    _write_coree(star_in, "Kerf Star", star_doc)
    r_laser = run_cmd([str(ctx.cli_bin), "laser", str(star_in), str(kerf_svg), "--kerf", "0.2", "--format", "svg"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_laser.returncode != 0 or not kerf_svg.is_file():
        return TestOutcome("FAIL", f"W2/W4 REM-09: laser --kerf failed: {r_laser.stderr}")
    try:
        k_root = ET.fromstring(kerf_svg.read_text(encoding="utf-8"))
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2/W4 REM-09: laser --kerf SVG malformed: {exc}")
    k_paths = [el.attrib.get("d", "") for el in k_root.iter() if el.tag.split("}", 1)[-1] == "path" and el.attrib.get("d", "").strip()]
    if not k_paths or not any(("Z" in d or "z" in d) for d in k_paths):
        return TestOutcome("FAIL", f"W2/W4 REM-09: laser --kerf SVG missing closed <path>: {k_paths}")

    # 2. Structural kernel checks: DCEL SmartFill, analytical erase_circle, enforce_continuity, BooleanOp metadata + stroke_to_closed_path
    sf_code = _read_rust_stripped(ctx, "crates/coree-render/src/smart_fill.rs")
    geom_code = _read_rust_stripped(ctx, "crates/coree-core/src/geometry.rs")
    bool_code = _read_rust_stripped(ctx, "crates/coree-core/src/boolean.rs")
    cmd_code = _read_rust_stripped(ctx, "crates/coree-core/src/command.rs")
    if not re.search(r"\b(?:Dcel|DCEL|HalfEdge)\b", sf_code):
        return TestOutcome("FAIL", "W2 IMP-05 (FEAT-25): smart_fill.rs uses raster flood-fill instead of planar DCEL HalfEdge topology")
    erase_fn = _extract_brace_block(geom_code, r"\bfn\s+erase_circle\b") or ""
    if not erase_fn or "96" in erase_fn:
        return TestOutcome("FAIL", "W2 REM-01 (FEAT-17): analytical erase_circle (zero 96-seg polygon) incomplete in geometry.rs")
    if not re.search(r"\benforce_continuity\b", geom_code):
        return TestOutcome("FAIL", "W2 REM-10 (FEAT-15): PathNode::enforce_continuity missing in geometry.rs")
    if not re.search(r"\bBooleanOp\b", bool_code) or not re.search(r"\bstyle_id\b", cmd_code):
        return TestOutcome("FAIL", "W2 REM-03 (FEAT-24): BooleanOp metadata preservation missing in boolean.rs / command.rs")
    if not all(k in bool_code for k in ("dash_pattern", "start_arrow", "calligraphy_angle_deg")):
        return TestOutcome("FAIL", "W2 REM-01: stroke_to_closed_path in boolean.rs ignores dashes/arrows/calligraphy")
    return TestOutcome("PASS", "Planar DCEL SmartFill, analytical erase_circle, enforce_continuity & BooleanOp metadata verified")


def _test_t3_w2_perspective_powerclip_dim_cdr(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W2-PERSP-CLIP-CDR")

    # 1. PowerClip visual clipping containment + .cdr round-trip + PowerClipFitMode (FEAT-42)
    doc_rs = _read_rust_stripped(ctx, "crates/coree-core/src/document.rs")
    cmd_rs = _read_rust_stripped(ctx, "crates/coree-core/src/command.rs")
    fit_fn = _extract_brace_block(doc_rs, r"\bpub\s+fn\s+fit_powerclip\b") or ""
    if not fit_fn or _is_trivial_stub_body(fit_fn) or not all(m in doc_rs for m in ("FitContents", "CenterContents", "StretchToFill")) or "PowerClipFitMode" not in cmd_rs:
        return TestOutcome("FAIL", "W2 ADD-03 (FEAT-42): PowerClipFitMode / fit_powerclip incomplete in document.rs / command.rs")

    pc_doc = _make_base_doc("Tier3 PowerClip", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    pc_doc["objects"]["20"] = _make_obj(
        20,
        "Oversized Red Child",
        _rect_payload(80.0, 80.0),
        tx=10.0,
        ty=10.0,
        fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}},
    )
    pc_doc["objects"]["21"] = _make_obj(
        21,
        "PowerClip Frame",
        {
            "PowerClip": {
                "container": {
                    "subpaths": [
                        {
                            "nodes": [
                                {"pos": {"x": 25.0, "y": 25.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 75.0, "y": 25.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 75.0, "y": 75.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 25.0, "y": 75.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            ],
                            "closed": True,
                        }
                    ]
                },
                "container_parametric": None,
                "lock_contents": True,
                "contents": [20],
            }
        },
        fill="None",
    )
    pc_doc["pages"][0]["layers"][0]["objects"] = [21]
    pc_in = work / "pc.coree"
    pc_png = work / "pc.png"
    pc_cdr = work / "pc_x6.cdr"
    pc_rt = work / "pc_rt.coree"
    _write_coree(pc_in, "PowerClip", pc_doc)
    if (e := _render_coree(ctx, pc_in, pc_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 ADD-03 (FEAT-42): PowerClip render failed: {e}")
    try:
        pc_bytes = pc_png.read_bytes()
        pc_inside = _sample_rgba_box_avg(pc_bytes, 100.0, 100.0, 50.0, 50.0, radius_px=1)
        pc_outside = _sample_rgba_box_avg(pc_bytes, 100.0, 100.0, 15.0, 15.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-03 (FEAT-42): PowerClip PNG decode failed: {exc}")
    if pc_inside[0] < 200 or pc_inside[1] > 60 or pc_inside[2] > 60:
        return TestOutcome("FAIL", f"W2 ADD-03 (FEAT-42): PowerClip interior (50,50)mm expected red, got {pc_inside}")
    if pc_outside[0] < 245 or pc_outside[1] < 245 or pc_outside[2] < 245:
        return TestOutcome("FAIL", f"W2 ADD-03 (FEAT-42): PowerClip failed to clip oversized child at (15,15)mm: {pc_outside}")

    r_pc_cdr = run_cmd([str(ctx.cli_bin), "convert", str(pc_in), str(pc_cdr), "--cdr-version", "x6"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    r_pc_rt = run_cmd([str(ctx.cli_bin), "convert", str(pc_cdr), str(pc_rt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale) if r_pc_cdr.returncode == 0 else None
    if r_pc_cdr.returncode != 0 or r_pc_rt is None or r_pc_rt.returncode != 0 or not pc_rt.is_file():
        return TestOutcome("FAIL", f"W2 ADD-03 (FEAT-42): PowerClip .cdr round-trip failed: {r_pc_cdr.stderr}")
    _, pc_rt_doc = inspect_coree_zip(pc_rt)
    if not any(isinstance(o.get("payload"), dict) and "PowerClip" in o["payload"] for o in pc_rt_doc.get("objects", {}).values()):
        return TestOutcome("FAIL", "W2 ADD-03 (FEAT-42): PowerClip flattened or dropped during .cdr round-trip")

    # 2. Effect::Perspective live 3x3 projective homography midpoint check (FEAT-27)
    def _bar_subpath(y0: float, y1: float) -> dict[str, Any]:
        return {
            "nodes": [
                {"pos": {"x": 0.0, "y": y0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                {"pos": {"x": 60.0, "y": y0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                {"pos": {"x": 60.0, "y": y1}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                {"pos": {"x": 0.0, "y": y1}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
            ],
            "closed": True,
        }

    persp_doc = _make_base_doc("Tier3 Perspective 3x3", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    persp_doc["objects"]["10"] = _make_obj(
        10,
        "3-Bar Perspective",
        {"Path": {"subpaths": [_bar_subpath(0.0, 4.0), _bar_subpath(27.0, 33.0), _bar_subpath(56.0, 60.0)]}},
        tx=20.0,
        ty=20.0,
        fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}},
        effects=[{"Perspective": {"quad": [{"x": 20.0, "y": 0.0}, {"x": 40.0, "y": 0.0}, {"x": 60.0, "y": 60.0}, {"x": 0.0, "y": 60.0}]}}],
    )
    persp_doc["pages"][0]["layers"][0]["objects"] = [10]
    persp_in = work / "persp.coree"
    persp_png = work / "persp.png"
    _write_coree(persp_in, "Perspective", persp_doc)
    if (e := _render_coree(ctx, persp_in, persp_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W2 ADD-04 (FEAT-27): Perspective render failed: {e}")
    try:
        p_bytes = persp_png.read_bytes()
        proj_mid = _sample_rgba_box_avg(p_bytes, 100.0, 100.0, 50.0, 35.0, radius_px=1)
        bilin_mid = _sample_rgba_box_avg(p_bytes, 100.0, 100.0, 50.0, 50.0, radius_px=1)
        ext_pt = _sample_rgba_box_avg(p_bytes, 100.0, 100.0, 25.0, 25.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W2 ADD-04 (FEAT-27): Perspective PNG decode failed: {exc}")
    if proj_mid[0] < 200 or proj_mid[1] > 60 or proj_mid[2] > 60:
        return TestOutcome("FAIL", f"W2 ADD-04 (FEAT-27): projective midpoint (50,35)mm expected red, got {proj_mid}")
    if min(bilin_mid[:3]) < 245 or min(ext_pt[:3]) < 245:
        return TestOutcome("FAIL", f"W2 ADD-04 (FEAT-27): bilinear midpoint/exterior leaked color (bilin={bilin_mid}, ext={ext_pt})")

    math_code = _read_rust_stripped(ctx, "crates/coree-core/src/math.rs")
    if not re.search(r"\bPerspective3x3\b|\bhomography\b", math_code, re.I):
        return TestOutcome("FAIL", "W2 ADD-04 (FEAT-27): 3x3 homography perspective transform missing in coree-core/src/math.rs")

    # 3. Dimension + .cdr round-trip visual parity (FEAT-28, FEAT-81)
    src = _t3_fixture(ctx, "visual_oracle_card.coree")
    _, doc = inspect_coree_zip(src)
    doc["objects"]["30"] = _make_obj(
        30,
        "Dim Horiz",
        {
            "Dimension": {
                "kind": "Horizontal",
                "p1": {"x": 15.0, "y": 16.0},
                "p2": {"x": 71.0, "y": 16.0},
                "p3": None,
                "offset_mm": 8.0,
                "precision": 1,
                "prefix": "",
                "suffix": " mm",
                "label_override": None,
                "text_style": _default_text_style("Noto Sans", 10.0),
                "unit_override": None,
            }
        },
        outline=_default_outline({"Cmyk": {"c": 0.0, "m": 0.0, "y": 0.0, "k": 1.0, "a": 1.0}}, 0.3, False),
    )
    doc["pages"][0]["layers"][0]["objects"].append(30)
    in_coree = work / "dim_card.coree"
    out_cdr = work / "dim_card_x6.cdr"
    rt_coree = work / "dim_card_rt.coree"
    _write_coree(in_coree, "Dim CDR Roundtrip", doc)

    r_cdr = run_cmd([str(ctx.cli_bin), "convert", str(in_coree), str(out_cdr), "--cdr-version", "x6"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cdr.returncode != 0 or not out_cdr.is_file():
        return TestOutcome("FAIL", f"W2 MND-03 (FEAT-81): convert -> x6 .cdr failed: {r_cdr.stderr}")
    zinfo = dissect_cdr_zcf(out_cdr.read_bytes())
    if (
        not zinfo.get("has_cdro_root")
        or not zinfo.get("has_valid_indirect_pointers")
        or not zinfo.get("has_20b_deref_loda")
        or zinfo.get("unindirected_loda_count", 1) != 0
        or zinfo.get("deref_12b_loda_count", 1) != 0
        or zinfo.get("cdss_key_count", 0) < 1
    ):
        return TestOutcome("FAIL", f"W2 MND-03 (FEAT-81): ZCF .cdr lacks 16B indirect pointers / 20B loda / document.cdss: {zinfo}")

    r_rt = run_cmd([str(ctx.cli_bin), "convert", str(out_cdr), str(rt_coree)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_rt.returncode != 0 or not rt_coree.is_file():
        return TestOutcome("FAIL", f"W2 MND-03: .cdr -> .coree re-import failed: {r_rt.stderr}")
    _, rt_doc = inspect_coree_zip(rt_coree)
    has_dim = any(isinstance(o.get("payload"), dict) and "Dimension" in o["payload"] for o in rt_doc.get("objects", {}).values())
    if not has_dim:
        return TestOutcome("FAIL", "W2 MND-03 (FEAT-28): Dimension flattened during .cdr round-trip")

    orig_png = work / "orig.png"
    rt_png = work / "rt.png"
    if _render_coree(ctx, in_coree, orig_png, work, dpi=72) or _render_coree(ctx, rt_coree, rt_png, work, dpi=72):
        return TestOutcome("FAIL", "W2 MND-03: .cdr visual round-trip render failed")
    cmp = compare_png_pixels(orig_png, rt_png)
    if cmp["mean_abs_diff"] >= 6.0 or cmp["non_white_iou"] < 0.85:
        return TestOutcome("FAIL", f"W2 MND-03 (FEAT-81): .cdr round-trip visual divergence too high: {cmp}")

    return TestOutcome("PASS", f"W2 Perspective3x3, PowerClip, non-flattened Dimension & authentic ZCF .cdr visual round-trip verified")


def _test_t3_w3_icc_bpc_chad_ciede2000(ctx: RunContext) -> TestOutcome:
    """Flagship Tier 3 colorimetric oracle: coree-cli render vs pure-Python ICC v2/v4 + CIEDE2000 (Delta-E < 1.0)."""
    if ctx.icc_dir is None:
        return TestOutcome("FAIL", "W3 REQ-02: /usr/share/color/icc/ directory not found")

    work = ctx.make_scratch_dir("T3-W3-ICC-CIEDE2000")
    src = _t3_fixture(ctx, "icc_swatches.coree")
    _, doc = inspect_coree_zip(src)

    fogra_path = ctx.icc_dir / "colord" / "FOGRA39L_coated.icc"
    swop_path = ctx.icc_dir / "colord" / "SWOP_TR003_coated_3.icc"
    if not fogra_path.is_file() or not swop_path.is_file():
        return TestOutcome("FAIL", "W3 REQ-02: FOGRA39L_coated.icc or SWOP_TR003_coated_3.icc missing")

    fogra_bytes = fogra_path.read_bytes()
    swop_bytes = swop_path.read_bytes()
    fogra_oracle = IccProfileOracle(fogra_bytes)

    # Render without custom ICC vs FOGRA39L vs SWOP
    naive_png = work / "naive.png"
    fogra_png = work / "fogra.png"
    swop_png = work / "swop.png"
    if (e := _render_coree(ctx, src, naive_png, work, dpi=96)):
        return TestOutcome("FAIL", f"W3 REQ-02: baseline swatch render failed: {e}")

    fogra_doc = json.loads(json.dumps(doc))
    fogra_doc["custom_icc_profile"] = base64.b64encode(fogra_bytes).decode("ascii")
    fogra_doc["custom_icc_name"] = fogra_path.name
    fogra_in = work / "fogra.coree"
    _write_coree(fogra_in, "FOGRA39 Swatches", fogra_doc)
    if (e := _render_coree(ctx, fogra_in, fogra_png, work, dpi=96)):
        return TestOutcome("FAIL", f"W3 REQ-02: FOGRA39 render failed: {e}")

    swop_doc = json.loads(json.dumps(doc))
    swop_doc["custom_icc_profile"] = base64.b64encode(swop_bytes).decode("ascii")
    swop_doc["custom_icc_name"] = swop_path.name
    swop_in = work / "swop.coree"
    _write_coree(swop_in, "SWOP Swatches", swop_doc)
    if (e := _render_coree(ctx, swop_in, swop_png, work, dpi=96)):
        return TestOutcome("FAIL", f"W3 REQ-02: SWOP render failed: {e}")

    # 1. Verify FOGRA39 vs SWOP produces measurable colorimetric diff
    fs_cmp = compare_png_pixels(fogra_png, swop_png)
    if fs_cmp["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W3 REQ-02 (FEAT-36): FOGRA39L vs SWOP produced no render difference (mean_abs_diff={fs_cmp['mean_abs_diff']:.4f})")

    # 2. Sample rendered CMYK swatches in both FOGRA39 and SWOP PNGs and enforce CIEDE2000 < 1.0 vs pure-Python ICC oracle
    fogra_png_bytes = fogra_png.read_bytes()
    swop_png_bytes = swop_png.read_bytes()
    swop_oracle = IccProfileOracle(swop_bytes)
    swatch_probes = [
        ("InGamutCyan", (0.65, 0.15, 0.0, 0.0), 24.0, 22.0),
        ("InGamutMagenta", (0.0, 0.85, 0.05, 0.0), 59.0, 22.0),
        ("WarmOrange", (0.0, 0.60, 0.80, 0.0), 94.0, 22.0),
        ("DeepWarm", (0.4, 0.7, 0.2, 0.1), 24.0, 57.0),
    ]
    max_de = 0.0
    swatch_des: dict[str, float] = {}
    for prof_tag, png_bytes, oracle in (("FOGRA39", fogra_png_bytes, fogra_oracle), ("SWOP", swop_png_bytes, swop_oracle)):
        for label, cmyk, cx_mm, cy_mm in swatch_probes:
            r, g, b, _ = _sample_rgba_box_avg(png_bytes, 120.0, 80.0, cx_mm, cy_mm)
            ren_lab = srgb_to_lab_d50((r / 255.0, g / 255.0, b / 255.0))
            ref_lab = srgb_to_lab_d50(oracle.cmyk_to_srgb(cmyk, intent=1, bpc=True))
            de = ciede2000(ren_lab, ref_lab)
            swatch_des[f"{prof_tag}:{label}"] = round(de, 3)
            max_de = max(max_de, de)
    if max_de >= 1.0:
        return TestOutcome("FAIL", f"W3 REQ-02 (FEAT-36): FOGRA39/SWOP rendered swatches exceed CIEDE2000 < 1.0 (max_dE={max_de:.3f}, per_swatch={swatch_des})")

    # 3. Multi-stop Fountain Fill (FEAT-31) ICC colorimetric change check
    grad_doc = _make_base_doc("Grad ICC", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Cmyk")
    grad_doc["objects"]["10"] = _make_obj(
        10,
        "CMYK Grad",
        _rect_payload(80.0, 80.0),
        tx=10.0,
        ty=10.0,
        fill={"LinearGradient": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 80.0, "y": 80.0}, "stops": [{"offset": 0.0, "color": {"Cmyk": {"c": 1.0, "m": 0.0, "y": 0.0, "k": 0.0, "a": 1.0}}}, {"offset": 1.0, "color": {"Cmyk": {"c": 0.0, "m": 1.0, "y": 0.0, "k": 0.0, "a": 1.0}}}]}},
    )
    grad_doc["pages"][0]["layers"][0]["objects"] = [10]
    g_base_in = work / "g_base.coree"
    g_base_png = work / "g_base.png"
    _write_coree(g_base_in, "Grad Base", grad_doc)
    if (e := _render_coree(ctx, g_base_in, g_base_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 REQ-02 (FEAT-31): gradient baseline render failed: {e}")
    grad_doc["custom_icc_profile"] = fogra_doc["custom_icc_profile"]
    grad_doc["custom_icc_name"] = fogra_doc["custom_icc_name"]
    g_icc_in = work / "g_icc.coree"
    g_icc_png = work / "g_icc.png"
    _write_coree(g_icc_in, "Grad ICC", grad_doc)
    if (e := _render_coree(ctx, g_icc_in, g_icc_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 REQ-02 (FEAT-31): gradient ICC render failed: {e}")
    g_cmp = compare_png_pixels(g_base_png, g_icc_png)
    if g_cmp["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", f"W3 REQ-02 (FEAT-31): LinearGradient ignored custom ICC profile ({g_cmp})")

    color_code = _read_rust_stripped(ctx, "crates/coree-core/src/color.rs")
    icc_code = _read_rust_stripped(ctx, "crates/coree-core/src/icc.rs")
    if not re.search(r"\bchad\b", icc_code, re.I) or not re.search(r"\b(?:black_point|bpc)\b", icc_code, re.I):
        return TestOutcome("FAIL", "W3 REQ-02 (FEAT-36): icc.rs missing chromatic adaptation (chad) or Black Point Compensation (BPC)")
    if not re.search(r"\bcmyk_to_rgb_icc\b", color_code):
        return TestOutcome("FAIL", "W3 REQ-02 (FEAT-36): Color::cmyk_to_rgb not yet routed through ICC + RenderingIntent")

    return TestOutcome(
        "PASS",
        f"ICC + BPC + chad + FOGRA39/SWOP CIEDE2000 < 1.0 (max_dE={max_de:.3f}) verified",
        metrics={"max_ciede2000": round(max_de, 3), "swatch_des": swatch_des},
    )


def _test_t3_w3_wgpu_cpu_gpu_parity(ctx: RunContext) -> TestOutcome:
    backend_code = _read_rust_stripped(ctx, "crates/coree-render/src/backend.rs")
    tc_code = _read_rust_stripped(ctx, "crates/coree-render/src/tile_cache.rs")
    if not re.search(r"\bstruct\s+WgpuBackend\b", backend_code) or not re.search(r"impl\s+CanvasBackend\s+for\s+WgpuBackend\b", backend_code):
        return TestOutcome("FAIL", "W3 IMP-01 (FEAT-05): WgpuBackend behind CanvasBackend not yet implemented in backend.rs")
    if not re.search(r"\bstruct\s+WorldTileKey\b", tc_code):
        return TestOutcome("FAIL", "W3 IMP-02: WorldTileKey DirtyTileCache missing in tile_cache.rs")

    work = ctx.make_scratch_dir("T3-W3-CPU-GPU")
    r_help = run_cmd([str(ctx.cli_bin), "render", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if r_help.returncode != 0 or "--backend" not in r_help.stdout:
        return TestOutcome("FAIL", "W3 IMP-01 (FEAT-05): coree-cli render missing --backend flag for CPU/GPU parity")
    src = _t3_fixture(ctx, "pairwise_master.coree")
    cpu_png = work / "cpu.png"
    gpu_png = work / "gpu.png"
    e1 = _render_coree(ctx, src, cpu_png, work, dpi=96, extra_args=["--backend", "tinyskia"])
    e2 = _render_coree(ctx, src, gpu_png, work, dpi=96, extra_args=["--backend", "wgpu"])
    if e1 or e2:
        return TestOutcome("FAIL", f"W3 IMP-01: CPU/GPU backend render failed: {e1} {e2}")
    cmp = compare_png_pixels(cpu_png, gpu_png)
    if cmp["mean_abs_diff"] >= 2.0 or cmp["non_white_iou"] < 0.98:
        return TestOutcome("FAIL", f"W3 IMP-01: TinySkia vs WgpuBackend pixel parity diverged: {cmp}")

    return TestOutcome("PASS", "W3 WgpuBackend + WorldTileKey + CPU/GPU render parity verified")


def _test_t3_w3_coons_mesh_and_effects_parity(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W3-COONS-EFFECTS")

    # Live CLI visual checks for Coons MeshFill + node alpha, TransparencyModes + Multiply, InnerShadow, Bevel, Extrude3D, Contour, Blend, 4-curve Envelope
    base = _make_base_doc("Tier3 W3 Effects", width_mm=120.0, height_mm=120.0, bleed_mm=0.0, color_mode="Rgb")
    base["objects"]["10"] = _make_obj(10, "Target", _rect_payload(60.0, 60.0), tx=30.0, ty=30.0, fill={"Solid": {"Rgba": {"r": 0.85, "g": 0.15, "b": 0.15, "a": 1.0}}})
    base["pages"][0]["layers"][0]["objects"] = [10]
    ctrl_in = work / "ctrl.coree"
    ctrl_png = work / "ctrl.png"
    _write_coree(ctrl_in, "Ctrl", base)
    if (e := _render_coree(ctx, ctrl_in, ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 effects control failed: {e}")

    # 1. Coons MeshFill with tangent handles + alpha=0.0 vs uncurved alpha=1.0 control (FEAT-33)
    ctrl_mesh_doc = json.loads(json.dumps(base))
    ctrl_mesh_doc["objects"]["10"]["fill"] = {
        "MeshFill": {
            "rows": 2,
            "cols": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Rgba": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}, "handle_n": None, "handle_s": None, "handle_e": None, "handle_w": None, "alpha": 1.0},
                {"row": 0, "col": 1, "pos": {"x": 60.0, "y": 0.0}, "color": {"Rgba": {"r": 0.0, "g": 1.0, "b": 0.0, "a": 1.0}}, "handle_n": None, "handle_s": None, "handle_e": None, "handle_w": None, "alpha": 1.0},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 60.0}, "color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0}}, "handle_n": None, "handle_s": None, "handle_e": None, "handle_w": None, "alpha": 1.0},
                {"row": 1, "col": 1, "pos": {"x": 60.0, "y": 60.0}, "color": {"Rgba": {"r": 1.0, "g": 1.0, "b": 0.0, "a": 1.0}}, "handle_n": None, "handle_s": None, "handle_e": None, "handle_w": None, "alpha": 1.0},
            ],
        }
    }
    ctrl_mesh_in = work / "ctrl_mesh.coree"
    ctrl_mesh_png = work / "ctrl_mesh.png"
    _write_coree(ctrl_mesh_in, "Ctrl Mesh", ctrl_mesh_doc)
    if (e := _render_coree(ctx, ctrl_mesh_in, ctrl_mesh_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-33): control MeshFill render failed: {e}")

    mesh_doc = json.loads(json.dumps(base))
    mesh_doc["objects"]["10"]["fill"] = {
        "MeshFill": {
            "rows": 2,
            "cols": 2,
            "nodes": [
                {"row": 0, "col": 0, "pos": {"x": 0.0, "y": 0.0}, "color": {"Rgba": {"r": 1.0, "g": 0.0, "b": 0.0, "a": 1.0}}, "handle_e": {"x": 20.0, "y": 25.0}, "handle_s": {"x": 25.0, "y": 20.0}, "alpha": 0.0},
                {"row": 0, "col": 1, "pos": {"x": 60.0, "y": 0.0}, "color": {"Rgba": {"r": 0.0, "g": 1.0, "b": 0.0, "a": 1.0}}, "handle_w": {"x": -20.0, "y": 25.0}, "handle_s": {"x": -25.0, "y": 20.0}, "alpha": 1.0},
                {"row": 1, "col": 0, "pos": {"x": 0.0, "y": 60.0}, "color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0}}, "handle_n": {"x": 25.0, "y": -20.0}, "handle_e": {"x": 20.0, "y": -25.0}, "alpha": 1.0},
                {"row": 1, "col": 1, "pos": {"x": 60.0, "y": 60.0}, "color": {"Rgba": {"r": 1.0, "g": 1.0, "b": 0.0, "a": 1.0}}, "handle_n": {"x": -25.0, "y": -20.0}, "handle_w": {"x": -20.0, "y": -25.0}, "alpha": 1.0},
            ],
        }
    }
    mesh_in = work / "coons.coree"
    mesh_png = work / "coons.png"
    _write_coree(mesh_in, "Coons Mesh", mesh_doc)
    if (e := _render_coree(ctx, mesh_in, mesh_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-33): Coons MeshFill render failed: {e}")
    mesh_cmp = compare_png_pixels(ctrl_mesh_png, mesh_png)
    if mesh_cmp["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-33): Coons handles/alpha produced no visual diff vs uncurved alpha=1.0 mesh ({mesh_cmp})")
    try:
        corner_px = _sample_rgba_box_avg(mesh_png.read_bytes(), 120.0, 120.0, 32.0, 32.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-33): Coons PNG decode failed: {exc}")
    if min(corner_px[:3]) < 200:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-33): MeshFill node (0,0) alpha=0.0 failed white bleed-through at (32,32)mm: {corner_px}")

    # 2. All 6 TransparencyModes + BlendMode::Multiply over blue background (FEAT-50)
    bg_doc = json.loads(json.dumps(base))
    bg_doc["objects"]["9"] = _make_obj(9, "Blue BG", _rect_payload(100.0, 100.0), tx=10.0, ty=10.0, fill={"Solid": {"Rgba": {"r": 0.0, "g": 0.25, "b": 0.9, "a": 1.0}}})
    bg_doc["pages"][0]["layers"][0]["objects"] = [9, 10]
    tr_ctrl_in = work / "tr_ctrl.coree"
    tr_ctrl_png = work / "tr_ctrl.png"
    _write_coree(tr_ctrl_in, "Tr Ctrl", bg_doc)
    if (e := _render_coree(ctx, tr_ctrl_in, tr_ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 FEAT-50: opaque control render failed: {e}")
    bg_only_doc = json.loads(json.dumps(bg_doc))
    bg_only_doc["pages"][0]["layers"][0]["objects"] = [9]
    bg_only_in = work / "bg_only.coree"
    bg_only_png = work / "bg_only.png"
    _write_coree(bg_only_in, "BG Only", bg_only_doc)
    if (e := _render_coree(ctx, bg_only_in, bg_only_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 FEAT-50: blue background control render failed: {e}")

    tr_cases = {
        "Pattern": ({"Pattern": {"pattern_name": "checker", "tile_w": 8.0, "tile_h": 8.0, "fg_opacity": 1.0, "bg_opacity": 0.1}}, "Normal"),
        "Texture": ({"Texture": {"preset": "marble", "scale": 6.0, "fg_opacity": 1.0, "bg_opacity": 0.1}}, "Normal"),
        "Conical": ({"Conical": {"center": {"x": 30.0, "y": 30.0}, "angle_deg": 0.0, "start_opacity": 1.0, "end_opacity": 0.1}}, "Normal"),
        "Square": ({"Square": {"center": {"x": 30.0, "y": 30.0}, "size": 50.0, "angle_deg": 0.0, "start_opacity": 1.0, "end_opacity": 0.1}}, "Normal"),
        "Uniform": ({"Uniform": 0.5}, "Normal"),
        "FountainLinear": ({"FountainLinear": {"start": {"x": 0.0, "y": 0.0}, "end": {"x": 60.0, "y": 60.0}, "start_alpha": 1.0, "end_alpha": 0.1}}, "Normal"),
        "Multiply": ({"Uniform": 0.8}, "Multiply"),
    }
    tr_pngs: dict[str, Path] = {}
    for tm_name, (tm_val, bmode) in tr_cases.items():
        td = json.loads(json.dumps(bg_doc))
        td["objects"]["10"]["effects"] = [{"Transparency": {"mode": tm_val, "blend_mode": bmode}}]
        tin = work / f"tr_{tm_name}.coree"
        tpng = work / f"tr_{tm_name}.png"
        _write_coree(tin, f"Tr {tm_name}", td)
        if (e := _render_coree(ctx, tin, tpng, work, dpi=72)):
            return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): TransparencyMode::{tm_name} render failed: {e}")
        if compare_png_pixels(tr_ctrl_png, tpng)["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): TransparencyMode::{tm_name} matched opaque control")
        if compare_png_pixels(bg_only_png, tpng)["mean_abs_diff"] <= 1.0:
            return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): TransparencyMode::{tm_name} matched empty blue background")
        if bmode == "Normal":
            try:
                pw, ph, rgba = _decode_png_rgba_bytes_stdlib(tpng.read_bytes())
            except _IO_PARSE_EXCEPTIONS as exc:
                return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): {tm_name} PNG decode failed: {exc}")
            max_blue = 0
            for py in range(int(32.0 / 120.0 * ph), int(88.0 / 120.0 * ph)):
                for px in range(int(32.0 / 120.0 * pw), int(88.0 / 120.0 * pw)):
                    max_blue = max(max_blue, rgba[(py * pw + px) * 4 + 2])
            if max_blue < 100:
                return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): {tm_name} showed no blue bleed-through (max_blue={max_blue})")
        tr_pngs[tm_name] = tpng

    tr_names = list(tr_pngs.keys())
    for i in range(len(tr_names)):
        for j in range(i + 1, len(tr_names)):
            pair_cmp = compare_png_pixels(tr_pngs[tr_names[i]], tr_pngs[tr_names[j]])
            if pair_cmp["mean_abs_diff"] <= 0.3:
                return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): {tr_names[i]} vs {tr_names[j]} indistinguishable ({pair_cmp})")
    try:
        mul_px = _sample_rgba_box_avg(tr_pngs["Multiply"].read_bytes(), 120.0, 120.0, 60.0, 60.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): Multiply PNG decode failed: {exc}")
    if mul_px[0] >= 120:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-50): BlendMode::Multiply failed to darken red over blue at (60,60)mm: {mul_px}")

    # 3. Live .coree renders for InnerShadow (FEAT-43), Bevel, Extrude3D (FEAT-49), Contour (FEAT-45), Blend (FEAT-46), 12-pt Envelope (FEAT-48)
    try:
        ctrl_nw = parse_png_info(ctrl_png.read_bytes())["non_white_pixels"]
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 effects control PNG decode failed: {exc}")

    # InnerShadow
    is_doc = json.loads(json.dumps(base))
    is_doc["objects"]["10"]["effects"] = [{"InnerShadow": {"dx_mm": 3.0, "dy_mm": 3.0, "blur_mm": 2.5, "opacity": 0.75, "color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, "feather_direction": "Inside"}}]
    is_in, is_png = work / "inner_shadow.coree", work / "inner_shadow.png"
    _write_coree(is_in, "InnerShadow", is_doc)
    if (e := _render_coree(ctx, is_in, is_png, work, dpi=72)) or compare_png_pixels(ctrl_png, is_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-43): InnerShadow render failed or no-op: {e}")
    c_in_px = _sample_rgba_box_avg(ctrl_png.read_bytes(), 120.0, 120.0, 32.0, 32.0, radius_px=1)
    is_in_px = _sample_rgba_box_avg(is_png.read_bytes(), 120.0, 120.0, 32.0, 32.0, radius_px=1)
    is_ext_px = _sample_rgba_box_avg(is_png.read_bytes(), 120.0, 120.0, 25.0, 25.0, radius_px=1)
    if sum(is_in_px[:3]) >= sum(c_in_px[:3]) - 20 or min(is_ext_px[:3]) < 245:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-43): InnerShadow interior/exterior invariant failed (in={is_in_px}, ctrl={c_in_px}, ext={is_ext_px})")

    # Bevel
    bev_doc = json.loads(json.dumps(base))
    bev_doc["objects"]["10"]["effects"] = [{"Bevel": {"style": "SoftEdge", "distance_mm": 4.0, "softness_mm": 1.5, "light_angle_deg": 45.0, "altitude_deg": 35.0, "highlight_color": {"Rgba": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}}, "shadow_color": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.2, "a": 1.0}}, "intensity": 0.85}}]
    bev_in, bev_png = work / "bevel.coree", work / "bevel.png"
    _write_coree(bev_in, "Bevel", bev_doc)
    if (e := _render_coree(ctx, bev_in, bev_png, work, dpi=72)) or compare_png_pixels(ctrl_png, bev_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 ADD-01: Bevel render failed or no-op: {e}")

    # Extrude3D
    ext_doc = json.loads(json.dumps(base))
    ext_doc["objects"]["10"]["effects"] = [{"Extrude3D": {"vanishing_point": {"x": 105.0, "y": 15.0}, "depth": 18.0, "bevel_depth": 1.5, "shade_color": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.2, "a": 1.0}}, "rotation_3d": [15.0, -20.0, 5.0], "lights": [{"dir": [0.5, -0.5, 0.7], "intensity": 0.8}, {"dir": [-0.6, 0.4, 0.7], "intensity": 0.5}]}}]
    ext_in, ext_png = work / "extrude3d.coree", work / "extrude3d.png"
    _write_coree(ext_in, "Extrude3D", ext_doc)
    if (e := _render_coree(ctx, ext_in, ext_png, work, dpi=72)) or compare_png_pixels(ctrl_png, ext_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-49): Extrude3D render failed or no-op: {e}")
    if parse_png_info(ext_png.read_bytes())["non_white_pixels"] <= ctrl_nw + 50:
        return TestOutcome("FAIL", "W3 ADD-01 (FEAT-49): Extrude3D did not add 3D extruded geometry")

    # Contour
    cnt_doc = json.loads(json.dumps(base))
    cnt_doc["objects"]["10"]["effects"] = [{"Contour": {"mode": "Outside", "steps": 3, "offset_mm": 2.5, "fill_color": {"Rgba": {"r": 0.1, "g": 0.75, "b": 0.9, "a": 1.0}}, "outline_color": {"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}}}]
    cnt_in, cnt_png = work / "contour.coree", work / "contour.png"
    _write_coree(cnt_in, "Contour", cnt_doc)
    if (e := _render_coree(ctx, cnt_in, cnt_png, work, dpi=72)) or compare_png_pixels(ctrl_png, cnt_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-45): Contour render failed or no-op: {e}")
    cnt_px = _sample_rgba_box_avg(cnt_png.read_bytes(), 120.0, 120.0, 26.0, 60.0, radius_px=1)
    if min(cnt_px[:3]) >= 240:
        return TestOutcome("FAIL", f"W3 ADD-01 (FEAT-45): Outside Contour ring missing at (26,60)mm: {cnt_px}")

    # Blend along S-curve with tangent rotation
    bl_doc = _make_base_doc("Tier3 Blend", width_mm=120.0, height_mm=120.0, bleed_mm=0.0, color_mode="Rgb")
    bl_doc["objects"]["10"] = _make_obj(10, "Bar1", _rect_payload(30.0, 4.0), tx=15.0, ty=28.0, fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}})
    bl_doc["objects"]["11"] = _make_obj(11, "Bar2", _rect_payload(30.0, 4.0), tx=75.0, ty=88.0, fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}})
    bl_doc["objects"]["12"] = _make_obj(12, "S-Path", {"Path": {"subpaths": [{"nodes": [{"pos": {"x": 30.0, "y": 30.0}, "handle_in": None, "handle_out": {"x": 90.0, "y": 30.0}, "kind": "Cusp"}, {"pos": {"x": 90.0, "y": 90.0}, "handle_in": {"x": 30.0, "y": 90.0}, "handle_out": None, "kind": "Cusp"}], "closed": False}]}}, fill="None")
    bl_doc["pages"][0]["layers"][0]["objects"] = [10, 11, 12]
    bl_ctrl_in, bl_ctrl_png = work / "bl_ctrl.coree", work / "bl_ctrl.png"
    _write_coree(bl_ctrl_in, "Blend Ctrl", bl_doc)
    if (e := _render_coree(ctx, bl_ctrl_in, bl_ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 FEAT-46: Blend control failed: {e}")
    bl_doc["objects"]["10"]["effects"] = [{"Blend": {"target_id": 11, "steps": 1, "rotation_deg": 0.0, "along_path": 12}}]
    bl_in, bl_png = work / "blend.coree", work / "blend.png"
    _write_coree(bl_in, "Blend", bl_doc)
    if (e := _render_coree(ctx, bl_in, bl_png, work, dpi=72)) or compare_png_pixels(bl_ctrl_png, bl_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-46): Blend along path failed or no-op: {e}")
    bl_bytes = bl_png.read_bytes()
    b_top = _sample_rgba_box_avg(bl_bytes, 120.0, 120.0, 60.0, 48.0, radius_px=1)
    b_bot = _sample_rgba_box_avg(bl_bytes, 120.0, 120.0, 60.0, 72.0, radius_px=1)
    b_gap = _sample_rgba_box_avg(bl_bytes, 120.0, 120.0, 48.0, 60.0, radius_px=1)
    if b_top[0] <= 150 or b_top[2] >= 100 or b_bot[0] <= 150 or b_bot[2] >= 100 or b_gap[0] < 245:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-46): Blend tangent-rotated midpoint check failed (top={b_top}, bot={b_bot}, gap={b_gap})")

    # 12-pt cubic Envelope
    env_doc = json.loads(json.dumps(base))
    env_doc["objects"]["10"]["effects"] = [{
        "Envelope": {
            "mode": "Unconstrained",
            "cage": [
                {"x": 0.0, "y": 0.0}, {"x": 20.0, "y": -20.0}, {"x": 40.0, "y": 20.0}, {"x": 60.0, "y": 0.0},
                {"x": 60.0, "y": 20.0}, {"x": 60.0, "y": 40.0}, {"x": 60.0, "y": 60.0}, {"x": 40.0, "y": 60.0},
                {"x": 20.0, "y": 60.0}, {"x": 0.0, "y": 60.0}, {"x": 0.0, "y": 40.0}, {"x": 0.0, "y": 20.0},
            ],
        }
    }]
    env_in, env_png = work / "envelope.coree", work / "envelope.png"
    _write_coree(env_in, "Envelope", env_doc)
    if (e := _render_coree(ctx, env_in, env_png, work, dpi=72)) or compare_png_pixels(ctrl_png, env_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", f"W3 IMP-04 (FEAT-48): 12-pt Envelope render failed or no-op: {e}")
    env_bytes = env_png.read_bytes()
    bulge_px = _sample_rgba_box_avg(env_bytes, 120.0, 120.0, 42.5, 27.0, radius_px=1)
    dip_px = _sample_rgba_box_avg(env_bytes, 120.0, 120.0, 77.5, 33.0, radius_px=1)
    tr_px = _sample_rgba_box_avg(env_bytes, 120.0, 120.0, 88.0, 35.0, radius_px=1)
    if min(bulge_px[:3]) >= 245 or min(dip_px[:3]) < 245 or tr_px[0] < 180 or tr_px[1] > 80 or tr_px[2] > 80:
        return TestOutcome(
            "FAIL",
            f"W3 IMP-04 (FEAT-48): 12-pt Envelope cubic S-curve check failed (bulge={bulge_px}, dip={dip_px}, top_right={tr_px})",
        )

    # 4. Structural kernel checks
    style_code = _read_rust_stripped(ctx, "crates/coree-core/src/style.rs")
    eff_code = _read_rust_stripped(ctx, "crates/coree-core/src/effects.rs")
    eff_ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/effects_render.rs")
    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")

    mesh_block = _extract_brace_block(style_code, r"\bpub\s+struct\s+MeshNode\s*\{") or ""
    if not all(re.search(rf"\b{h}\b", mesh_block) for h in ("handle_n", "handle_s", "handle_e", "handle_w")) or not re.search(r"\balpha\b", mesh_block):
        return TestOutcome("FAIL", "W3 IMP-04 (FEAT-33): MeshNode lacks Coons N/S/E/W tangent handles or alpha in style.rs")
    for eff_name in ("InnerShadow", "Bevel", "Extrude3D"):
        if not re.search(rf"\b{eff_name}\b", eff_code) or _has_empty_match_arm(eff_ren_code + ren_code, rf"(?:Effect::)?{eff_name}"):
            return TestOutcome("FAIL", f"W3 ADD-01: Effect::{eff_name} missing or empty stub")

    blend_fn = _extract_brace_block(eff_ren_code, r"\bfn\s+compute_blend_steps\b") or ""
    if re.search(r"\|\(\s*pt\s*,\s*_\s*\)\|", blend_fn) or not re.search(r"\b(?:atan2|tangent)\b", blend_fn):
        return TestOutcome("FAIL", "W3 IMP-04 (FEAT-46): compute_blend_steps discards path tangent")
    env_fn = _extract_brace_block(eff_ren_code, r"\bfn\s+apply_envelope_to_path\b") or ""
    if not re.search(r"cage\.len\(\)\s*>=\s*12\b|matches!\([^)]*cage\.len\(\)[^)]*\b12\b", env_fn):
        return TestOutcome("FAIL", "W3 IMP-04 (FEAT-48): apply_envelope_to_path lacks 12-pt shared-corner cubic cage (>=12 pts)")
    if not re.search(r"\bContour\b", eff_ren_code) or not re.search(r"\bExtrude3D\b", eff_ren_code):
        return TestOutcome("FAIL", "W3 ADD-01 (FEAT-45/FEAT-49): Contour / Extrude3D missing in effects_render.rs")

    return TestOutcome("PASS", "Coons MeshFill, InnerShadow, Bevel, Extrude3D, 6 TransparencyModes, Contour, Blend & 4-curve Envelope verified")


def _test_t3_w3_frozen_lens_and_glyph_remove_face(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W3-FROZEN-REMFACE")

    # 1. Verify frozen_snapshot survives .coree convert & underlying object removal
    doc = _make_base_doc("Tier3 Frozen Lens", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    snap_b64 = base64.b64encode(_encode_png_rgba_stdlib(40, 40, bytes([0, 180, 255, 255] * (40 * 40)))).decode("ascii")
    doc["objects"]["10"] = _make_obj(
        10,
        "Frozen Lens",
        _rect_payload(50.0, 50.0),
        tx=25.0,
        ty=25.0,
        lens={"kind": "Invert", "frozen": True, "viewpoint": None, "remove_face": False, "frozen_snapshot": snap_b64},
    )
    doc["pages"][0]["layers"][0]["objects"] = [10]
    in_p = work / "frozen.coree"
    rt_p = work / "frozen_rt.coree"
    out_png = work / "frozen.png"
    _write_coree(in_p, "Frozen", doc)
    r_rt = run_cmd([str(ctx.cli_bin), "convert", str(in_p), str(rt_p)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_rt.returncode != 0 or not rt_p.is_file():
        return TestOutcome("FAIL", f"W3 REM-04: Frozen lens convert failed: {r_rt.stderr}")
    _, rt_doc = inspect_coree_zip(rt_p)
    if rt_doc["objects"]["10"]["lens"].get("frozen_snapshot") != snap_b64:
        return TestOutcome("FAIL", "W3 REM-04 (FEAT-63): frozen_snapshot dropped during .coree convert")
    if (e := _render_coree(ctx, rt_p, out_png, work, dpi=72)) or parse_png_info(out_png.read_bytes())["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): frozen_snapshot failed to render with no underlying objects: {e}")
    try:
        snap_px = _sample_rgba_box_avg(out_png.read_bytes(), 100.0, 100.0, 50.0, 50.0, radius_px=2)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): frozen_snapshot PNG decode failed: {exc}")
    if abs(snap_px[0] - 0.0) > 10.0 or abs(snap_px[1] - 180.0) > 10.0 or abs(snap_px[2] - 255.0) > 10.0:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): frozen_snapshot center (50,50)mm expected cyan (0,180,255), got {snap_px}")

    # 2. Glyph-exact remove_face=True over Blue ArtisticText("O") at (35, 55)mm, 72pt — center counter hole & AABB corners must stay white!
    blue_style = _default_text_style("Noto Sans", 72.0)
    blue_style["fill"] = {"Rgba": {"r": 0.0, "g": 0.0, "b": 1.0, "a": 1.0}}
    rf_ctrl_doc = _make_base_doc("Tier3 RemoveFace O Ctrl", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    rf_ctrl_doc["objects"]["10"] = _make_obj(
        10,
        "Glyph O",
        {"ArtisticText": {"runs": [{"text": "O", "style": blue_style}], "direction": "Ltr", "align": "Left"}},
        tx=35.0,
        ty=55.0,
        fill="None",
    )
    rf_ctrl_doc["pages"][0]["layers"][0]["objects"] = [10]
    rf_ctrl_in = work / "rf_o_ctrl.coree"
    rf_ctrl_png = work / "rf_o_ctrl.png"
    _write_coree(rf_ctrl_in, "RemoveFace O Ctrl", rf_ctrl_doc)
    if (e := _render_coree(ctx, rf_ctrl_in, rf_ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 REM-04: remove_face control render failed: {e}")

    rf_doc = json.loads(json.dumps(rf_ctrl_doc))
    rf_doc["objects"]["11"] = _make_obj(
        11,
        "RemoveFace Lens",
        _rect_payload(80.0, 80.0),
        tx=10.0,
        ty=10.0,
        lens={"kind": "Invert", "frozen": False, "viewpoint": None, "remove_face": True, "frozen_snapshot": None},
    )
    rf_doc["pages"][0]["layers"][0]["objects"] = [10, 11]
    rf_in = work / "rf_o.coree"
    rf_png = work / "rf_o.png"
    _write_coree(rf_in, "RemoveFace O", rf_doc)
    if (e := _render_coree(ctx, rf_in, rf_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 REM-04: remove_face render failed: {e}")
    if compare_png_pixels(rf_ctrl_png, rf_png)["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", "W3 REM-04 (FEAT-63): remove_face Invert lens produced no pixel difference vs control")

    try:
        ctrl_bytes = rf_ctrl_png.read_bytes()
        rf_bytes = rf_png.read_bytes()
        ctrl_stroke = _sample_rgba_box_avg(ctrl_bytes, 100.0, 100.0, 37.8, 70.7, radius_px=1)
        lens_stroke = _sample_rgba_box_avg(rf_bytes, 100.0, 100.0, 37.8, 70.7, radius_px=1)
        counter_px = _sample_rgba_box_avg(rf_bytes, 100.0, 100.0, 44.5, 70.7, radius_px=1)
        aabb_c1 = _sample_rgba_box_avg(rf_bytes, 100.0, 100.0, 36.8, 62.0, radius_px=1)
        aabb_c2 = _sample_rgba_box_avg(rf_bytes, 100.0, 100.0, 35.7, 56.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): remove_face PNG decode failed: {exc}")
    if ctrl_stroke[0] > 15 or ctrl_stroke[1] > 15 or ctrl_stroke[2] < 240:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): control 'O' left stroke (37.8,70.7)mm expected pure blue, got {ctrl_stroke}")
    if lens_stroke[0] < 240 or lens_stroke[1] < 240 or lens_stroke[2] > 15:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): lensed 'O' stroke (37.8,70.7)mm expected inverted yellow, got {lens_stroke}")
    if min(counter_px[:3]) < 245 or min(aabb_c1[:3]) < 245 or min(aabb_c2[:3]) < 245:
        return TestOutcome("FAIL", f"W3 REM-04 (FEAT-63): remove_face=True falsely inverted non-glyph area (counter={counter_px}, aabb1={aabb_c1}, aabb2={aabb_c2})")

    lens_code = _read_rust_stripped(ctx, "crates/coree-core/src/lens.rs")
    if not re.search(r"\bfrozen_snapshot\b", lens_code):
        return TestOutcome("FAIL", "W3 REM-04 (FEAT-63): LensConfig::frozen_snapshot not persisted in lens.rs")

    return TestOutcome("PASS", "W3 Frozen Lens snapshot persistence & glyph-exact remove_face verified")


def _test_t3_w3_typography_wrap_fvar_convert_curves(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W3-TYPO")
    doc = _make_base_doc("Tier3 Typography", width_mm=120.0, height_mm=120.0, bleed_mm=0.0, color_mode="Rgb")
    para_style = _default_text_style("Noto Sans", 14.0)
    para_style["opentype_features"] = {"liga": True, "calt": True, "kern": True, "dlig": True, "smcp": True}
    para_style["font_weight"] = 700
    overflow_text = " ".join(["Drop cap paragraph flow across linked frames with contour wrap."] * 5)
    doc["objects"]["10"] = _make_obj(
        10,
        "Para Linked",
        {
            "ParagraphText": {
                "runs": [{"text": overflow_text, "style": para_style}],
                "frame_size": {"x": 45.0, "y": 30.0},
                "columns": 2,
                "column_gutter_mm": 4.0,
                "linked_next_frame": 11,
                "wrap_mode": "ContourLeft",
                "drop_cap": {"enabled": True, "lines": 3, "gap_mm": 1.5},
                "direction": "Ltr",
                "align": "Left",
            }
        },
        tx=10.0,
        ty=10.0,
    )
    doc["objects"]["11"] = _make_obj(
        11,
        "Para Frame 2",
        {
            "ParagraphText": {
                "runs": [],
                "frame_size": {"x": 45.0, "y": 40.0},
                "columns": 1,
                "column_gutter_mm": 4.0,
                "linked_next_frame": None,
                "wrap_mode": "None",
                "drop_cap": None,
                "direction": "Ltr",
                "align": "Left",
            }
        },
        tx=65.0,
        ty=10.0,
    )
    doc["pages"][0]["layers"][0]["objects"] = [10, 11]
    in_p = work / "typo.coree"
    out_png = work / "typo.png"
    _write_coree(in_p, "Typo", doc)
    if (e := _render_coree(ctx, in_p, out_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 ADD-10: linked ParagraphText render failed: {e}")

    nolink_doc = json.loads(json.dumps(doc))
    nolink_doc["objects"]["10"]["payload"]["ParagraphText"]["linked_next_frame"] = None
    nolink_in = work / "typo_nolink.coree"
    nolink_png = work / "typo_nolink.png"
    _write_coree(nolink_in, "Typo NoLink", nolink_doc)
    if (e := _render_coree(ctx, nolink_in, nolink_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 ADD-10: unlinked ParagraphText control render failed: {e}")

    def _count_frame2_non_white(png_path: Path) -> int:
        pw, ph, rgba = _decode_png_rgba_bytes_stdlib(png_path.read_bytes())
        x0, x1 = int(65.0 / 120.0 * pw), int(110.0 / 120.0 * pw)
        y0, y1 = int(10.0 / 120.0 * ph), int(50.0 / 120.0 * ph)
        cnt = 0
        for py in range(max(0, y0), min(ph, y1)):
            for px in range(max(0, x0), min(pw, x1)):
                off = (py * pw + px) * 4
                if rgba[off] < 245 or rgba[off + 1] < 245 or rgba[off + 2] < 245:
                    cnt += 1
        return cnt

    try:
        linked_f2_nw = _count_frame2_non_white(out_png)
        nolink_f2_nw = _count_frame2_non_white(nolink_png)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-10 (FEAT-38): linked ParagraphText PNG decode failed: {exc}")
    if linked_f2_nw - nolink_f2_nw <= 20:
        return TestOutcome("FAIL", f"W3 ADD-10 (FEAT-38): Frame 1 overflow did not populate Frame 2 (linked={linked_f2_nw}, nolink={nolink_f2_nw})")

    gate = require_cli_subcommand(ctx, "convert-to-curves", "W2/W3")
    if gate is not None:
        return gate

    curves_doc = _make_base_doc("Tier3 ConvertToCurves", width_mm=100.0, height_mm=60.0, bleed_mm=0.0, color_mode="Rgb")
    at_style = _default_text_style("Noto Sans", 28.0)
    at_style["opentype_features"] = {"liga": True, "calt": True, "kern": True, "dlig": False, "smcp": False}
    at_style["font_weight"] = 700
    curves_doc["objects"]["10"] = _make_obj(
        10,
        "Ligature Text",
        {"ArtisticText": {"runs": [{"text": "fi fl S", "style": at_style}], "direction": "Ltr", "align": "Left"}},
        tx=15.0,
        ty=40.0,
        fill="None",
    )
    curves_doc["pages"][0]["layers"][0]["objects"] = [10]
    curves_in = work / "curves_in.coree"
    curves_out = work / "curves_out.coree"
    curves_png = work / "curves_out.png"
    _write_coree(curves_in, "Curves In", curves_doc)
    r_cc = run_cmd([str(ctx.cli_bin), "convert-to-curves", str(curves_in), str(curves_out)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_cc.returncode != 0 or not curves_out.is_file():
        return TestOutcome("FAIL", f"W3 REM-02 (FEAT-40): convert-to-curves failed: {r_cc.stderr}")
    _, cc_doc = inspect_coree_zip(curves_out)
    cc_objs = list(cc_doc.get("objects", {}).values())
    if any(isinstance(o.get("payload"), dict) and "ArtisticText" in o["payload"] for o in cc_objs):
        return TestOutcome("FAIL", "W3 REM-02 (FEAT-40): convert-to-curves left ArtisticText unconverted")
    max_nodes = 0
    for o in cc_objs:
        p = o.get("payload")
        if isinstance(p, dict) and "Path" in p:
            for sp in p["Path"].get("subpaths", []):
                max_nodes = max(max_nodes, len(sp.get("nodes", [])))
    if max_nodes <= 4:
        return TestOutcome("FAIL", f"W3 REM-02 (FEAT-40): convert-to-curves emitted AABB/degenerate path (max_nodes={max_nodes})")
    if (e := _render_coree(ctx, curves_out, curves_png, work, dpi=72)) or parse_png_info(curves_png.read_bytes())["non_white_pixels"] < 50:
        return TestOutcome("FAIL", f"W3 REM-02 (FEAT-40): converted-to-curves output failed to render: {e}")

    text_code = _read_rust_stripped(ctx, "crates/coree-render/src/text_layout.rs")
    for sym in ("linked_next_frame", "TextWrapMode", "DropCap", "fvar", "SubpixelMask"):
        if not re.search(rf"\b{sym}\b", text_code):
            return TestOutcome("FAIL", f"W3 ADD-10 (FEAT-38/FEAT-40): {sym} not yet implemented in text_layout.rs")

    return TestOutcome("PASS", "W3 linked_next_frame overflow, TextWrapMode, DropCap, OpenTypeFeatures, fvar, SubpixelMask & convert-to-curves verified")


def _test_t3_w3_photo_studio_16bit_and_filters(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W3-PHOTO16")
    # 1. Verify baseline Bitmap Effects stack (FEAT-51) renders semantic diffs for GaussianBlur, Levels, HSL, ColorBalance, Sharpen, Noise
    doc = _make_base_doc("Tier3 Raster Stack", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    asset_uuid = "55555555-5555-4555-8555-555555555555"
    px = bytearray()
    for y in range(16):
        for x in range(16):
            px.extend((220, 40, 20, 255) if x < 8 else (20, 90, 210, 255))
    doc["assets"] = {
        asset_uuid: {"id": asset_uuid, "width": 16, "height": 16, "rgba_bytes": base64.b64encode(bytes(px)).decode("ascii"), "source_dpi": 72.0, "color_mode": "Rgb"}
    }
    doc["objects"]["10"] = _make_obj(10, "Raster", {"Raster": {"asset_id": asset_uuid, "width_mm": 60.0, "height_mm": 60.0, "filters": [], "crop_rect": None}}, tx=20.0, ty=20.0)
    doc["pages"][0]["layers"][0]["objects"] = [10]
    id_in = work / "ras_id.coree"
    id_png = work / "ras_id.png"
    _write_coree(id_in, "Raster Id", doc)
    if (e := _render_coree(ctx, id_in, id_png, work, dpi=72)):
        return TestOutcome("FAIL", f"Raster identity render failed: {e}")

    stack_doc = json.loads(json.dumps(doc))
    stack_doc["objects"]["10"]["payload"]["Raster"]["filters"] = [
        {"GaussianBlur": {"radius_px": 2.0}},
        {"Hsl": {"hue_deg": 90.0, "saturation": 0.2, "lightness": 0.05}},
        {"Noise": {"amount": 0.15, "monochrome": False, "seed": 42}},
    ]
    st_in = work / "ras_stack.coree"
    st_png = work / "ras_stack.png"
    _write_coree(st_in, "Raster Stack", stack_doc)
    if (e := _render_coree(ctx, st_in, st_png, work, dpi=72)) or compare_png_pixels(id_png, st_png)["mean_abs_diff"] <= 2.0:
        return TestOutcome("FAIL", f"W3 FEAT-51: RasterFilter stack render failed or no-op: {e}")

    # 2. Live ChannelMixer (FEAT-68) R<->G swap render check
    cm_doc = json.loads(json.dumps(doc))
    cm_doc["objects"]["10"]["payload"]["Raster"]["filters"] = [
        {"ChannelMixer": {"red": [0.0, 1.0, 0.0], "green": [1.0, 0.0, 0.0], "blue": [0.0, 0.0, 1.0], "monochrome": False}},
    ]
    cm_in = work / "ras_cm.coree"
    cm_png = work / "ras_cm.png"
    _write_coree(cm_in, "Raster ChannelMixer", cm_doc)
    if (e := _render_coree(ctx, cm_in, cm_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 ADD-12 (FEAT-68): ChannelMixer render failed: {e}")
    if compare_png_pixels(id_png, cm_png)["mean_abs_diff"] <= 5.0:
        return TestOutcome("FAIL", "W3 ADD-12 (FEAT-68): ChannelMixer produced insufficient diff vs identity")
    try:
        cm_bytes = cm_png.read_bytes()
        cm_px = _sample_rgba_box_avg(cm_bytes, 100.0, 100.0, 35.0, 50.0, radius_px=1)
        cm_ext = _sample_rgba_box_avg(cm_bytes, 100.0, 100.0, 5.0, 5.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 ADD-12 (FEAT-68): ChannelMixer PNG decode failed: {exc}")
    if not (cm_px[1] > cm_px[0] + 100 and cm_px[1] > 180 and cm_px[2] < 60 and cm_px[3] > 200) or min(cm_ext[:3]) < 245:
        return TestOutcome("FAIL", f"W3 ADD-12 (FEAT-68): ChannelMixer R<->G swap failed (left={cm_px}, ext={cm_ext})")

    # 3. Live Pixel Layers with BlendMode::Multiply + opacity + alpha + PowerClip Clip Mask (FEAT-65)
    top_uuid = "66666666-6666-4666-8666-666666666666"
    top_px = bytes((30, 210, 190, 180) * (16 * 16))
    pl_doc = _make_base_doc("Tier3 Pixel Layers", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    pl_doc["assets"] = {
        asset_uuid: doc["assets"][asset_uuid],
        top_uuid: {"id": top_uuid, "width": 16, "height": 16, "rgba_bytes": base64.b64encode(top_px).decode("ascii"), "source_dpi": 72.0, "color_mode": "Rgb"},
    }
    pl_doc["objects"]["10"] = _make_obj(10, "Bottom Red", {"Raster": {"asset_id": asset_uuid, "width_mm": 60.0, "height_mm": 60.0, "filters": [], "crop_rect": None}}, tx=20.0, ty=20.0)
    pl_doc["objects"]["11"] = _make_obj(11, "Top Semi", {"Raster": {"asset_id": top_uuid, "width_mm": 60.0, "height_mm": 60.0, "filters": [], "crop_rect": None}}, tx=20.0, ty=20.0, opacity=0.8)
    pl_doc["objects"]["11"]["blend_mode"] = "Multiply"
    pl_doc["objects"]["12"] = _make_obj(
        12,
        "Pixel Clip Mask",
        {
            "PowerClip": {
                "container": {
                    "subpaths": [
                        {
                            "nodes": [
                                {"pos": {"x": 35.0, "y": 35.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 65.0, "y": 35.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 65.0, "y": 65.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                                {"pos": {"x": 35.0, "y": 65.0}, "handle_in": None, "handle_out": None, "kind": "Cusp"},
                            ],
                            "closed": True,
                        }
                    ]
                },
                "container_parametric": None,
                "lock_contents": True,
                "contents": [11],
            }
        },
        fill="None",
    )
    pl_doc["pages"][0]["layers"][0]["objects"] = [10, 12]
    pl_in, pl_png = work / "pixel_layers.coree", work / "pixel_layers.png"
    _write_coree(pl_in, "Pixel Layers", pl_doc)
    if (e := _render_coree(ctx, pl_in, pl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): stacked pixel layer render failed: {e}")

    pl_norm_doc = json.loads(json.dumps(pl_doc))
    pl_norm_doc["objects"]["11"]["blend_mode"] = "Normal"
    pl_norm_in, pl_norm_png = work / "pixel_layers_norm.coree", work / "pixel_layers_norm.png"
    _write_coree(pl_norm_in, "Pixel Layers Normal", pl_norm_doc)
    if (e := _render_coree(ctx, pl_norm_in, pl_norm_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): Normal blend control render failed: {e}")
    if compare_png_pixels(pl_norm_png, pl_png)["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", "W3 IMP-06 (FEAT-65): Multiply vs Normal pixel layer blend produced no visual diff")

    pl_unclip_doc = json.loads(json.dumps(pl_doc))
    pl_unclip_doc["pages"][0]["layers"][0]["objects"] = [10, 11]
    pl_unclip_in, pl_unclip_png = work / "pixel_layers_unclip.coree", work / "pixel_layers_unclip.png"
    _write_coree(pl_unclip_in, "Pixel Layers Unclipped", pl_unclip_doc)
    if (e := _render_coree(ctx, pl_unclip_in, pl_unclip_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): unclipped pixel layer control failed: {e}")
    if compare_png_pixels(pl_unclip_png, pl_png)["mean_abs_diff"] <= 1.0:
        return TestOutcome("FAIL", "W3 IMP-06 (FEAT-65): PowerClip clip-mask on pixel layer produced no diff vs unclipped")

    try:
        pl_bytes = pl_png.read_bytes()
        pl_norm_bytes = pl_norm_png.read_bytes()
        pl_mid = _sample_rgba_box_avg(pl_bytes, 100.0, 100.0, 45.0, 50.0, radius_px=1)
        pl_norm_mid = _sample_rgba_box_avg(pl_norm_bytes, 100.0, 100.0, 45.0, 50.0, radius_px=1)
        pl_edge = _sample_rgba_box_avg(pl_bytes, 100.0, 100.0, 25.0, 25.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): Pixel Layer PNG decode failed: {exc}")
    if sum(pl_mid[:3]) >= sum(pl_norm_mid[:3]) - 10:
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): Multiply blend at (45,50)mm did not darken vs Normal (mul={pl_mid}, norm={pl_norm_mid})")
    if pl_edge[0] < 180 or pl_edge[1] > 80 or pl_edge[2] > 80:
        return TestOutcome("FAIL", f"W3 IMP-06 (FEAT-65): outside clip-mask (25,25)mm expected bottom red, got {pl_edge}")

    # 4. W3/W4 16-bit CoW Photo Studio (FEAT-65..69) + ChannelMixer source audit
    re_code = _read_rust_stripped(ctx, "crates/coree-render/src/raster_engine.rs")
    eff_code = _read_rust_stripped(ctx, "crates/coree-core/src/effects.rs")
    has_16bit_cow = bool(re.search(r"\b(?:cow|copy_on_write)\b|\bArc\s*<", re_code, re.I) and re.search(r"\b(?:u16_pixels|channels_u16|PixelDepth|Bit16|Rgba16)\b|\bVec\s*<\s*u16\s*>", re_code, re.I))
    if not has_16bit_cow or not re.search(r"\bChannelMixer\b", eff_code) or not re.search(r"\bChannelMixer\b", re_code):
        return TestOutcome("FAIL", "W3/W4 IMP-06/ADD-12 (FEAT-65..69): 16-bit CoW RasterAsset / ChannelMixer not yet implemented")
    for sym in ("MagicWand|Lasso|Feather", "CloneStamp|Healing|Poisson", "Bicubic|Lanczos"):
        if not re.search(rf"\b(?:{sym})\b", re_code, re.I):
            return TestOutcome("FAIL", f"W3/W4 IMP-06 (FEAT-66/67/69): missing Photo Studio capability ({sym}) in raster_engine.rs")

    return TestOutcome("PASS", "W3/W4 Photo Studio 16-bit CoW, Pixel Layers, ChannelMixer, Selection/Masking, Paint/Healing & Resample verified")


def _test_t3_w4_pdfx_gs_poppler_prepress(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W4-PDFX-PREPRESS")
    src = _t3_fixture(ctx, "visual_oracle_card.coree")
    out_pdf = work / "w4_pdfx.pdf"
    r = run_cmd([str(ctx.cli_bin), "convert", str(src), str(out_pdf), "--cmyk"], cwd=work, timeout=30.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0 or not out_pdf.is_file():
        return TestOutcome("FAIL", f"W4 REQ-03 (FEAT-83): PDF export failed: {r.stderr}")

    pdf_s = parse_pdf_structure(out_pdf.read_bytes())
    if not pdf_s["has_pdfx_version"] or not pdf_s["has_output_intents"]:
        return TestOutcome("FAIL", f"W4 REQ-03 (FEAT-83): PDF missing /GTS_PDFXVersion or /OutputIntents: {pdf_s}")

    fonts = run_poppler_fonts(out_pdf, timeout_scale=ctx.timeout_scale)
    cid_rows = [f for f in fonts.get("fonts", []) if ("CID" in f["type"] or "Type 0" in f["type"]) and f["emb"] == "yes" and f["sub"] == "yes" and f["uni"] == "yes"]
    if not fonts["ok"] or not cid_rows:
        return TestOutcome("FAIL", f"W4 REQ-03 (FEAT-83): pdffonts missing embedded+subsetted+ToUnicode Type0/CIDFontType2 row: {fonts.get('fonts')}")

    txt = run_poppler_text(out_pdf, timeout_scale=ctx.timeout_scale)
    if not txt["ok"] or "coree T3" not in txt["text"]:
        return TestOutcome("FAIL", f"W4 REQ-03 (FEAT-83): pdftotext failed to extract embedded text: {txt}")

    pinfo = run_poppler_info(out_pdf, timeout_scale=ctx.timeout_scale)
    if not pinfo["ok"] or "TrimBox" not in pinfo["fields"]:
        return TestOutcome("FAIL", f"W4 REQ-03: pdfinfo missing TrimBox: {pinfo}")

    # Live Spot + CMYK Separation Oracle (FEAT-76) via Ghostscript tiffsep
    sep_doc = _make_base_doc("Tier3 Spot Sep", width_mm=80.0, height_mm=80.0, bleed_mm=0.0, color_mode="Cmyk")
    sep_doc["objects"]["10"] = _make_obj(
        10,
        "Spot Pantone485",
        _rect_payload(28.0, 28.0),
        tx=10.0,
        ty=10.0,
        fill={"Solid": {"Spot": {"name": "Pantone485", "tint": 1.0, "fallback_cmyk": [0.0, 0.95, 0.9, 0.0], "alpha": 1.0}}},
    )
    sep_doc["objects"]["11"] = _make_obj(
        11,
        "CMYK Process",
        _rect_payload(28.0, 28.0),
        tx=42.0,
        ty=42.0,
        fill={"Solid": {"Cmyk": {"c": 0.85, "m": 0.25, "y": 0.15, "k": 0.1, "a": 1.0}}},
    )
    sep_doc["pages"][0]["layers"][0]["objects"] = [10, 11]
    sep_in = work / "sep_in.coree"
    sep_pdf = work / "sep_out.pdf"
    _write_coree(sep_in, "Spot Sep", sep_doc)
    r_sep = run_cmd([str(ctx.cli_bin), "convert", str(sep_in), str(sep_pdf), "--cmyk"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_sep.returncode != 0 or not sep_pdf.is_file():
        return TestOutcome("FAIL", f"W4 REQ-05 (FEAT-76): Spot CMYK PDF convert failed: {r_sep.stderr}")
    sep_bytes = sep_pdf.read_bytes()
    if b"/Separation" not in sep_bytes or b"Pantone485" not in sep_bytes:
        return TestOutcome("FAIL", "W4 REQ-05 (FEAT-76): exported PDF missing /Separation /Pantone485 colorant")
    gs_bin = shutil.which("gs")
    if not gs_bin:
        return TestOutcome("FAIL", "W4 REQ-05 (FEAT-76): Ghostscript (/usr/bin/gs) not found for tiffsep oracle")
    sep_dir = work / "tiffsep_plates"
    sep_dir.mkdir(parents=True, exist_ok=True)
    r_gs_sep = run_cmd(
        [gs_bin, "-q", "-dSAFER", "-dBATCH", "-dNOPAUSE", "-sDEVICE=tiffsep", "-sCompression=none", "-r72", "-sOutputFile=plate_%d.tif", str(sep_pdf.resolve())],
        cwd=sep_dir,
        timeout=30.0,
        timeout_scale=ctx.timeout_scale,
    )
    plate_names = {p.name for p in sep_dir.glob("*.tif")}
    required_plates = {"plate_1(Cyan).tif", "plate_1(Magenta).tif", "plate_1(Yellow).tif", "plate_1(Black).tif", "plate_1(Pantone485).tif"}
    if r_gs_sep.returncode != 0 or not required_plates.issubset(plate_names):
        return TestOutcome("FAIL", f"W4 REQ-05 (FEAT-76): gs tiffsep missing CMYK/Pantone485 plates (rc={r_gs_sep.returncode}, got={sorted(plate_names)})")

    # Verify plate_1(Pantone485).tif contains non-white ink pixels (pure-stdlib uncompressed 8-bit grayscale TIFF parser)
    def _tiff_plate_ink_pixels(tif_bytes: bytes) -> int:
        if len(tif_bytes) < 8 or tif_bytes[:2] not in (b"II", b"MM"):
            raise ValueError("Invalid TIFF header")
        endian = "<" if tif_bytes[:2] == b"II" else ">"
        ifd_off = struct.unpack_from(endian + "I", tif_bytes, 4)[0]
        if ifd_off + 2 > len(tif_bytes):
            raise ValueError("Truncated TIFF IFD")
        n_ent = struct.unpack_from(endian + "H", tif_bytes, ifd_off)[0]
        tags: dict[int, tuple[int, int, int]] = {}
        for idx_e in range(n_ent):
            t, typ, cnt, val_raw = struct.unpack_from(endian + "HHII", tif_bytes, ifd_off + 2 + idx_e * 12)
            tags[t] = (typ, cnt, val_raw)

        def _tag_vals(tag_id: int) -> list[int]:
            typ, cnt, val_raw = tags[tag_id]
            sz_map = {1: 1, 3: 2, 4: 4}
            fmt_map = {1: "B", 3: "H", 4: "I"}
            if typ not in sz_map:
                raise ValueError(f"Unsupported TIFF tag type {typ}")
            if cnt * sz_map[typ] <= 4:
                raw_b = struct.pack(endian + "I", val_raw)
                return list(struct.unpack_from(endian + f"{cnt}{fmt_map[typ]}", raw_b, 0))
            return list(struct.unpack_from(endian + f"{cnt}{fmt_map[typ]}", tif_bytes, val_raw))

        comp = _tag_vals(259)[0] if 259 in tags else 1
        bps = _tag_vals(258)[0] if 258 in tags else 8
        photo = _tag_vals(262)[0] if 262 in tags else 1
        if comp != 1 or bps != 8:
            raise ValueError(f"Expected uncompressed 8-bit TIFF, got comp={comp} bps={bps}")
        offsets = _tag_vals(273)
        counts = _tag_vals(279)
        ink = 0
        for off_s, cnt_s in zip(offsets, counts):
            strip = tif_bytes[off_s:off_s + cnt_s]
            if photo == 0:
                ink += sum(1 for v in strip if v > 5)
            else:
                ink += sum(1 for v in strip if v < 250)
        return ink

    try:
        spot_ink = _tiff_plate_ink_pixels((sep_dir / "plate_1(Pantone485).tif").read_bytes())
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REQ-05 (FEAT-76): failed to inspect plate_1(Pantone485).tif: {exc}")
    if spot_ink < 100:
        return TestOutcome("FAIL", f"W4 REQ-05 (FEAT-76): plate_1(Pantone485).tif has no ink pixels (ink={spot_ink})")

    # Imposition PerfectBound (FEAT-74), Vector Separations+Trapping+Halftone (FEAT-76), 2D Barcodes (FEAT-79) non-stub checks
    prepress_code = _read_rust_stripped(ctx, "crates/coree-prod/src/prepress.rs")
    barcode_code = _read_rust_stripped(ctx, "crates/coree-prod/src/barcode.rs")
    impose_fn = _extract_brace_block(prepress_code, r"\bfn\s+impose_document\b") or ""
    if not re.search(r"\bPerfectBound\b", impose_fn) or _has_empty_match_arm(impose_fn, r"(?:ImpositionMode::)?PerfectBound"):
        return TestOutcome("FAIL", "W4 REQ-05 (FEAT-74): non-stub PerfectBound imposition missing in prepress.rs::impose_document")
    for fn_rx, label in ((r"\bfn\s+\w*trap\w*\b", "Trapping"), (r"\bfn\s+\w*halftone\w*\b", "Halftone")):
        m_fn = re.search(fn_rx, prepress_code, re.I)
        body = _extract_brace_block(prepress_code, fn_rx) if m_fn else None
        if not body or _is_trivial_stub_body(body):
            return TestOutcome("FAIL", f"W4 REQ-05 (FEAT-76): non-stub {label} implementation missing in prepress.rs")
    if not re.search(r"\bTrapping\b", prepress_code) or not re.search(r"\bHalftone\b", prepress_code):
        return TestOutcome("FAIL", "W4 REQ-05 (FEAT-76): Trapping / Halftone types missing in prepress.rs")

    gen_bc_fn = _extract_brace_block(barcode_code, r"\bfn\s+generate_barcode\b") or ""
    for bsym in ("DataMatrix", "Pdf417", "Aztec", "Gs1128"):
        if not re.search(rf"\b{bsym}\b", gen_bc_fn) or _has_empty_match_arm(gen_bc_fn, rf"(?:BarcodeSymbology::)?{bsym}"):
            return TestOutcome("FAIL", f"W4 ADD-05 (FEAT-79): non-stub 2D barcode {bsym} missing in barcode.rs::generate_barcode")

    return TestOutcome("PASS", "W4 PDF/X-1a/4 + CIDFontType2 + pdftotext + gs tiffsep Spot/CMYK + PerfectBound + Separations + 2D Barcodes verified")


def _test_t3_w4_eps_alpha_svg_cad_psd(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W4-EPS-SVG-CAD")
    # 1. Transparent EPS alpha check via /usr/bin/gs pngalpha (FEAT-84)
    doc = _make_base_doc("Tier3 Transparent EPS", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    doc["objects"]["10"] = _make_obj(
        10,
        "Semi-Transparent Box",
        _rect_payload(50.0, 50.0),
        tx=25.0,
        ty=25.0,
        fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.2, "b": 0.1, "a": 1.0}}},
        effects=[{"Transparency": {"mode": {"Uniform": 0.5}, "blend_mode": "Normal"}}],
    )
    doc["pages"][0]["layers"][0]["objects"] = [10]
    in_p = work / "tr_eps.coree"
    out_eps = work / "tr_eps.eps"
    _write_coree(in_p, "Transparent EPS", doc)
    r_eps = run_cmd([str(ctx.cli_bin), "convert", str(in_p), str(out_eps)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_eps.returncode != 0 or not out_eps.is_file():
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-84): EPS convert failed: {r_eps.stderr}")
    gs_res = run_ghostscript_render(out_eps, work / "gs_eps_alpha.png", dpi=96, device="pngalpha", timeout_scale=ctx.timeout_scale)
    alpha_stats = gs_res.get("alpha_stats") or {}
    if not gs_res["ok"] or alpha_stats.get("opaque_white_pixels", 1) != 0 or alpha_stats.get("near_white_opaque_pixels", 1) != 0:
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-84): EPS white-knockout halo detected in gs pngalpha: {alpha_stats}")

    # 2. SVG TwoColorPattern dynamic geometry (FEAT-32, FEAT-84)
    pat_doc = _make_base_doc("SVG TwoColorPattern", width_mm=80.0, height_mm=80.0, bleed_mm=0.0, color_mode="Rgb")
    pat_doc["objects"]["10"] = _make_obj(10, "Pat", _rect_payload(50.0, 50.0), tx=15.0, ty=15.0, fill={"TwoColorPattern": {"fg": {"Rgba": {"r": 0.1, "g": 0.2, "b": 0.8, "a": 1.0}}, "bg": {"Rgba": {"r": 1.0, "g": 1.0, "b": 1.0, "a": 1.0}}, "pattern_name": "DiagonalStripe", "tile_w": 8.0, "tile_h": 8.0}})
    pat_doc["pages"][0]["layers"][0]["objects"] = [10]
    p_in = work / "pat.coree"
    p_svg = work / "pat.svg"
    _write_coree(p_in, "Pat", pat_doc)
    r_pat = run_cmd([str(ctx.cli_bin), "convert", str(p_in), str(p_svg)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_pat.returncode != 0 or not p_svg.is_file():
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-32/FEAT-84): SVG convert failed: {r_pat.stderr}")
    try:
        svg_root = ET.fromstring(p_svg.read_text(encoding="utf-8"))
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-32/FEAT-84): SVG XML malformed: {exc}")
    pats = [el for el in svg_root.iter() if el.tag.split("}", 1)[-1] == "pattern"]
    if not pats or [c.tag.split("}", 1)[-1] for c in pats[0]] == ["rect", "rect", "rect"]:
        return TestOutcome("FAIL", "W4 REM-08 (FEAT-32/FEAT-84): SVG TwoColorPattern emitted static 2x2 checkerboard")

    # 3. DXF TEXT/DIMENSION & PLT CI/AA (FEAT-85)
    card = _t3_fixture(ctx, "visual_oracle_card.coree")
    out_dxf = work / "card.dxf"
    out_plt = work / "card.plt"
    r_dxf = run_cmd([str(ctx.cli_bin), "convert", str(card), str(out_dxf)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_dxf.returncode != 0 or not out_dxf.is_file():
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-85): DXF convert failed: {r_dxf.stderr}")
    r_plt = run_cmd([str(ctx.cli_bin), "convert", str(card), str(out_plt)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_plt.returncode != 0 or not out_plt.is_file():
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-85): PLT convert failed: {r_plt.stderr}")
    try:
        dxf_s = parse_dxf_structure(out_dxf.read_bytes())
        plt_s = parse_plt_commands(out_plt.read_bytes())
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-85): DXF/PLT dissector failed: {exc}")
    if not any(e in ("TEXT", "MTEXT") for e in dxf_s["entities"]) or not plt_s["has_ci_or_aa"]:
        return TestOutcome("FAIL", f"W4 REM-08 (FEAT-85): DXF missing TEXT ({dxf_s['entities']}) or PLT missing CI/AA ({set(plt_s['opcodes'])})")

    # 4. PSD export/import >2048px / 16-bit / Spot / lsct + TIFF / WebP live codecs (FEAT-86)
    psd_code = _read_rust_stripped(ctx, "crates/coree-io/src/psd.rs")
    bridge_code = _read_rust_stripped(ctx, "crates/coree-app/src/io_bridge.rs")
    exp_psd_fn = _extract_brace_block(psd_code, r"\bfn\s+(?:export_psd|save_psd)\b") or ""
    if not exp_psd_fn or _is_trivial_stub_body(exp_psd_fn) or not re.search(r"\b(?:export_psd|save_psd)\b", bridge_code):
        return TestOutcome("FAIL", "W4 IMP-06 (FEAT-86): export_psd/save_psd missing in psd.rs or io_bridge.rs")
    if not re.search(r"\bSpot\b|\bspot_channel\b", psd_code) or re.search(r"if\s+!matches!\(\s*ch_id\s*,\s*-1\.\.=3\s*\)", psd_code):
        return TestOutcome("FAIL", "W4 IMP-06 (FEAT-86): psd.rs still skips Spot channels (ch_id >= 4)")
    if not re.search(r"\blsct\b", psd_code) or not re.search(r"\bu16\b|\b16\b", psd_code):
        return TestOutcome("FAIL", "W4 IMP-06 (FEAT-86): psd.rs lacks lsct or 16-bit parity")

    card_png = work / "card_ref.png"
    if (e := _render_coree(ctx, card, card_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W4 FEAT-86: card reference render failed: {e}")
    out_psd = work / "card.psd"
    psd_ren_png = work / "card_psd_ren.png"
    r_psd = run_cmd([str(ctx.cli_bin), "convert", str(card), str(out_psd)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r_psd.returncode != 0 or not out_psd.is_file():
        return TestOutcome("FAIL", f"W4 IMP-06 (FEAT-86): live .psd export failed: {r_psd.stderr}")
    try:
        psd_info = dissect_psd(out_psd.read_bytes())
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-06 (FEAT-86): exported PSD failed dissect_psd: {exc}")
    if psd_info.get("width", 0) < 10 or psd_info.get("height", 0) < 10:
        return TestOutcome("FAIL", f"W4 IMP-06 (FEAT-86): invalid PSD dimensions: {psd_info}")
    if (e := _render_coree(ctx, out_psd, psd_ren_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W4 IMP-06 (FEAT-86): PSD re-render failed: {e}")
    if compare_png_pixels(card_png, psd_ren_png)["mean_abs_diff"] >= 12.0:
        return TestOutcome("FAIL", "W4 IMP-06 (FEAT-86): PSD round-trip render diverged from reference")

    for ext in ("webp", "tif"):
        out_img = work / f"card.{ext}"
        ren_img = work / f"card_{ext}_ren.png"
        r_img = run_cmd([str(ctx.cli_bin), "convert", str(card), str(out_img)], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
        if r_img.returncode != 0 or not out_img.is_file():
            return TestOutcome("FAIL", f"W4 FEAT-86: .{ext} convert failed: {r_img.stderr}")
        if (e := _render_coree(ctx, out_img, ren_img, work, dpi=72)):
            return TestOutcome("FAIL", f"W4 FEAT-86: .{ext} re-render failed: {e}")
        try:
            if parse_png_info(ren_img.read_bytes())["non_white_pixels"] < 50:
                return TestOutcome("FAIL", f"W4 FEAT-86: .{ext} re-rendered blank")
        except _IO_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W4 FEAT-86: .{ext} PNG decode failed: {exc}")

    return TestOutcome("PASS", "W4 transparent EPS (gs pngalpha), dynamic SVG TwoColorPattern, DXF/PLT & PSD/raster codecs verified")


def _test_t3_w4_potrace_zerogap_oracle(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W4-POTRACE-ZEROGAP")
    adj_png = _t3_fixture(ctx, "potrace_adjacent.png")
    out_svg = work / "potrace_adj.svg"
    ren_png = work / "potrace_adj_ren.png"
    r = run_cmd([str(ctx.cli_bin), "trace", str(adj_png), str(out_svg), "--mode", "outline", "--colors", "4"], cwd=work, timeout=25.0, timeout_scale=ctx.timeout_scale)
    if r.returncode != 0 or not out_svg.is_file():
        return TestOutcome("FAIL", f"W4 IMP-05 (FEAT-70/71): trace failed: {r.stderr}")

    try:
        svg_root = ET.fromstring(out_svg.read_text(encoding="utf-8"))
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-05 (FEAT-71): traced SVG malformed: {exc}")
    paths = [el.attrib.get("d", "") for el in svg_root.iter() if el.tag.split("}", 1)[-1] == "path" and el.attrib.get("d", "").strip()]
    if len(paths) < 2 or not all(("Z" in d or "z" in d) for d in paths):
        return TestOutcome("FAIL", f"W4 IMP-05 (FEAT-71): expected >= 2 closed <path> elements, got {len(paths)}")

    if (e := _render_coree(ctx, out_svg, ren_png, work, dpi=300)):
        return TestOutcome("FAIL", f"W4 IMP-05: traced SVG render failed: {e}")

    try:
        rw, rh, rgba = _decode_png_rgba_bytes_stdlib(ren_png.read_bytes())
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W4 IMP-05: traced SVG PNG decode failed: {exc}")
    if rw < 16 or rh < 16:
        return TestOutcome("FAIL", f"W4 IMP-05: traced SVG render too small: {rw}x{rh}")
    cx = rw // 2
    white_seam = 0
    for yy in range(rh // 4, (3 * rh) // 4):
        for dx in (-1, 0, 1):
            off = (yy * rw + (cx + dx)) * 4
            if rgba[off] >= 245 and rgba[off + 1] >= 245 and rgba[off + 2] >= 245:
                white_seam += 1
    if white_seam > 0:
        return TestOutcome("FAIL", f"W4 IMP-05 (FEAT-70/FEAT-71): topological Potrace failed — {white_seam} white seam gap pixels along red/blue shared edge")

    return TestOutcome("PASS", "W4 topological zero-gap Potrace OutlineTrace (0 seam pixels) verified")


def _test_t3_w5_page_bg_spread_master_print_preview(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W5-PAGE-SPREAD-PREVIEW")
    # 1. Page.printable_background + Page.background_fill (FEAT-11)
    doc = _make_base_doc("Tier3 W5 PageBg", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    doc["pages"][0]["printable_background"] = True
    doc["pages"][0]["background_fill"] = {"Solid": {"Rgba": {"r": 0.12, "g": 0.50, "b": 0.75, "a": 1.0}}}
    bg_in = work / "page_bg.coree"
    bg_png = work / "page_bg.png"
    _write_coree(bg_in, "PageBg", doc)
    if (e := _render_coree(ctx, bg_in, bg_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W5 REQ-12 (FEAT-11): printable_background render failed: {e}")
    try:
        bg_info = parse_png_info(bg_png.read_bytes())
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 REQ-12 (FEAT-11): page_bg PNG decode failed: {exc}")
    if bg_info["corner_pixel"][:3] == (255, 255, 255):
        return TestOutcome("FAIL", f"W5 REQ-12 (FEAT-11): printable_background=True left corner white: {bg_info['corner_pixel']}")

    # 2. Master Layers OddPages/EvenPages/AllPages + AutoPageNumber live --page 0 / --page 1 render (FEAT-09)
    m_doc = _make_base_doc("Tier3 W5 MasterLayers", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    m_doc["objects"]["10"] = _make_obj(10, "Odd Red", _rect_payload(20.0, 20.0), tx=10.0, ty=10.0, fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}})
    m_doc["objects"]["11"] = _make_obj(11, "Even Blue", _rect_payload(20.0, 20.0), tx=60.0, ty=10.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.1, "b": 0.9, "a": 1.0}}})
    m_doc["objects"]["12"] = _make_obj(12, "All Green", _rect_payload(20.0, 20.0), tx=35.0, ty=60.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.75, "b": 0.2, "a": 1.0}}})
    apn_run = {"text": "P?", "style": _default_text_style("Noto Sans", 14.0), "field": "AutoPageNumber"}
    m_doc["objects"]["13"] = _make_obj(13, "PageNum", {"ArtisticText": {"runs": [apn_run], "direction": "Ltr", "align": "Left"}}, tx=10.0, ty=88.0, fill="None")
    m_doc["pages"][0]["layers"] = [
        {"id": 2, "name": "Master Odd", "visible": True, "locked": False, "printable": True, "master_scope": "OddPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [10]},
        {"id": 3, "name": "Master Even", "visible": True, "locked": False, "printable": True, "master_scope": "EvenPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [11]},
        {"id": 4, "name": "Master All", "visible": True, "locked": False, "printable": True, "master_scope": "AllPages", "is_cut_layer": False, "is_desktop_layer": False, "objects": [12, 13]},
    ]
    m_doc["pages"].append({
        "id": 101,
        "name": "Page 2",
        "width_mm": 100.0,
        "height_mm": 100.0,
        "bleed_mm": 0.0,
        "facing_pages": False,
        "layers": [
            {"id": 102, "name": "Artwork P2", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": []},
        ],
    })
    m_in = work / "master_pages.coree"
    p0_png = work / "master_p0.png"
    p1_png = work / "master_p1.png"
    _write_coree(m_in, "MasterLayers", m_doc)
    if (e := _render_coree(ctx, m_in, p0_png, work, dpi=72, extra_args=["--page", "0"])):
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): MasterLayers page 0 render failed: {e}")
    if (e := _render_coree(ctx, m_in, p1_png, work, dpi=72, extra_args=["--page", "1"])):
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): MasterLayers page 1 render failed: {e}")
    try:
        p0_bytes, p1_bytes = p0_png.read_bytes(), p1_png.read_bytes()
        p0_odd = _sample_rgba_box_avg(p0_bytes, 100.0, 100.0, 20.0, 20.0, radius_px=1)
        p0_even = _sample_rgba_box_avg(p0_bytes, 100.0, 100.0, 70.0, 20.0, radius_px=1)
        p0_all = _sample_rgba_box_avg(p0_bytes, 100.0, 100.0, 45.0, 70.0, radius_px=1)
        p1_odd = _sample_rgba_box_avg(p1_bytes, 100.0, 100.0, 20.0, 20.0, radius_px=1)
        p1_even = _sample_rgba_box_avg(p1_bytes, 100.0, 100.0, 70.0, 20.0, radius_px=1)
        p1_all = _sample_rgba_box_avg(p1_bytes, 100.0, 100.0, 45.0, 70.0, radius_px=1)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): MasterLayers PNG decode failed: {exc}")
    if p0_odd[0] < 200 or p0_odd[2] > 60 or min(p0_even[:3]) < 245 or p0_all[1] < 150:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): Page 1 (Odd) master layer mismatch (odd={p0_odd}, even={p0_even}, all={p0_all})")
    if min(p1_odd[:3]) < 245 or p1_even[2] < 200 or p1_even[0] > 60 or p1_all[1] < 150:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): Page 2 (Even) master layer mismatch (odd={p1_odd}, even={p1_even}, all={p1_all})")
    if compare_png_pixels(p0_png, p1_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", "W5 ADD-07 (FEAT-09): Page 0 vs Page 1 MasterLayers rendered identically")

    # Isolated AutoPageNumber local region diff (10..35, 78..95) mm (disjoint from Odd/Even/All rectangles)
    try:
        pw0, ph0, buf0 = _decode_png_rgba_bytes_stdlib(p0_bytes)
        pw1, ph1, buf1 = _decode_png_rgba_bytes_stdlib(p1_bytes)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): AutoPageNumber PNG decode failed: {exc}")
    if (pw0, ph0) != (pw1, ph1):
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-09): MasterLayers page dimensions mismatch: {(pw0, ph0)} vs {(pw1, ph1)}")
    x0_px, x1_px = max(0, int(10.0 / 100.0 * pw0)), min(pw0, int(35.0 / 100.0 * pw0))
    y0_px, y1_px = max(0, int(78.0 / 100.0 * ph0)), min(ph0, int(95.0 / 100.0 * ph0))
    apn_nw0 = apn_nw1 = apn_changed = 0
    for py in range(y0_px, y1_px):
        for px in range(x0_px, x1_px):
            off = (py * pw0 + px) * 4
            r0, g0, b0 = buf0[off], buf0[off + 1], buf0[off + 2]
            r1, g1, b1 = buf1[off], buf1[off + 1], buf1[off + 2]
            if r0 < 245 or g0 < 245 or b0 < 245:
                apn_nw0 += 1
            if r1 < 245 or g1 < 245 or b1 < 245:
                apn_nw1 += 1
            if abs(r0 - r1) + abs(g0 - g1) + abs(b0 - b1) > 30:
                apn_changed += 1
    if apn_nw0 < 10 or apn_nw1 < 10 or apn_changed < 5:
        return TestOutcome(
            "FAIL",
            f"W5 ADD-07 (FEAT-09): AutoPageNumber local region (10..35, 78..95)mm did not update across pages (nw0={apn_nw0}, nw1={apn_nw1}, changed={apn_changed})",
        )

    # 3. Facing Pages spread --multipage render (FEAT-10)
    sp_doc = _make_base_doc("Tier3 W5 FacingPages", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    sp_doc["objects"]["10"] = _make_obj(10, "P1 Red", _rect_payload(30.0, 30.0), tx=20.0, ty=20.0, fill={"Solid": {"Rgba": {"r": 0.9, "g": 0.1, "b": 0.1, "a": 1.0}}})
    sp_doc["objects"]["11"] = _make_obj(11, "P2 Green", _rect_payload(30.0, 30.0), tx=20.0, ty=20.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.8, "b": 0.2, "a": 1.0}}})
    sp_doc["objects"]["12"] = _make_obj(12, "P3 Blue", _rect_payload(30.0, 30.0), tx=20.0, ty=20.0, fill={"Solid": {"Rgba": {"r": 0.1, "g": 0.2, "b": 0.9, "a": 1.0}}})
    sp_doc["pages"][0]["layers"][0]["objects"] = [10]
    sp_doc["pages"].append({"id": 101, "name": "Page 2", "width_mm": 100.0, "height_mm": 100.0, "bleed_mm": 0.0, "facing_pages": False, "layers": [{"id": 102, "name": "L2", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [11]}]})
    sp_doc["pages"].append({"id": 103, "name": "Page 3", "width_mm": 100.0, "height_mm": 100.0, "bleed_mm": 0.0, "facing_pages": False, "layers": [{"id": 104, "name": "L3", "visible": True, "locked": False, "printable": True, "master_scope": "None", "is_cut_layer": False, "is_desktop_layer": False, "objects": [12]}]})
    sp_off_in, sp_off_png = work / "spread_off.coree", work / "spread_off.png"
    _write_coree(sp_off_in, "Spread Off", sp_doc)
    if (e := _render_coree(ctx, sp_off_in, sp_off_png, work, dpi=48, extra_args=["--multipage"])):
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-10): multipage spread_off render failed: {e}")
    sp_on_doc = json.loads(json.dumps(sp_doc))
    for pg in sp_on_doc["pages"]:
        pg["facing_pages"] = True
    sp_on_in, sp_on_png = work / "spread_on.coree", work / "spread_on.png"
    _write_coree(sp_on_in, "Spread On", sp_on_doc)
    if (e := _render_coree(ctx, sp_on_in, sp_on_png, work, dpi=48, extra_args=["--multipage"])):
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-10): multipage spread_on render failed: {e}")

    def _halves_non_white(png_path: Path) -> tuple[int, int, int, int]:
        pw, ph, rgba = _decode_png_rgba_bytes_stdlib(png_path.read_bytes())
        left_nw = right_nw = 0
        mid = pw // 2
        for py in range(ph):
            for px in range(pw):
                off = (py * pw + px) * 4
                if rgba[off] < 245 or rgba[off + 1] < 245 or rgba[off + 2] < 245:
                    if px < mid:
                        left_nw += 1
                    else:
                        right_nw += 1
        return pw, ph, left_nw, right_nw

    try:
        w_off, h_off, l_off, r_off = _halves_non_white(sp_off_png)
        w_on, h_on, l_on, r_on = _halves_non_white(sp_on_png)
    except _IO_PARSE_EXCEPTIONS as exc:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-10): spread PNG decode failed: {exc}")
    if l_off + r_off < 20 or l_on < 10 or r_on < 10:
        return TestOutcome("FAIL", f"W5 ADD-07 (FEAT-10): FacingPages spread missing left/right content (off={l_off}/{r_off}, on={l_on}/{r_on})")
    if (w_on, h_on) == (w_off, h_off) and compare_png_pixels(sp_off_png, sp_on_png)["mean_abs_diff"] <= 0.2:
        return TestOutcome("FAIL", "W5 ADD-07 (FEAT-10): facing_pages=True produced identical multipage layout to facing_pages=False")

    # 4. Facing Pages layout code, AutoPageNumber code & PrintPreviewDialog (FEAT-73)
    ren_code = _read_rust_stripped(ctx, "crates/coree-render/src/renderer.rs")
    layout_fn = _extract_brace_block(ren_code, r"\bfn\s+compute_page_layouts\b") or ""
    if "facing_pages" not in layout_fn:
        return TestOutcome("FAIL", "W5 ADD-07 (FEAT-10): compute_page_layouts ignores facing_pages spread layout")
    if not re.search(r"\bAutoPageNumber\b", ren_code + _read_rust_stripped(ctx, "crates/coree-core/src/text.rs")):
        return TestOutcome("FAIL", "W5 ADD-07 (FEAT-09): AutoPageNumber dynamic field missing")

    dlg_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/dialogs.rs")
    if not re.search(r"\bseparations_preview\b", dlg_code) or not re.search(r"\bdraw_print_preview_thumbnail\b", dlg_code):
        return TestOutcome("FAIL", "W5 IMP-09 (FEAT-73): interactive PrintPreviewDialog separations/tiling preview missing in dialogs.rs")

    return TestOutcome("PASS", "W5 printable Page background, MasterLayers Odd/Even/All, FacingPages spreads, AutoPageNumber & PrintPreviewDialog verified")


def _test_t3_w5_viewmodes_snap_shapes_sculpt(ctx: RunContext) -> TestOutcome:
    work = ctx.make_scratch_dir("T3-W5-VIEW-SNAP-SHAPES")
    # 1. simple-wireframe ViewMode gated via `render --help` (FEAT-05)
    r_help = run_cmd([str(ctx.cli_bin), "render", "--help"], cwd=work, timeout=15.0, timeout_scale=ctx.timeout_scale)
    if "simple-wireframe" not in r_help.stdout.lower():
        return TestOutcome("FAIL", "W5 ADD-08 (FEAT-05): --view simple-wireframe not exposed in coree-cli render --help")

    src = _t3_fixture(ctx, "pairwise_master.coree")
    swf_png = work / "swf.png"
    enh_png = work / "enh.png"
    if (e1 := _render_coree(ctx, src, swf_png, work, dpi=72, view="simple-wireframe")) or (e2 := _render_coree(ctx, src, enh_png, work, dpi=72, view="enhanced")):
        return TestOutcome("FAIL", f"W5 ADD-08: simple-wireframe/enhanced render failed: {e1} {e2}")
    if compare_png_pixels(swf_png, enh_png)["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", "W5 ADD-08 (FEAT-05): simple-wireframe identical to enhanced")

    # 2. All 5 Parametric Shapes live visual render (FEAT-23): Polygon, Star, Spiral, Impact, GraphPaper
    blank_doc = _make_base_doc("Tier3 Blank Ctrl", width_mm=100.0, height_mm=100.0, bleed_mm=0.0, color_mode="Rgb")
    ctrl_in, ctrl_png = work / "shapes_ctrl.coree", work / "shapes_ctrl.png"
    _write_coree(ctrl_in, "Blank Ctrl", blank_doc)
    if (e := _render_coree(ctx, ctrl_in, ctrl_png, work, dpi=72)):
        return TestOutcome("FAIL", f"W5 FEAT-23: blank control render failed: {e}")

    fill_gold = {"Solid": {"Rgba": {"r": 0.9, "g": 0.7, "b": 0.1, "a": 1.0}}}
    black_ol_08 = _default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 0.8, False)
    black_ol_06 = _default_outline({"Rgba": {"r": 0.0, "g": 0.0, "b": 0.0, "a": 1.0}}, 0.6, False)
    shape_specs = [
        ("Polygon", {"Shape": {"Polygon": {"sides": 6, "radius_x": 25.0, "radius_y": 25.0}}}, 50.0, 50.0, fill_gold, black_ol_06),
        ("Star", {"Shape": {"Star": {"points": 5, "outer_radius": 25.0, "inner_ratio": 0.45, "complex": False}}}, 50.0, 50.0, fill_gold, black_ol_06),
        ("Spiral", {"Shape": {"Spiral": {"revolutions": 3.0, "radius": 25.0, "logarithmic": False, "expansion": 2.0}}}, 50.0, 50.0, "None", black_ol_08),
        ("Impact", {"Shape": {"Impact": {"rays": 12, "inner_radius": 8.0, "outer_radius": 25.0, "style": "Radial"}}}, 50.0, 50.0, fill_gold, black_ol_06),
        ("GraphPaper", {"Shape": {"GraphPaper": {"cols": 3, "rows": 3, "width": 50.0, "height": 50.0}}}, 25.0, 25.0, "None", black_ol_06),
    ]
    shape_pngs: dict[str, Path] = {}
    for sname, spayload, sx, sy, sfill, sol in shape_specs:
        sdoc = json.loads(json.dumps(blank_doc))
        sdoc["objects"]["10"] = _make_obj(10, sname, spayload, tx=sx, ty=sy, fill=sfill, outline=sol)
        sdoc["pages"][0]["layers"][0]["objects"] = [10]
        sin, spng = work / f"shape_{sname}.coree", work / f"shape_{sname}.png"
        _write_coree(sin, sname, sdoc)
        if (e := _render_coree(ctx, sin, spng, work, dpi=72)):
            return TestOutcome("FAIL", f"W5 FEAT-23: {sname} render failed: {e}")
        try:
            snw = parse_png_info(spng.read_bytes())["non_white_pixels"]
        except _IO_PARSE_EXCEPTIONS as exc:
            return TestOutcome("FAIL", f"W5 FEAT-23: {sname} PNG decode failed: {exc}")
        if snw < 100 or compare_png_pixels(ctrl_png, spng)["mean_abs_diff"] <= 0.5:
            return TestOutcome("FAIL", f"W5 FEAT-23: {sname} rendered blank or matched control (nw={snw})")
        shape_pngs[sname] = spng
    if compare_png_pixels(shape_pngs["Polygon"], shape_pngs["Star"])["mean_abs_diff"] <= 0.5:
        return TestOutcome("FAIL", "W5 FEAT-23: Polygon and Star rendered identically")

    # 3. Genuine FEAT-02 Toolbox flyouts (15 groups / 65 distinct icons), Creation+Selection Property Bars, Status Bar & zero DrawingBox
    ty_code = "\n".join(
        filter(
            None,
            (
                _read_rust_stripped(ctx, "crates/coree-app/src/state/types.rs"),
                _read_rust_stripped(ctx, "crates/coree-tools/src/types.rs"),
            ),
        )
    )
    tb_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/toolbox.rs")
    bar_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/toolbar.rs")
    cv_code = _read_rust_stripped(ctx, "crates/coree-app/src/ui/canvas.rs")

    tool_id_block = _extract_brace_block(ty_code, r"\bpub\s+enum\s+ToolId\s*\{") or ""
    tool_variants = [v for v in re.findall(r"^\s*([A-Z][A-Za-z0-9_]*)\s*,", tool_id_block, re.M)]
    if "TOOL_GROUPS: [&[ToolId]; 15]" not in ty_code or len(tool_variants) != 65:
        return TestOutcome("FAIL", f"W5 IMP-08 (FEAT-02): expected 15 TOOL_GROUPS and 65 ToolId variants, got {len(tool_variants)}")
    pti_body = _extract_brace_block(tb_code, r"\bfn\s+paint_tool_icon\b") or ""
    shared_arms = re.findall(r"ToolId::([A-Za-z0-9_]+)\s*\|", pti_body)
    distinct_icons = set(re.findall(r"ToolId::([A-Za-z0-9_]+)", pti_body))
    if shared_arms or len(distinct_icons) < 65 or "open_flyout_group" not in tb_code or "TOOL_GROUPS" not in tb_code:
        return TestOutcome("FAIL", f"W5 IMP-08 (FEAT-02): toolbox.rs icon/flyout check failed (shared={shared_arms}, distinct={len(distinct_icons)})")
    if "show_context_property_contents" not in bar_code:
        return TestOutcome("FAIL", "W5 IMP-08 (FEAT-02): show_context_property_contents missing in toolbar.rs")
    required_prop_bars = (
        "ToolId::TwoPointLine",
        "NodePayload::Table",
        "NodePayload::Dimension",
        "NodePayload::Connector",
        "NodePayload::PowerClip",
        "ParametricShape::Polygon",
        "ParametricShape::Star",
        "ParametricShape::Spiral",
        "ParametricShape::Impact",
        "ParametricShape::GraphPaper",
    )
    missing_bars = [sym for sym in required_prop_bars if not re.search(rf"\b{re.escape(sym)}\b", bar_code)]
    if missing_bars:
        return TestOutcome("FAIL", f"W5 IMP-08 (FEAT-02): toolbar.rs missing context Property Bars for {missing_bars}")
    if not re.search(r"\bfn\s+show_status_bar\b", cv_code) or re.search(r"\bDrawingBox\b", cv_code):
        return TestOutcome("FAIL", "W5 IMP-08 (FEAT-02): canvas.rs missing show_status_bar or still contains DrawingBox fallback")

    # 4. 5 geometric snaps & guides/grids (FEAT-04) & Sculpting brushes (FEAT-16) in crates/coree-tools
    tools_src = "".join(
        strip_rust_comments_and_strings(p.read_text(encoding="utf-8", errors="replace"))
        for p in sorted((ctx.repo_root / "crates" / "coree-tools" / "src").rglob("*.rs"))
    )
    if not tools_src:
        return TestOutcome("FAIL", "W5 MND-02 (FEAT-04/FEAT-16): crates/coree-tools crate not yet implemented")
    for snap in ("Tangent", "Perpendicular", "Quadrant", "Intersection", "Baseline"):
        if not re.search(rf"\b{snap}\b", tools_src):
            return TestOutcome("FAIL", f"W5 ADD-08 (FEAT-04): geometric snap mode {snap} missing in coree-tools")
    for brush in ("Smear", "Twirl", "Attract", "Repel", "Roughen"):
        if not re.search(rf"\b{brush}\b", tools_src):
            return TestOutcome("FAIL", f"W5 FEAT-16: sculpting brush {brush} missing in coree-tools")

    return TestOutcome("PASS", "W5 simple-wireframe, 5 geometric snaps, 5 ParametricShapes, Sculpting brushes & 65/65 Toolbox/PropertyBars/StatusBar verified")


# ---------------------------------------------------------------------------
# Tier 3 Test Registry (Covers ALL 67 T3 FEAT-* Rows)
# ---------------------------------------------------------------------------

def get_tests() -> list[TestCase]:
    return [
        # --- Baseline (smoke=True) ---
        TestCase(
            id="T3-BASE-ICC-PARSER-DE2000",
            tier=3,
            title="Pure-Python stdlib ICC v2/v4 parser (colord+ghostscript) & CIEDE2000 math oracle",
            roadmap_ids=("REQ-02",),
            feature_refs=(),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_icc_parser_and_ciede2000,
        ),
        TestCase(
            id="T3-BASE-PAIRWISE-FILLS",
            tier=3,
            title="Pairwise ColorModels x ParametricShapes x Fills CLI render verification",
            roadmap_ids=("MND-02", "REQ-02", "IMP-11"),
            feature_refs=("FEAT-21", "FEAT-22", "FEAT-31"),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_pairwise_colors_shapes_fills,
        ),
        TestCase(
            id="T3-BASE-OUTLINE-PEN",
            tier=3,
            title="Pairwise OutlineStyle (dashes, arrowheads, calligraphy, hairline, behind_fill) visual diffs",
            roadmap_ids=("REM-01",),
            feature_refs=("FEAT-35",),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_outline_pen_pairwise,
        ),
        TestCase(
            id="T3-BASE-BLOCKSHADOW-DISTORT",
            tier=3,
            title="Baseline BlockShadow (FEAT-44) & Distort PushPull/Zipper/Twister (FEAT-47) visual oracles",
            roadmap_ids=("ADD-01", "MND-03"),
            feature_refs=("FEAT-44", "FEAT-47"),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_blockshadow_distort_effects,
        ),
        TestCase(
            id="T3-BASE-LENSES-SEMANTIC",
            tier=3,
            title="All 11 Lenses (FEAT-52..62) semantic pixel/channel checks + viewpoint X/Y offset",
            roadmap_ids=("REM-04",),
            feature_refs=(
                "FEAT-52", "FEAT-53", "FEAT-54", "FEAT-55", "FEAT-56", "FEAT-57",
                "FEAT-58", "FEAT-59", "FEAT-60", "FEAT-61", "FEAT-62",
            ),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_eleven_lenses_and_viewpoint,
        ),
        TestCase(
            id="T3-BASE-VISUAL-ORACLES",
            tier=3,
            title="ViewModes, TextOnPath, PrintersMarks, Trace & GS/Poppler TrimBox pixel parity",
            roadmap_ids=("REQ-03", "REQ-04", "IMP-01", "IMP-05", "ADD-10"),
            feature_refs=("FEAT-05", "FEAT-39", "FEAT-70", "FEAT-75"),
            target_wave="baseline",
            smoke=True,
            run=_test_t3_base_visual_oracles_pdf_eps_gs_trace,
        ),

        # --- Wave 2 (smoke=False) ---
        TestCase(
            id="T3-W2-BSPLINE-MEDIA-CLONE",
            tier=3,
            title="W2 ADD-02/FEAT-19/20/26: BSpline, ArtisticMedia, Clone, LiveSketch & Symmetry visual checks",
            roadmap_ids=("ADD-02", "ADD-01", "MND-02"),
            feature_refs=("FEAT-18", "FEAT-19", "FEAT-20", "FEAT-26"),
            target_wave="W2",
            smoke=False,
            run=_test_t3_w2_bspline_artistic_media_clone_symmetry,
        ),
        TestCase(
            id="T3-W2-DCEL-CURVES-BOOL",
            tier=3,
            title="W2 IMP-05/REM-01/03/10: Planar DCEL SmartFill, analytical erase_circle, continuity & BooleanOp",
            roadmap_ids=("IMP-05", "REM-01", "REM-03", "REM-10"),
            feature_refs=("FEAT-15", "FEAT-17", "FEAT-24", "FEAT-25"),
            target_wave="W2",
            smoke=False,
            run=_test_t3_w2_curves_dcel_smartfill_boolean,
        ),
        TestCase(
            id="T3-W2-PERSP-CLIP-DIM-CDR",
            tier=3,
            title="W2 MND-03/ADD-03/04: 3x3 Perspective, PowerClip, Dimension & ZCF .cdr visual round-trip",
            roadmap_ids=("MND-03", "ADD-03", "ADD-04", "REQ-06", "REM-05", "REM-06"),
            feature_refs=("FEAT-27", "FEAT-28", "FEAT-42", "FEAT-81"),
            target_wave="W2",
            smoke=False,
            run=_test_t3_w2_perspective_powerclip_dim_cdr,
        ),

        # --- Wave 3 (smoke=False) ---
        TestCase(
            id="T3-W3-ICC-BPC-CHAD-DE2000",
            tier=3,
            title="W3 REQ-02: FOGRA39/SWOP ICC + BPC + chad CLI render vs pure-Python CIEDE2000 < 1.0",
            roadmap_ids=("REQ-02", "IMP-03"),
            feature_refs=("FEAT-31", "FEAT-36"),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_icc_bpc_chad_ciede2000,
        ),
        TestCase(
            id="T3-W3-WGPU-CPU-GPU-PARITY",
            tier=3,
            title="W3 IMP-01/02: WgpuBackend vs TinySkiaBackend CPU/GPU pixel parity & WorldTileKey",
            roadmap_ids=("IMP-01", "IMP-02"),
            feature_refs=("FEAT-05",),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_wgpu_cpu_gpu_parity,
        ),
        TestCase(
            id="T3-W3-COONS-EFFECTS-TRANSP",
            tier=3,
            title="W3 ADD-01/IMP-04: Coons MeshFill, InnerShadow, Bevel, Extrude3D, 4 TransparencyModes, Contour, Blend & Envelope",
            roadmap_ids=("ADD-01", "IMP-04", "REQ-04"),
            feature_refs=("FEAT-33", "FEAT-43", "FEAT-45", "FEAT-46", "FEAT-48", "FEAT-49", "FEAT-50"),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_coons_mesh_and_effects_parity,
        ),
        TestCase(
            id="T3-W3-FROZEN-LENS-REMFACE",
            tier=3,
            title="W3 REM-04: Persistent frozen_snapshot lens & glyph-exact remove_face over 'O'",
            roadmap_ids=("REM-04",),
            feature_refs=("FEAT-63",),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_frozen_lens_and_glyph_remove_face,
        ),
        TestCase(
            id="T3-W3-TYPO-WRAP-FVAR-CURVES",
            tier=3,
            title="W3 ADD-10/REM-02: Linked frames, TextWrapMode, DropCap, OpenTypeFeatures, fvar, SubpixelMask & convert-to-curves",
            roadmap_ids=("ADD-10", "REM-02", "IMP-10"),
            feature_refs=("FEAT-38", "FEAT-40"),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_typography_wrap_fvar_convert_curves,
        ),
        TestCase(
            id="T3-W3-PHOTO16-FILTERS",
            tier=3,
            title="W3 IMP-06/ADD-12: Bitmap Effects stack, 16-bit CoW Photo Studio, ChannelMixer & Resample",
            roadmap_ids=("IMP-06", "ADD-12"),
            feature_refs=("FEAT-51", "FEAT-65", "FEAT-66", "FEAT-67", "FEAT-68", "FEAT-69"),
            target_wave="W3",
            smoke=False,
            run=_test_t3_w3_photo_studio_16bit_and_filters,
        ),

        # --- Wave 4 (smoke=False) ---
        TestCase(
            id="T3-W4-PDFX-GS-POPPLER-SEP",
            tier=3,
            title="W4 REQ-03/05/ADD-05: PDF/X-1a/4 + CIDFontType2 + PerfectBound + Separations + 2D Barcodes",
            roadmap_ids=("REQ-03", "REQ-04", "REQ-05", "ADD-05"),
            feature_refs=("FEAT-74", "FEAT-76", "FEAT-79", "FEAT-83"),
            target_wave="W4",
            smoke=False,
            run=_test_t3_w4_pdfx_gs_poppler_prepress,
        ),
        TestCase(
            id="T3-W4-EPS-ALPHA-SVG-CAD-PSD",
            tier=3,
            title="W4 REM-08/ADD-06: GS pngalpha EPS zero-halo, dynamic SVG TwoColorPattern, DXF/PLT & PSD",
            roadmap_ids=("REM-08", "ADD-06", "IMP-06"),
            feature_refs=("FEAT-32", "FEAT-84", "FEAT-85", "FEAT-86"),
            target_wave="W4",
            smoke=False,
            run=_test_t3_w4_eps_alpha_svg_cad_psd,
        ),
        TestCase(
            id="T3-W4-POTRACE-ZEROGAP",
            tier=3,
            title="W4 IMP-05: Topological zero-gap Potrace OutlineTrace over adjacent color regions",
            roadmap_ids=("IMP-05",),
            feature_refs=("FEAT-70", "FEAT-71"),
            target_wave="W4",
            smoke=False,
            run=_test_t3_w4_potrace_zerogap_oracle,
        ),

        # --- Wave 5 (smoke=False) ---
        TestCase(
            id="T3-W5-PAGEBG-SPREAD-PREVIEW",
            tier=3,
            title="W5 REQ-12/ADD-07/IMP-09: Printable Page Bg, FacingPages spreads, AutoPageNumber & PrintPreviewDialog",
            roadmap_ids=("REQ-12", "ADD-07", "IMP-09"),
            feature_refs=("FEAT-09", "FEAT-10", "FEAT-11", "FEAT-73"),
            target_wave="W5",
            smoke=False,
            run=_test_t3_w5_page_bg_spread_master_print_preview,
        ),
        TestCase(
            id="T3-W5-VIEW-SNAP-SHAPES-SCULPT",
            tier=3,
            title="W5 ADD-08/IMP-08: simple-wireframe, 5 geometric snaps, 5 ParametricShapes, Sculpting & 65/65 Toolbox/PropertyBars/StatusBar",
            roadmap_ids=("ADD-08", "IMP-08", "MND-02"),
            feature_refs=("FEAT-02", "FEAT-04", "FEAT-05", "FEAT-16", "FEAT-23"),
            target_wave="W5",
            smoke=False,
            run=_test_t3_w5_viewmodes_snap_shapes_sculpt,
        ),
    ]
