#!/usr/bin/env python3
"""Repository invariant verification gate for coree (MND-05).

Standard library only (Python 3.11+ for tomllib). Verifies:
1. `#![forbid(unsafe_code)]` across all crate roots, binaries, build.rs, and
   integration tests, `version.workspace = true` in all workspace crates, and
   zero `unsafe` / `allow|warn|deny|expect(unsafe_code)` in Rust code.
2. 100% Offline-First invariants: zero prohibited network crates in workspace
   manifests or Cargo.lock (allowing `tokio` solely as a wasm-bindgen-futures
   transitive dependency), zero `std::net`/`Tcp*`/`Udp*` usage outside the
   temporary W1.D boundary `crates/coree-app/src/ipc.rs` (or anywhere with
   `--strict-no-std-net`), and absence of `--share=network` in Flatpak.
3. Version consistency across `Cargo.toml`, `Cargo.lock`, `CHANGELOG.md`,
   Windows packaging (`coree.rc`, `coree.exe.manifest`, `wix/main.wxs`),
   Linux Flatpak (`io.github.ahrrfy.coree.metainfo.xml`), `.github/workflows/release.yml`,
   and `replica/launch/listing.json`.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import tomllib
import xml.etree.ElementTree as ET
from pathlib import Path

PROHIBITED_CRATES = {
    "reqwest",
    "ureq",
    "hyper",
    "hyper-util",
    "surf",
    "isahc",
    "attohttpc",
    "curl",
    "curl-sys",
    "minreq",
    "tiny_http",
    "h2",
    "h3",
    "quinn",
    "rustls",
    "native-tls",
    "openssl",
    "openssl-sys",
    "tungstenite",
    "tokio-tungstenite",
    "axum",
    "actix-web",
    "warp",
    "rocket",
    "tonic",
    "dns-lookup",
    "hickory-resolver",
    "trust-dns-resolver",
    "tokio",
    "async-std",
    "smol",
    "socket2",
}


CHAR_LIT_RX = re.compile(
    r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|[^\r\n])|[^'\\\r\n])'"
)
RAW_STR_PREFIX_RX = re.compile(r'(?:br|cr|r)(#*)"')


def strip_rust_comments_and_strings(src: str) -> str:
    """Strip Rust comments (`//`, `/* ... */` nested) and string/char literals."""
    out: list[str] = []
    i = 0
    n = len(src)
    while i < n:
        ch = src[i]
        nxt = src[i + 1] if i + 1 < n else ""
        # Line comment
        if ch == "/" and nxt == "/":
            i += 2
            while i < n and src[i] != "\n":
                i += 1
            continue
        # Nested block comment
        if ch == "/" and nxt == "*":
            depth = 1
            i += 2
            while i < n and depth > 0:
                if i + 1 < n and src[i] == "/" and src[i + 1] == "*":
                    depth += 1
                    i += 2
                elif i + 1 < n and src[i] == "*" and src[i + 1] == "/":
                    depth -= 1
                    i += 2
                else:
                    i += 1
            out.append(" ")
            continue
        at_token_start = i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")
        # Raw string: r#"..."#, br#"..."#, cr#"..."#
        if at_token_start and ch in ("r", "b", "c"):
            m = RAW_STR_PREFIX_RX.match(src, i)
            if m:
                hashes = len(m.group(1))
                j = m.end()
                end_pat = '"' + ("#" * hashes)
                end_idx = src.find(end_pat, j)
                if end_idx == -1:
                    i = n
                else:
                    i = end_idx + len(end_pat)
                out.append(" ")
                continue
        # Byte-character literal b'...'
        if at_token_start and ch == "b" and nxt == "'":
            m = CHAR_LIT_RX.match(src, i + 1)
            if m:
                i = m.end()
                out.append(" ")
                continue
        # Character literal '...' (must NOT consume lifetimes like 'a / 'static / '_)
        if ch == "'":
            m = CHAR_LIT_RX.match(src, i)
            if m:
                i = m.end()
                out.append(" ")
                continue
        # Normal, byte, or C string: "...", b"...", c"..."
        if ch == '"' or (at_token_start and ch in ("b", "c") and nxt == '"'):
            i += 2 if ch in ("b", "c") else 1
            while i < n:
                if src[i] == "\\":
                    i += 2
                elif src[i] == '"':
                    i += 1
                    break
                else:
                    i += 1
            out.append(" ")
            continue
        out.append(ch)
        i += 1
    return "".join(out)


def check_unsafe_invariants(repo_root: Path) -> list[str]:
    errors: list[str] = []
    crate_dirs = sorted(p for p in (repo_root / "crates").iterdir() if p.is_dir())
    if not crate_dirs:
        return ["no crate directories found under crates/"]

    for cdir in crate_dirs:
        toml_path = cdir / "Cargo.toml"
        if not toml_path.is_file():
            errors.append(f"missing {toml_path.relative_to(repo_root)}")
            continue
        try:
            manifest = tomllib.loads(toml_path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            errors.append(f"failed to parse {toml_path.relative_to(repo_root)}: {exc}")
            continue
        pkg = manifest.get("package", {})
        if pkg.get("version") != {"workspace": True}:
            errors.append(
                f"{toml_path.relative_to(repo_root)}: package.version must be `version.workspace = true` (got {pkg.get('version')!r})"
            )

        required_forbid_files: list[Path] = []
        for rel in ("src/lib.rs", "src/main.rs", "build.rs"):
            candidate = cdir / rel
            if candidate.is_file():
                required_forbid_files.append(candidate)
        for sub in ("src/bin", "tests"):
            sdir = cdir / sub
            if sdir.is_dir():
                required_forbid_files.extend(sorted(sdir.glob("*.rs")))

        for fpath in required_forbid_files:
            code_only = strip_rust_comments_and_strings(fpath.read_text(encoding="utf-8"))
            if not re.search(r"#!\[\s*forbid\s*\(\s*unsafe_code\s*\)\s*\]", code_only):
                errors.append(
                    f"missing `#![forbid(unsafe_code)]` in {fpath.relative_to(repo_root)}"
                )

    unsafe_kw_rx = re.compile(r"\bunsafe\b")
    allow_unsafe_rx = re.compile(r"\b(?:allow|warn|deny|expect)\s*\([^)]*\bunsafe_code\b")
    for rs_file in sorted((repo_root / "crates").rglob("*.rs")):
        code_only = strip_rust_comments_and_strings(rs_file.read_text(encoding="utf-8"))
        if unsafe_kw_rx.search(code_only):
            errors.append(
                f"prohibited `unsafe` token found in {rs_file.relative_to(repo_root)}"
            )
        if allow_unsafe_rx.search(code_only):
            errors.append(
                f"prohibited `allow/warn/deny/expect(unsafe_code)` found in {rs_file.relative_to(repo_root)}"
            )

    return errors


def collect_manifest_dep_names(data: dict) -> set[str]:
    names: set[str] = set()
    for section in ("dependencies", "dev-dependencies", "build-dependencies"):
        tbl = data.get(section)
        if isinstance(tbl, dict):
            for k, v in tbl.items():
                pkg = v.get("package") if isinstance(v, dict) else None
                names.add(str(pkg or k))
    ws_deps = data.get("workspace", {}).get("dependencies")
    if isinstance(ws_deps, dict):
        for k, v in ws_deps.items():
            pkg = v.get("package") if isinstance(v, dict) else None
            names.add(str(pkg or k))
    target_tbl = data.get("target")
    if isinstance(target_tbl, dict):
        for tdata in target_tbl.values():
            if isinstance(tdata, dict):
                names.update(collect_manifest_dep_names(tdata))
    return names


def check_offline_invariants(repo_root: Path, strict_no_std_net: bool) -> list[str]:
    errors: list[str] = []

    manifests = [repo_root / "Cargo.toml"] + sorted((repo_root / "crates").glob("*/Cargo.toml"))
    workspace_crate_names: set[str] = set()
    for mpath in manifests:
        try:
            data = tomllib.loads(mpath.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            errors.append(f"failed to parse {mpath.relative_to(repo_root)}: {exc}")
            continue
        if mpath != repo_root / "Cargo.toml":
            pkg_name = data.get("package", {}).get("name")
            if isinstance(pkg_name, str):
                workspace_crate_names.add(pkg_name)
        deps = collect_manifest_dep_names(data)
        bad = sorted(deps & PROHIBITED_CRATES)
        if bad:
            errors.append(
                f"{mpath.relative_to(repo_root)} declares prohibited network/runtime crate(s): {', '.join(bad)}"
            )

    lock_path = repo_root / "Cargo.lock"
    if not lock_path.is_file():
        errors.append("missing Cargo.lock")
    else:
        try:
            lock_data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
        except (OSError, tomllib.TOMLDecodeError) as exc:
            errors.append(f"failed to parse Cargo.lock: {exc}")
            lock_data = {}
        packages = lock_data.get("package", [])
        rev_deps: dict[str, set[str]] = {}
        present_names: set[str] = set()
        for pkg in packages:
            pname = str(pkg.get("name", ""))
            present_names.add(pname)
            for dep_spec in pkg.get("dependencies", []):
                dep_name = str(dep_spec).split()[0]
                rev_deps.setdefault(dep_name, set()).add(pname)
        for bad_pkg in sorted(present_names & PROHIBITED_CRATES):
            parents = rev_deps.get(bad_pkg, set())
            if bad_pkg == "tokio" and parents == {"wasm-bindgen-futures"}:
                continue
            errors.append(
                f"Cargo.lock contains prohibited crate `{bad_pkg}` (required by: {sorted(parents)})"
            )

    net_rx = re.compile(r"\b(?:std::net|TcpListener|TcpStream|UdpSocket)\b")
    for rs_file in sorted((repo_root / "crates").rglob("*.rs")):
        rel = rs_file.relative_to(repo_root).as_posix()
        code_only = strip_rust_comments_and_strings(rs_file.read_text(encoding="utf-8"))
        if net_rx.search(code_only):
            if not strict_no_std_net and rel == "crates/coree-app/src/ipc.rs":
                continue
            errors.append(f"prohibited std::net / socket usage in {rel}")

    flatpak_yml = repo_root / "packaging/linux/flatpak/io.github.ahrrfy.coree.yml"
    if flatpak_yml.is_file():
        yml_text = flatpak_yml.read_text(encoding="utf-8")
        try:
            import yaml  # type: ignore

            parsed_yml = yaml.safe_load(yml_text)
            finish_args = parsed_yml.get("finish-args", []) if isinstance(parsed_yml, dict) else []
            if "--share=network" in finish_args:
                errors.append("Flatpak manifest finish-args must NOT include --share=network")
        except ImportError:
            for line in yml_text.splitlines():
                stripped = line.split("#", 1)[0].strip()
                if stripped in ("--share=network", "- --share=network", "- '--share=network'", '- "--share=network"'):
                    errors.append("Flatpak manifest must NOT include --share=network")
    else:
        errors.append("missing packaging/linux/flatpak/io.github.ahrrfy.coree.yml")

    return errors


def check_version_consistency(repo_root: Path, expected_version: str | None) -> tuple[str | None, list[str]]:
    errors: list[str] = []
    root_toml = tomllib.loads((repo_root / "Cargo.toml").read_text(encoding="utf-8"))
    ver = root_toml.get("workspace", {}).get("package", {}).get("version")
    if not isinstance(ver, str) or not re.fullmatch(r"\d+\.\d+\.\d+", ver):
        return None, [f"invalid Cargo.toml [workspace.package].version: {ver!r}"]

    if expected_version and ver != expected_version:
        errors.append(
            f"Cargo.toml version `{ver}` != --expected-version `{expected_version}`"
        )

    ver_comma = ver.replace(".", ",") + ",0"
    ver4 = f"{ver}.0"

    # Cargo.lock workspace crate versions
    lock_path = repo_root / "Cargo.lock"
    if lock_path.is_file():
        lock_data = tomllib.loads(lock_path.read_text(encoding="utf-8"))
        ws_members: set[str] = set()
        for m in root_toml.get("workspace", {}).get("members", []):
            member_toml = repo_root / m / "Cargo.toml"
            if member_toml.is_file():
                try:
                    mdata = tomllib.loads(member_toml.read_text(encoding="utf-8"))
                    pkg_name = mdata.get("package", {}).get("name")
                    if isinstance(pkg_name, str):
                        ws_members.add(pkg_name)
                except (OSError, tomllib.TOMLDecodeError) as exc:
                    errors.append(
                        f"failed to parse {member_toml.relative_to(repo_root)}: {exc}"
                    )
        for pkg in lock_data.get("package", []):
            if pkg.get("name") in ws_members and "source" not in pkg and pkg.get("version") != ver:
                errors.append(
                    f"Cargo.lock package `{pkg.get('name')}` has version {pkg.get('version')!r}, expected {ver!r}"
                )

    # CHANGELOG.md
    changelog_text = (repo_root / "CHANGELOG.md").read_text(encoding="utf-8")
    if not re.search(rf"^## \[{re.escape(ver)}\]", changelog_text, re.M):
        errors.append(f"CHANGELOG.md missing heading `## [{ver}]`")

    # packaging/windows/coree.rc
    rc_text = (repo_root / "packaging/windows/coree.rc").read_text(encoding="utf-8")
    for pattern, label in (
        (rf"^\s*FILEVERSION\s+{re.escape(ver_comma)}\b", f"FILEVERSION {ver_comma}"),
        (rf"^\s*PRODUCTVERSION\s+{re.escape(ver_comma)}\b", f"PRODUCTVERSION {ver_comma}"),
        (rf'"FileVersion"\s*,\s*"{re.escape(ver4)}\\0"', f'"FileVersion", "{ver4}\\0"'),
        (rf'"ProductVersion"\s*,\s*"{re.escape(ver)}\\0"', f'"ProductVersion", "{ver}\\0"'),
    ):
        if not re.search(pattern, rc_text, re.M):
            errors.append(f"packaging/windows/coree.rc missing `{label}`")

    # packaging/windows/coree.exe.manifest
    manifest_tree = ET.parse(repo_root / "packaging/windows/coree.exe.manifest")
    asm_id = manifest_tree.getroot().find("{urn:schemas-microsoft-com:asm.v1}assemblyIdentity")
    if asm_id is None or asm_id.attrib.get("version") != ver4:
        got = asm_id.attrib.get("version") if asm_id is not None else None
        errors.append(
            f"packaging/windows/coree.exe.manifest assemblyIdentity version={got!r}, expected {ver4!r}"
        )

    # packaging/windows/wix/main.wxs
    wxs_path = repo_root / "packaging/windows/wix/main.wxs"
    wxs_text = wxs_path.read_text(encoding="utf-8")
    if not re.search(rf'<\?define\s+Version\s*=\s*"{re.escape(ver)}"\?>', wxs_text):
        errors.append(f'packaging/windows/wix/main.wxs missing `<?define Version = "{ver}"?>`')
    try:
        wxs_tree = ET.parse(wxs_path)
        pkg_el = wxs_tree.getroot().find("{http://wixtoolset.org/schemas/v4/wxs}Package")
        if pkg_el is None or pkg_el.attrib.get("Version") != "$(var.Version)":
            got_ver = pkg_el.attrib.get("Version") if pkg_el is not None else None
            errors.append(
                f'packaging/windows/wix/main.wxs <Package Version=...> must be "$(var.Version)" (got {got_ver!r})'
            )
    except ET.ParseError as exc:
        errors.append(f"packaging/windows/wix/main.wxs XML parse error: {exc}")

    # packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml
    meta_tree = ET.parse(repo_root / "packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml")
    releases = [el.attrib.get("version") for el in meta_tree.getroot().findall("./releases/release")]
    if ver not in releases:
        errors.append(
            f"packaging/linux/flatpak/io.github.ahrrfy.coree.metainfo.xml missing <release version=\"{ver}\"> (found {releases})"
        )

    # .github/workflows/release.yml
    rel_text = (repo_root / ".github/workflows/release.yml").read_text(encoding="utf-8")
    if f"default: 'v{ver}'" not in rel_text:
        errors.append(f".github/workflows/release.yml missing `default: 'v{ver}'`")
    if f'$msiVer = "{ver}"' not in rel_text:
        errors.append(f'.github/workflows/release.yml missing `$msiVer = "{ver}"`')

    # replica/launch/listing.json
    listing_data = json.loads(
        (repo_root / "replica/launch/listing.json").read_text(encoding="utf-8")
    )
    whats_new = str(listing_data.get("app_store", {}).get("whats_new", ""))
    if f"v{ver}" not in whats_new:
        errors.append(
            f"replica/launch/listing.json app_store.whats_new does not mention `v{ver}`"
        )

    return ver, errors


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Verify coree MND-05 safety, offline, and version invariants.")
    ap.add_argument("--repo-root", default=".", help="Repository root directory")
    ap.add_argument("--expected-version", default=None, help="Assert Cargo.toml version equals this semver")
    ap.add_argument(
        "--strict-no-std-net",
        action="store_true",
        help="Disallow std::net even in crates/coree-app/src/ipc.rs (post-W1.D)",
    )
    args = ap.parse_args(argv)

    repo_root = Path(args.repo_root).resolve()
    unsafe_errs = check_unsafe_invariants(repo_root)
    offline_errs = check_offline_invariants(repo_root, args.strict_no_std_net)
    ver, ver_errs = check_version_consistency(repo_root, args.expected_version)

    all_errors = unsafe_errs + offline_errs + ver_errs
    if all_errors:
        print("verify_invariants: FAILED:", file=sys.stderr)
        for err in all_errors:
            print(f"  - {err}", file=sys.stderr)
        return 1

    print(
        f"verify_invariants: PASS (version={ver}, forbid_unsafe=all, offline_first=clean, strict_no_std_net={args.strict_no_std_net})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
