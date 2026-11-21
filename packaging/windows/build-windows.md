# Building & Packaging `coree` for Windows

`coree` is written in 100% pure Rust (`#![forbid(unsafe_code)]` across all 7 crates) with bundled SQLite (`rusqlite`) and native Windows Common Item Dialogs (`rfd`), so Windows builds require **no external DLLs** beyond standard Windows system libraries (`opengl32.dll`, `user32.dll`, `shell32.dll`, `ole32.dll`).

---

## Option A: Native Build on Windows (`x86_64-pc-windows-msvc`) — Recommended

### Prerequisites
1. Install **Visual Studio 2022 Build Tools** with the **Desktop development with C++** workload (MSVC linker + Windows SDK).
2. Install Rust via [rustup](https://rustup.rs) (default target `x86_64-pc-windows-msvc`).

### Build & Verify
```powershell
git clone https://github.com/ahrrfy/coree.git
cd coree

# Run full workspace test suite and lint checks
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets -- -D warnings

# Build optimized Windows desktop binary
cargo build --release --target x86_64-pc-windows-msvc

# Smoke-check the compiled executable
.\target\x86_64-pc-windows-msvc\release\coree.exe --diagnostics
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
# Install MinGW-w64 cross-compiler (Debian/Ubuntu)
sudo apt-get install -y gcc-mingw-w64-x86-64

# Add the Rust GNU Windows target
rustup target add x86_64-pc-windows-gnu

# Build coree.exe
cargo build --release --target x86_64-pc-windows-gnu
```
The standalone executable is emitted at `target/x86_64-pc-windows-gnu/release/coree.exe`.

---

## Packaging a Windows MSI Installer with WiX Toolset v4

The manifest at [`packaging/windows/wix/main.wxs`](wix/main.wxs) packages `coree.exe`, creates a Start Menu shortcut, adds the install directory to `PATH`, and registers file associations for `.coree`, `.cdr`, `.cdt`, `.cmx`, and `.cpt`.

### Prerequisites
Install the .NET SDK (8.0+) and the WiX v4 CLI tool:
```powershell
dotnet tool install --global wix
```

### Build the `.msi`
```powershell
# 1. Build the release binary (MSVC default target puts binary at target\release\coree.exe)
cargo build --release

# 2. Compile the WiX v4 MSI installer
wix build packaging\windows\wix\main.wxs `
    -arch x64 `
    -d BinaryPath="target\release\coree.exe" `
    -d Version="0.1.0" `
    -o target\release\coree-0.1.0-x64.msi
```

### Silent Enterprise Install / Uninstall Verification
```powershell
# Silent install
msiexec /i target\release\coree-0.1.0-x64.msi /qn /l*v install.log

# Verify CLI diagnostics
& "$env:ProgramFiles\coree\coree.exe" --diagnostics

# Silent uninstall
msiexec /x target\release\coree-0.1.0-x64.msi /qn
```
