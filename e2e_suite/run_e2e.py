#!/usr/bin/env python3
"""Unified CLI runner for the coree opaque-box E2E test suite (Tiers 1-4)."""

from __future__ import annotations

import argparse
import importlib
import json
import math
import sys
import tempfile
import time
import traceback
from pathlib import Path
from typing import Any, Optional

# Ensure repo root is on sys.path whether invoked as `python3 e2e_suite/run_e2e.py`
# or `python3 -m e2e_suite.run_e2e`.
_E2E_ROOT = Path(__file__).resolve().parent
_REPO_ROOT = _E2E_ROOT.parent
if str(_REPO_ROOT) not in sys.path:
    sys.path.insert(0, str(_REPO_ROOT))

from e2e_suite.harness import (  # noqa: E402
    ALL_51_ROADMAP_IDS,
    VALID_WAVES,
    FeaturesCatalog,
    RunContext,
    TestCase,
    TestOutcome,
    discover_binaries,
    discover_cli_subcommands,
    discover_icc_dir,
    discover_replica_skills_dir,
    dissect_acb,
    dissect_dwg,
    dissect_emf,
    dissect_psd,
    ensure_tier1_fixtures,
    load_features_csv,
)

TIER_MODULES: dict[int, str] = {
    1: "e2e_suite.tier1_cli_roundtrip",
    2: "e2e_suite.tier2_boundary_gui",
    3: "e2e_suite.tier3_color_visual",
    4: "e2e_suite.tier4_stress_perf",
}


def load_available_tiers() -> dict[int, Any]:
    loaded: dict[int, Any] = {}
    for tier_num, mod_name in TIER_MODULES.items():
        mod_file = _E2E_ROOT / f"{mod_name.split('.')[-1]}.py"
        if not mod_file.is_file():
            continue
        mod = importlib.import_module(mod_name)
        loaded[tier_num] = mod
    return loaded


def validate_suite(
    loaded_tiers: dict[int, Any],
    *,
    require_all_tiers: bool,
    cli_bin_override: Optional[Path],
    bin_dir_override: Optional[Path],
) -> tuple[bool, dict[str, Any]]:
    errors: list[str] = []
    warnings: list[str] = []

    if require_all_tiers and len(loaded_tiers) < 4:
        missing_t = sorted(set(TIER_MODULES) - set(loaded_tiers))
        errors.append(f"--require-all-tiers specified, but tiers {missing_t} are not present")

    # 1. features.csv structural validation
    catalog: Optional[FeaturesCatalog] = None
    try:
        catalog = load_features_csv(_REPO_ROOT)
    except Exception as exc:
        errors.append(f"features.csv check failed: {exc}")

    # 2. TierModule contract & TestCase validation
    all_tests: list[TestCase] = []
    seen_ids: set[str] = set()
    roadmap_set = set(ALL_51_ROADMAP_IDS)
    valid_waves = set(VALID_WAVES)
    valid_feat_ids = {f.id for f in catalog.counted} if catalog else set()

    for expected_tier, mod in sorted(loaded_tiers.items()):
        for attr in ("TIER", "TIER_NAME", "get_tests"):
            if not hasattr(mod, attr):
                errors.append(f"{mod.__name__} missing required TierModule attribute '{attr}'")
                continue
        if getattr(mod, "TIER", None) != expected_tier:
            errors.append(f"{mod.__name__}.TIER={getattr(mod, 'TIER', None)} != {expected_tier}")
        try:
            tests = mod.get_tests()
        except Exception as exc:
            errors.append(f"{mod.__name__}.get_tests() raised {exc}")
            continue
        if not isinstance(tests, list) or not tests:
            errors.append(f"{mod.__name__}.get_tests() returned empty or non-list")
            continue

        for tc in tests:
            if not isinstance(tc, TestCase):
                errors.append(f"{mod.__name__} returned non-TestCase object: {tc!r}")
                continue
            if tc.id in seen_ids:
                errors.append(f"Duplicate TestCase.id: {tc.id}")
            seen_ids.add(tc.id)
            if tc.tier != expected_tier:
                errors.append(f"{tc.id}: tier={tc.tier} != module TIER={expected_tier}")
            if tc.target_wave not in valid_waves:
                errors.append(f"{tc.id}: invalid target_wave={tc.target_wave!r}")
            if tc.smoke and tc.target_wave != "baseline":
                errors.append(f"{tc.id}: smoke=True is only allowed on target_wave='baseline' (got {tc.target_wave!r})")
            if not tc.roadmap_ids:
                errors.append(f"{tc.id}: roadmap_ids must not be empty")
            for rid in tc.roadmap_ids:
                if rid not in roadmap_set:
                    errors.append(f"{tc.id}: unknown roadmap_id {rid!r}")
            for fref in tc.feature_refs:
                if catalog is not None and fref not in catalog.by_id:
                    errors.append(f"{tc.id}: unknown feature_ref {fref!r} (expected FEAT-01..86 / SKIP-01..06 / EXT-F01..12)")
            all_tests.append(tc)

    # 3. 51/51 Roadmap ID coverage across loaded tests
    covered_roadmap = {rid for tc in all_tests for rid in tc.roadmap_ids}
    missing_roadmap = [rid for rid in ALL_51_ROADMAP_IDS if rid not in covered_roadmap]
    if missing_roadmap:
        errors.append(f"Missing roadmap ID coverage ({len(missing_roadmap)}): {missing_roadmap}")

    # 4. 86/86 FEAT-01..FEAT-86 coverage check
    covered_feats = {fref for tc in all_tests for fref in tc.feature_refs if fref in valid_feat_ids}
    missing_feats = sorted(valid_feat_ids - covered_feats)
    if missing_feats:
        if len(loaded_tiers) == 4 or require_all_tiers:
            errors.append(
                f"Strict 86/86 features.csv coverage missing {len(missing_feats)} features: {missing_feats}"
            )
        else:
            warnings.append(
                f"Partial tiers loaded ({sorted(loaded_tiers)}): {len(covered_feats)}/86 features.csv rows mapped "
                f"(remaining {len(missing_feats)} allocated to Tiers 2-4)"
            )

    # 5. Offline readiness checks
    icc_dir = discover_icc_dir()
    if icc_dir is None:
        errors.append("System ICC directory /usr/share/color/icc/ not found")
    else:
        icc_count = len(list(icc_dir.rglob("*.icc")))
        if icc_count == 0:
            errors.append("No .icc profiles found under /usr/share/color/icc/")

    for rel in ("replica/features.csv", "replica/brand.json", "replica/design/tokens.json", "replica/launch/listing.json"):
        if not (_REPO_ROOT / rel).is_file():
            errors.append(f"Missing required offline replica artifact: {rel}")

    skills_dir = discover_replica_skills_dir()
    if skills_dir is None:
        errors.append("Replica skills directory not found (set REPLICA_SKILLS_DIR if non-default)")
    else:
        for script_rel in (
            "replica-brand/sweep.py",
            "replica-design/contrast.py",
            "replica-launch/listing.py",
            "replica-diff/parity.py",
        ):
            if not (skills_dir / script_rel).is_file():
                errors.append(f"Missing replica skill script: {skills_dir / script_rel}")

    cli_bin_path: Optional[Path] = None
    subcommands: list[str] = []
    try:
        cli_bin_path, _gui = discover_binaries(
            _REPO_ROOT,
            cli_bin_override=cli_bin_override,
            bin_dir_override=bin_dir_override,
            allow_cargo_build=False,
        )
        subcommands = sorted(discover_cli_subcommands(cli_bin_path))
    except Exception as exc:
        errors.append(f"Binary discovery/subcommand probe failed: {exc}")

    # Ensure deterministic Tier 1 fixtures exist and pass binary dissector sanity checks
    if 1 in loaded_tiers:
        t1_fixtures = ensure_tier1_fixtures(_E2E_ROOT / "fixtures" / "tier1")
        try:
            dissect_dwg(t1_fixtures["probe_w4.dwg"].read_bytes())
            dissect_emf(t1_fixtures["probe_w4.emf"].read_bytes())
            dissect_acb(t1_fixtures["probe_w4.acb"].read_bytes())
            dissect_psd(t1_fixtures["fixture.psd"].read_bytes())
        except Exception as exc:
            errors.append(f"Tier 1 binary probe fixture self-check failed: {exc}")

    report = {
        "ok": len(errors) == 0,
        "loaded_tiers": sorted(loaded_tiers.keys()),
        "total_tests": len(all_tests),
        "smoke_tests": sum(1 for t in all_tests if t.smoke),
        "roadmap_covered": f"{len(covered_roadmap)}/{len(ALL_51_ROADMAP_IDS)}",
        "features_csv_counted": len(catalog.counted) if catalog else 0,
        "features_csv_mapped": f"{len(covered_feats)}/86",
        "cli_bin": str(cli_bin_path) if cli_bin_path else None,
        "cli_subcommands": subcommands,
        "errors": errors,
        "warnings": warnings,
    }
    return report["ok"], report


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(description="Run coree opaque-box E2E test suite")
    parser.add_argument("--tier", type=int, action="append", choices=[1, 2, 3, 4], help="Tier number to run (repeatable)")
    parser.add_argument("--all", action="store_true", help="Run all loaded tiers")
    parser.add_argument("--smoke", action="store_true", help="Run only baseline smoke=True tests across loaded tiers")
    parser.add_argument("--validate", action="store_true", help="Validate tier contracts, coverage, and offline readiness")
    parser.add_argument("--require-all-tiers", action="store_true", help="Fail validation/run if any of Tiers 1-4 is missing")
    parser.add_argument("--list", action="store_true", help="List selected tests without executing them")
    parser.add_argument("--json-out", type=Path, help="Write structured JSON test report to this path")
    parser.add_argument("--bin-dir", type=Path, help="Directory containing coree-cli / coree binaries")
    parser.add_argument("--cli-bin", type=Path, help="Explicit path to coree-cli binary")
    parser.add_argument("--timeout-scale", type=float, default=1.0, help="Multiplier applied to subprocess timeouts")

    args = parser.parse_args(argv)
    if not math.isfinite(args.timeout_scale) or args.timeout_scale <= 0.0:
        print("error: --timeout-scale must be finite and > 0", file=sys.stderr)
        return 2

    loaded_tiers = load_available_tiers()
    if not loaded_tiers:
        print("error: no tier modules found in e2e_suite/", file=sys.stderr)
        return 2

    if args.require_all_tiers and len(loaded_tiers) < 4:
        missing = sorted(set(TIER_MODULES) - set(loaded_tiers))
        print(f"error: --require-all-tiers failed; missing tier modules for tiers {missing}", file=sys.stderr)
        return 2

    if args.tier:
        missing_req = [t for t in args.tier if t not in loaded_tiers]
        if missing_req:
            print(
                f"error: requested tier(s) {missing_req} not yet implemented in e2e_suite/ "
                f"(available: {sorted(loaded_tiers)})",
                file=sys.stderr,
            )
            return 2

    if args.validate:
        ok, vreport = validate_suite(
            loaded_tiers,
            require_all_tiers=args.require_all_tiers,
            cli_bin_override=args.cli_bin,
            bin_dir_override=args.bin_dir,
        )
        print(
            f"[validate] ok={ok} tiers={vreport['loaded_tiers']} "
            f"tests={vreport['total_tests']} (smoke={vreport['smoke_tests']}) "
            f"roadmap={vreport['roadmap_covered']} features_mapped={vreport['features_csv_mapped']}"
        )
        if vreport["cli_bin"]:
            print(f"[validate] cli_bin={vreport['cli_bin']} subcommands={len(vreport['cli_subcommands'])}")
        for w in vreport["warnings"]:
            print(f"  WARN: {w}")
        for e in vreport["errors"]:
            print(f"  ERROR: {e}", file=sys.stderr)
        if args.json_out:
            args.json_out.parent.mkdir(parents=True, exist_ok=True)
            args.json_out.write_text(json.dumps(vreport, indent=2), encoding="utf-8")
        return 0 if ok else 1

    # Select tests to list or run
    selected_tiers = sorted(set(args.tier)) if args.tier else sorted(loaded_tiers.keys())
    if not (args.all or args.tier or args.smoke or args.list):
        parser.print_help()
        return 2

    selected_tests: list[TestCase] = []
    for tnum in selected_tiers:
        for tc in loaded_tiers[tnum].get_tests():
            if args.smoke and not tc.smoke:
                continue
            selected_tests.append(tc)

    if args.list:
        for tc in selected_tests:
            rids = ",".join(tc.roadmap_ids)
            print(f"T{tc.tier} [{tc.target_wave:8s}] {'SMOKE' if tc.smoke else 'WAVE ':5s} {tc.id:26s} ({rids}) - {tc.title}")
        print(f"Total listed: {len(selected_tests)}")
        return 0

    try:
        catalog = load_features_csv(_REPO_ROOT)
        cli_bin, gui_bin = discover_binaries(
            _REPO_ROOT,
            cli_bin_override=args.cli_bin,
            bin_dir_override=args.bin_dir,
            allow_cargo_build=True,
        )
        subcommands = discover_cli_subcommands(cli_bin)
    except Exception as exc:
        print(f"error: {exc}", file=sys.stderr)
        return 2
    skills_dir = discover_replica_skills_dir()
    icc_dir = discover_icc_dir()

    fixtures_base = _E2E_ROOT / "fixtures"
    if 1 in selected_tiers:
        ensure_tier1_fixtures(fixtures_base / "tier1")

    results: list[dict[str, Any]] = []
    counts = {"PASS": 0, "FAIL": 0, "ERROR": 0}
    by_wave: dict[str, dict[str, int]] = {}
    by_tier: dict[str, dict[str, int]] = {}

    suite_t0 = time.perf_counter()
    with tempfile.TemporaryDirectory(prefix="coree_e2e_") as tmp_root:
        ctx = RunContext(
            repo_root=_REPO_ROOT,
            e2e_root=_E2E_ROOT,
            fixtures_dir=fixtures_base,
            cli_bin=cli_bin,
            gui_bin=gui_bin,
            replica_skills_dir=skills_dir,
            timeout_scale=args.timeout_scale,
            scratch_root=Path(tmp_root),
            cli_subcommands=subcommands,
            features_catalog=catalog,
            icc_dir=icc_dir,
        )

        print(f"Running {len(selected_tests)} E2E tests | cli={cli_bin} | subcommands={len(subcommands)}")
        for tc in selected_tests:
            t0 = time.perf_counter()
            try:
                outcome = tc.run(ctx)
                if not isinstance(outcome, TestOutcome):
                    outcome = TestOutcome("ERROR", f"Invalid TestOutcome returned: {outcome!r}")
            except Exception as exc:
                tb = traceback.format_exc(limit=4)
                outcome = TestOutcome("ERROR", f"Unhandled exception: {exc}\n{tb}")
            dt_ms = outcome.duration_ms if outcome.duration_ms > 0.0 else (time.perf_counter() - t0) * 1000.0

            counts[outcome.status] = counts.get(outcome.status, 0) + 1
            w_bucket = by_wave.setdefault(tc.target_wave, {"PASS": 0, "FAIL": 0, "ERROR": 0})
            w_bucket[outcome.status] += 1
            t_bucket = by_tier.setdefault(f"T{tc.tier}", {"PASS": 0, "FAIL": 0, "ERROR": 0})
            t_bucket[outcome.status] += 1

            results.append({
                "id": tc.id,
                "tier": tc.tier,
                "title": tc.title,
                "target_wave": tc.target_wave,
                "smoke": tc.smoke,
                "roadmap_ids": list(tc.roadmap_ids),
                "feature_refs": list(tc.feature_refs),
                "status": outcome.status,
                "duration_ms": round(dt_ms, 2),
                "message": outcome.message,
                "metrics": outcome.metrics,
            })
            print(f"  [{outcome.status:5s}] {tc.id:26s} ({tc.target_wave:8s}, {dt_ms:7.1f}ms) — {outcome.message}")

    total_ms = (time.perf_counter() - suite_t0) * 1000.0
    print("\nSummary by Wave:")
    for w in VALID_WAVES:
        if w in by_wave:
            b = by_wave[w]
            print(f"  {w:8s}: PASS={b['PASS']} FAIL={b['FAIL']} ERROR={b['ERROR']}")
    print(
        f"Total ({total_ms:.1f}ms): PASS={counts['PASS']} FAIL={counts['FAIL']} ERROR={counts['ERROR']}"
    )

    if args.json_out:
        payload = {
            "cli_bin": str(cli_bin),
            "gui_bin": str(gui_bin) if gui_bin else None,
            "cli_subcommands": sorted(subcommands),
            "total_duration_ms": round(total_ms, 2),
            "counts": counts,
            "by_wave": by_wave,
            "by_tier": by_tier,
            "results": results,
        }
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(payload, indent=2), encoding="utf-8")

    return 0 if (counts["FAIL"] == 0 and counts["ERROR"] == 0) else 1


if __name__ == "__main__":
    raise SystemExit(main())
