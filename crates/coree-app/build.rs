// SPDX-License-Identifier: MIT OR Apache-2.0
#![forbid(unsafe_code)]

//! Build script embedding the Windows Win32 icon (`coree.ico`), version metadata,
//! and PerMonitorV2 DPI + UTF-8 codepage manifest (`coree.exe.manifest`) into
//! `coree.exe` when targeting Windows (`msvc` or `gnu`).
//!
//! Cleanly no-ops on non-Windows targets or when no Windows resource compiler is
//! installed on the host machine.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../packaging/windows/coree.rc");
    println!("cargo:rerun-if-changed=../../packaging/windows/coree.ico");
    println!("cargo:rerun-if-changed=../../packaging/windows/coree.exe.manifest");

    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    let Ok(manifest_dir) = env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    let Ok(out_dir) = env::var("OUT_DIR") else {
        return;
    };

    let win_pkg_dir = PathBuf::from(&manifest_dir)
        .join("..")
        .join("..")
        .join("packaging")
        .join("windows");
    let rc_path = win_pkg_dir.join("coree.rc");
    let ico_path = win_pkg_dir.join("coree.ico");
    let manifest_path = win_pkg_dir.join("coree.exe.manifest");

    if !rc_path.is_file() || !ico_path.is_file() {
        return;
    }

    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_else(|_| "x86_64".to_owned());
    // MSVC link.exe consumes `.res`; MinGW/GNU gcc linker driver recognizes `.o` COFF objects.
    let res_path = if target_env == "msvc" {
        PathBuf::from(&out_dir).join("coree.res")
    } else {
        PathBuf::from(&out_dir).join("coree_res.o")
    };

    let compiled = if target_env == "msvc" {
        compile_for_msvc(&win_pkg_dir, &rc_path, &res_path, &target_arch)
    } else {
        compile_for_gnu(&win_pkg_dir, &rc_path, &res_path, &target_arch)
    };

    if compiled && fs::metadata(&res_path).is_ok_and(|m| m.len() > 0) {
        println!("cargo:rustc-link-arg-bin=coree={}", res_path.display());
        if target_env == "msvc" && manifest_path.is_file() {
            // Prevent MSVC link.exe from generating a conflicting default RT_MANIFEST
            // alongside `1 24 "coree.exe.manifest"` embedded in `coree.res`.
            println!("cargo:rustc-link-arg-bin=coree=/MANIFEST:NO");
        }
    }
}

fn compile_for_msvc(
    win_pkg_dir: &Path,
    rc_path: &Path,
    res_path: &Path,
    target_arch: &str,
) -> bool {
    // 1. Prefer Microsoft Windows SDK `rc.exe` (PATH first, then Windows Kits 10 discovery)
    if let Some((rc_exe, sdk_includes)) = find_msvc_rc(target_arch) {
        let mut cmd = Command::new(&rc_exe);
        cmd.current_dir(win_pkg_dir)
            .arg("/nologo")
            .arg("/I")
            .arg(win_pkg_dir);
        for inc in &sdk_includes {
            cmd.arg("/I").arg(inc);
        }
        cmd.arg("/fo").arg(res_path).arg(rc_path);
        if run_resource_compiler(cmd, &rc_exe) {
            return true;
        }
    }

    // 2. Fallback: LLVM `llvm-rc`
    for candidate in ["llvm-rc.exe", "llvm-rc"] {
        if let Some(exe) = find_on_path(candidate) {
            let mut cmd = Command::new(&exe);
            cmd.current_dir(win_pkg_dir)
                .arg("/I")
                .arg(win_pkg_dir)
                .arg("/fo")
                .arg(res_path)
                .arg(rc_path);
            if run_resource_compiler(cmd, &exe) {
                return true;
            }
        }
    }

    // 3. Fallback: MinGW `windres` emitting raw `.res`
    if let Some(exe) = find_windres(target_arch) {
        let mut cmd = Command::new(&exe);
        cmd.current_dir(win_pkg_dir)
            .arg("-I")
            .arg(win_pkg_dir)
            .arg("-O")
            .arg("res")
            .arg("-i")
            .arg(rc_path)
            .arg("-o")
            .arg(res_path);
        if run_resource_compiler(cmd, &exe) {
            return true;
        }
    }

    false
}

fn compile_for_gnu(win_pkg_dir: &Path, rc_path: &Path, res_path: &Path, target_arch: &str) -> bool {
    // GNU/MinGW linkers consume COFF object format (`-O coff`) with matching PE target arch.
    if let Some(exe) = find_windres(target_arch) {
        let mut cmd = Command::new(&exe);
        cmd.current_dir(win_pkg_dir)
            .arg("-I")
            .arg(win_pkg_dir)
            .arg("-O")
            .arg("coff");
        if let Some(pe_target) = windres_pe_target(target_arch) {
            cmd.arg(format!("--target={pe_target}"));
        }
        cmd.arg("-i").arg(rc_path).arg("-o").arg(res_path);
        if run_resource_compiler(cmd, &exe) {
            return true;
        }
    }
    false
}

fn windres_pe_target(target_arch: &str) -> Option<&'static str> {
    match target_arch {
        "x86_64" => Some("pe-x86-64"),
        "x86" => Some("pe-i386"),
        _ => None,
    }
}

fn run_resource_compiler(mut cmd: Command, exe: &Path) -> bool {
    match cmd.output() {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let detail = if !stderr.trim().is_empty() {
                stderr.trim()
            } else {
                stdout.trim()
            };
            println!(
                "cargo:warning=Windows resource compiler '{}' failed (status {}): {}",
                exe.display(),
                output.status,
                detail
            );
            false
        }
        Err(_) => false,
    }
}

fn find_msvc_rc(target_arch: &str) -> Option<(PathBuf, Vec<PathBuf>)> {
    // Search strictly for `rc.exe` (never bare `rc`, which can match Plan 9 shell on Unix hosts).
    if let Some(exe) = find_on_path("rc.exe") {
        return Some((exe, Vec::new()));
    }

    let sdk_arch = match target_arch {
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "x64",
    };

    let mut kit_roots = Vec::new();
    if let Ok(sdk_dir) = env::var("WindowsSdkDir") {
        if !sdk_dir.trim().is_empty() {
            kit_roots.push(PathBuf::from(sdk_dir));
        }
    }
    for var in ["ProgramFiles(x86)", "ProgramFiles"] {
        if let Ok(pf) = env::var(var) {
            if !pf.trim().is_empty() {
                kit_roots.push(PathBuf::from(pf).join("Windows Kits").join("10"));
            }
        }
    }
    kit_roots.push(PathBuf::from(r"C:\Program Files (x86)\Windows Kits\10"));

    for root in kit_roots {
        let bin_dir = root.join("bin");
        let Ok(entries) = fs::read_dir(&bin_dir) else {
            continue;
        };
        let mut versions: Vec<String> = entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_owned();
                (e.path().is_dir() && name.starts_with("10.")).then_some(name)
            })
            .collect();
        versions.sort_by_key(|v| std::cmp::Reverse(parse_sdk_version(v)));

        for ver in versions {
            let rc_candidate = bin_dir.join(&ver).join(sdk_arch).join("rc.exe");
            if rc_candidate.is_file() {
                let mut includes = Vec::new();
                let inc_base = root.join("Include").join(&ver);
                for sub in ["um", "shared"] {
                    let inc_dir = inc_base.join(sub);
                    if inc_dir.is_dir() {
                        includes.push(inc_dir);
                    }
                }
                return Some((rc_candidate, includes));
            }
        }
    }

    None
}

fn find_windres(target_arch: &str) -> Option<PathBuf> {
    let mut candidates: Vec<&str> = match target_arch {
        "x86" => vec!["i686-w64-mingw32-windres", "i686-w64-mingw32-windres.exe"],
        "aarch64" => vec![
            "aarch64-w64-mingw32-windres",
            "aarch64-w64-mingw32-windres.exe",
        ],
        _ => vec![
            "x86_64-w64-mingw32-windres",
            "x86_64-w64-mingw32-windres.exe",
        ],
    };
    candidates.extend(["llvm-windres.exe", "llvm-windres", "windres.exe", "windres"]);

    for name in candidates {
        if let Some(exe) = find_on_path(name) {
            return Some(exe);
        }
    }
    None
}

fn parse_sdk_version(ver: &str) -> (u32, u32, u32, u32) {
    let mut parts = ver.split('.').map(|s| s.parse::<u32>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn find_on_path(exe_name: &str) -> Option<PathBuf> {
    let path_var = env::var_os("PATH")?;
    for dir in env::split_paths(&path_var) {
        let candidate = dir.join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}
