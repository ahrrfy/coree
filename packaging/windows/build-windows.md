# Building & Packaging `coree` for Windows

`coree` is written in 100% pure Rust (`#![forbid(unsafe_code)]` across all 7 crates, dual-licensed `MIT OR Apache-2.0`) with bundled SQLite (`rusqlite`) and native Windows Common Item Dialogs (`rfd`), so Windows builds require **no external DLLs** beyond standard Windows system libraries (`opengl32.dll`, `user32.dll`, `shell32.dll`, `ole32.dll`).

## Embedded Windows Resources (`coree.ico` + `coree.exe.manifest` + `coree.rc`)

When compiling for Windows (`x86_64-pc-windows-msvc` or `x86_64-pc-windows-gnu`), `crates/coree-app/build.rs` automatically compiles [`packaging/windows/coree.rc`](coree.rc) into `coree.exe`:

- **Multi-Size Icon ([`coree.ico`](coree.ico)):** `16×16`, `32×32`, `48×48`, and `256×256` PNG-compressed RGBA frames for crisp rendering across Explorer, Taskbar, Start Menu, and Alt+Tab.
- **Application Manifest ([`coree.exe.manifest`](coree.exe.manifest)):**
  - `PerMonitorV2` HiDPI awareness for sharp canvas and UI rendering on 4K/mixed-DPI displays.
  - `<activeCodePage>UTF-8</activeCodePage>` so Windows CLI & file dialogs handle Arabic and Unicode filenames (such as `درع الشكر والتقدير.cdr`) flawlessly without legacy ANSI codepage corruption.
  - `longPathAware` enabled.
- **Version Metadata (`VS_VERSION_INFO`):** Product name `coree`, version `0.3.0.0`, and `MIT OR Apache-2.0` copyright metadata.

---

## Option A: Native Build on Windows (`x86_64-pc-windows-msvc`) — Recommended

### Prerequisites
1. Install **Visual Studio 2022 Build Tools** with the **Desktop development with C++** workload (MSVC linker + Windows SDK including `rc.exe`).
2. Install Rust via [rustup](https://rustup.rs) (default target `x86_64-pc-windows-msvc`).

### Build & Atomic Verification
```powershell
git clone https://github.com/ahrrfy/coree.git
cd coree

# Run full workspace test suite and lint checks
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

# Build optimized Windows GUI + Console CLI companion binaries (embeds coree.ico & UTF-8/DPI manifest)
cargo build --release --package coree-app --bins --target x86_64-pc-windows-msvc
Copy-Item ".\target\x86_64-pc-windows-msvc\release\coree-cli.exe" ".\target\x86_64-pc-windows-msvc\release\coree.com" -Force

# Atomic CLI smoke checks via coree.com (synchronous console stdout + exit codes)
$exe = ".\target\x86_64-pc-windows-msvc\release\coree.com"
& $exe --diagnostics
& $exe generate-sample sample.cdr --format cdr-x6
& $exe inspect sample.cdr --json
& $exe convert sample.cdr press.pdf --cmyk --crop-marks
& $exe convert sample.cdr cut.dxf
& $exe thumbnail sample.cdr thumb.png --size 256
& $exe laser sample.cdr job.gcode --format gcode --dialect grbl
```

---

## Option B: MinGW-w64 Build (`x86_64-pc-windows-gnu`)

Works either natively inside **MSYS2 (MinGW64 shell)** on Windows or as a cross-compilation target from Linux.

### On Windows (MSYS2 MinGW64)
```bash
pacman -S --needed mingw-w64-x86_64-toolchain
rustup target add x86_64-pc-windows-gnu
cargo build --release --target x86_64-pc-windows-gnu
./target/x86_64-pc-windows-gnu/release/coree.exe --diagnostics
```

### Cross-Compiling from Linux (Ubuntu / Debian / Fedora)
```bash
# Install MinGW-w64 cross-compiler (Debian/Ubuntu; includes x86_64-w64-mingw32-windres)
sudo apt-get install -y gcc-mingw-w64-x86-64

# Add the Rust GNU Windows target
rustup target add x86_64-pc-windows-gnu

# Build coree.exe
cargo build --release --target x86_64-pc-windows-gnu
```
The standalone executable is emitted at `target/x86_64-pc-windows-gnu/release/coree.exe`.

---

## Packaging a Windows MSI Installer with WiX Toolset v4

The manifest at [`packaging/windows/wix/main.wxs`](wix/main.wxs) packages `coree.exe`, `coree.com` (Console CLI companion), `coree.ico`, `LICENSE-MIT`, `LICENSE-APACHE`, and `NOTICE`, creates Start Menu and Desktop shortcuts, adds the install directory to system `PATH`, and registers `coree.Document` (`DefaultIcon` + `shell\open\command` + `.cdr` `Content Type`) plus `OpenWithProgids` for `.coree`, `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.svg`, `.pdf`, `.eps`, `.dxf`, `.plt`, and `.psd`.

### Prerequisites
Install the .NET SDK (8.0+) and the WiX v4 CLI tool:
```powershell
dotnet tool install --global wix
```

### Build the `.msi`
```powershell
# 1. Build both GUI (coree.exe) and Console CLI (coree-cli.exe -> coree.com) release binaries
cargo build --release --package coree-app --bins
Copy-Item "target\release\coree-cli.exe" "target\release\coree.com" -Force

# 2. Compile the WiX v4 MSI installer
wix build packaging\windows\wix\main.wxs `
    -arch x64 `
    -d BinaryPath="target\release\coree.exe" `
    -d ComPath="target\release\coree.com" `
    -d IconPath="packaging\windows\coree.ico" `
    -d RepoRoot="." `
    -d Version="0.3.0" `
    -o dist\coree-v0.3.0-x86_64-windows-setup.msi
```

### Portable `.zip` Package (Zero Admin)
Users who prefer a portable install without Administrator privileges can unpack `coree-v0.3.0-x86_64-windows-portable.zip` (containing `coree.exe`, `coree.com`, `coree.ico`, [`register-file-associations.cmd`](register-file-associations.cmd), `LICENSE-MIT`, `LICENSE-APACHE`, `NOTICE`, and `README.md`) and double-click `register-file-associations.cmd` to register per-user (`HKCU`) file associations and icons.

### Silent Enterprise Install / Uninstall Verification
```powershell
# Silent install
msiexec /i dist\coree-v0.3.0-x86_64-windows-setup.msi /qn /l*v install.log

# Verify CLI diagnostics via coree.com (synchronous stdout + exit code)
& "$env:ProgramFiles\coree\coree.com" --diagnostics

# Silent uninstall
msiexec /x dist\coree-v0.3.0-x86_64-windows-setup.msi /qn
```
