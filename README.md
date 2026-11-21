# coree

**Clean-Room Open-Source Vector Graphics, Multipage Layout & Print Production Studio — Linux-First, Cross-Platform, 100% Offline**

**كورِي (`coree`) — حزمة رسوميات متجهية وتخطيط صفحات وإنتاج طباعي مفتوحة المصدر ومبنية بالكامل من الصفر — لينكس أولاً، تعمل محلياً دون اتصال بالإنترنت**

[![License: MPL-2.0](https://img.shields.io/badge/License-MPL--2.0-3ea6ff.svg)](LICENSE)
[![Rust: #![forbid(unsafe_code)]](https://img.shields.io/badge/Rust-forbid(unsafe__code)-f6ad3c.svg)](SECURITY.md)

---

## English Overview

`coree` is a clean-room, open-source desktop vector illustration, multipage layout, integrated raster studio, vector tracing, and commercial print-production suite written in pure Rust under the **MPL-2.0** license. Built for print shops, sign makers, packaging/laser/vinyl workshops, Arabic/MENA & international designers on **Linux** and **Windows**, `coree` runs **100% offline** with **zero subscriptions, zero accounts, zero cloud lock-in, and zero telemetry**.

- **Repository:** [https://github.com/ahrrfy/coree](https://github.com/ahrrfy/coree)
- **License:** Mozilla Public License 2.0 ([LICENSE](LICENSE))
- **Verified Quality:** **202/202** automated tests passing across all 7 crates (`0` failed, `0` ignored), `cargo clippy --workspace --all-targets -- -D warnings` clean, `#![forbid(unsafe_code)]` across all 7 crates, **35/35** WCAG 2.2 AA/AAA contrast pairs passing, and **100.0%** deterministic pixel-identical rendering verified via `imgdiff.py`.

---

### Architecture (7-Crate Pure-Rust Workspace)

| Crate | Purpose | Automated Tests |
| --- | --- | ---: |
| [`crates/coree-core`](crates/coree-core) | Document scene graph, 8 units (`mm`..`CdrTenthMicron`), 6 color models, 8 parametric shapes, 9 `i_overlay` Boolean ops, all 11 Lens definitions, 10 live vector effects, 19 raster filters, Arabic RTL/Kashida model, `VectorTable`, dimensions/connectors, R-Tree `SpatialIndex`, 28 serializable `CommandOp`s, transactional undo/redo | 44 |
| [`crates/coree-cdr`](crates/coree-cdr) | Clean-room `.cdr` (RIFF v4–v13, ZIP/OPC X4–X5, X6–2025), `.cdt`, `.cmx` (16/32-bit RIFF & RIFX), and `.cpt` (multi-layer v7–v9 & RIFF) codecs, byte-exact **Unknown-Chunk Passthrough**, **CP1256** legacy Arabic decoder, **CDR Structural Inspector**, and Zip-Bomb/Zip-Slip/depth hardening | 31 |
| [`crates/coree-io`](crates/coree-io) | Native `.coree` archive, SVG/SVGZ import & export, native DeviceCMYK/RGB PDF 1.7 + print marks export, EPS export, DXF (R12/R2000+) & HPGL `.plt` plotter export, PNG/JPEG/WebP/TIFF/BMP + PNG `pHYs` DPI, palette `.gpl`/`.ase`/`.pal` I/O, and atomic temp-file + `fsync` saves | 31 |
| [`crates/coree-storage`](crates/coree-storage) | Bundled SQLite 3 (WAL mode, 10-table schema), SHA-256 autosave snapshots, deterministic `command_journal` crash recovery, recents/palettes/templates/layouts/preferences, local SFNT font scanner (Arabic/Kashida detection), and non-blocking Rayon background job pool | 18 |
| [`crates/coree-render`](crates/coree-render) | Backend-agnostic `CanvasBackend` (`tiny-skia`), Rayon 256×256 `DirtyTileCache`, `cosmic-text` Arabic RTL/BiDi/Kashida shaping & real glyph curve extraction, all 11 live optical Lenses (with Frozen/Viewpoint/RemoveFace), all 10 vector effects, integrated Raster Studio (brushes/masks/19 filters/cutout/resample), Quick/Outline/Centerline Vector Trace, Smart Fill, and hit-testing/snapping | 19 |
| [`crates/coree-prod`](crates/coree-prod) | CMYK + Spot color separations & overprint simulation, N-Up / Saddle-Stitch Booklet / Tiling imposition, vector Print Marks, Document Preflight, Collect for Output, one-click `CutContour`, 4 Parametric Packaging Dielines (`TuckEndBox`, `Envelope`, `ShoppingBag`, `StickerSheet`), Path Length & Area Meter, vector QR & 1D Barcodes (`EAN-13`/`EAN-8`/`UPC-A`/`Code128`/`Code39`), bilingual CSV Print Merge, CIE76 Find & Replace, and sandboxed Rhai macros | 36 |
| [`crates/coree-app`](crates/coree-app) | Native cross-platform desktop UI (`eframe 0.37` / `egui 0.37` / `egui_dock 0.22`, pure-Rust `rfd` XDG Portal dialogs), 15 Toolbox Flyout groups (62 tools), dockers, Command Palette (`Ctrl+K`), ClassicStudio & StandardVector shortcut profiles, WCAG AA Dark/Light/HighContrast themes, and production headless CLI (`coree` binary) | 23 |
| **Total** | **`#![forbid(unsafe_code)]` across all 7 crates** | **202** |

---

### Installation & Building

#### Prerequisites
- Rust stable toolchain (`rustc`, `cargo`, `clippy`, `rustfmt`) via [rustup.rs](https://rustup.rs)

#### Linux (Zero-Sudo Local Install — Ubuntu / Debian / Fedora / Arch)
`coree` dynamically loads Wayland/X11/OpenGL runtime libraries, bundles SQLite, and uses pure-Rust XDG Desktop Portals (`rfd` `xdg-portal`) — **no `-dev` C header packages (`libgtk-3-dev`, `libx11-dev`, `libfontconfig-dev`) are required**; only `gcc` (`build-essential`) and standard desktop runtime libraries are needed.

```bash
git clone https://github.com/ahrrfy/coree.git
cd coree

# Builds release binary and installs ~/.local/bin/coree, icon, desktop entry & MIME types (no sudo)
./packaging/linux/install-linux.sh

# Verify local installation
~/.local/bin/coree --diagnostics
```

To stage a portable Linux AppDir (and build an `.AppImage` when `appimagetool` is present):

```bash
./packaging/linux/build-appimage.sh
```

#### Windows (`x86_64-pc-windows-msvc` / `x86_64-pc-windows-gnu` + WiX v4 MSI)
See the dedicated Windows build and MSI installer guide at [packaging/windows/build-windows.md](packaging/windows/build-windows.md) and WiX v4 manifest at [packaging/windows/wix/main.wxs](packaging/windows/wix/main.wxs).

```powershell
cargo build --release
.\target\release\coree.exe --diagnostics
```

---

### Headless CLI Reference

Launching `coree` with no subcommand opens the native desktop studio GUI (optionally passing files to open in tabs: `coree artwork.cdr layout.coree`). All production pipelines are also exposed directly as fast offline CLI subcommands:

```bash
# Print build version, license, supported formats, and indexed local font count
coree --diagnostics

# Generate a rich showcase sample (.coree, cdr-riff, cdr-x4, cdr-x6, cdt, cmx, svg)
coree generate-sample demo.cdr --format cdr-x6

# Inspect .cdr/.cdt/.cmx/.cpt/.coree/.svg structure, chunk tree & preserved unknown chunks
coree inspect demo.cdr --json

# Universal offline converter (.coree, .cdr, .cdt, .cmx, .svg, .pdf, .eps, .dxf, .plt, .png, .jpg, .webp, .tiff, .bmp)
coree convert demo.cdr press_ready.pdf --cmyk --crop-marks
coree convert demo.cdr cut_job.dxf

# Deterministic headless renderer (single page or side-by-side Multipage View)
coree render demo.coree page0.png --page 0 --dpi 150 --view enhanced
coree render brochure.coree spread.png --multipage --dpi 120

# Print-production Preflight audit (text or JSON)
coree preflight press_ready.coree --min-dpi 300 --json

# Variable-data CSV Print Merge (UTF-8 Arabic + English)
coree print-merge badge_template.coree guests.csv merged_badges.coree

# Collect for Output (bundle document, images, font manifest & preflight report)
coree collect press_ready.coree ./print_handoff_bundle/

# Vector Trace raster artwork into editable curves (quick, outline, centerline)
coree trace logo.png logo_vector.svg --mode outline --colors 6

# Execute sandboxed Rhai automation macro (built-in ID or custom .rhai file)
coree run-macro artwork.coree recolor-cmyk artwork_cmyk.coree
```

---

### Running Verification & Quality Checks

```bash
cargo test --workspace --all-targets
cargo test --workspace --doc
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

---

## نظرة عامة بالعربية

**`coree`** حزمة رسوميات متجهية وتخطيط صفحات متعددة وإنتاج طباعي مفتوحة المصدر ومبنية بالكامل من الصفر (Clean-Room) بلغة **Rust** وتحت رخصة **MPL-2.0**. صُمّمت لمطابع الأوفست والديجيتال ومشاغل اللافتات والقص بالليزر والفينيل ومصممي العبوات والخط العربي على **لينكس** و**ويندوز**، وتعمل **محلياً بالكامل بنسبة 100%** دون اشتراكات أو حسابات أو سحابة إجبارية أو تتبع.

### أبرز المزايا
- **محرك متجهات وصفحات متعددة (`coree-core`)**: 8 وحدات قياس، 6 نماذج لونية (CMYK/RGB/Lab/HSL/Gray/Spot)، 9 عمليات منطقية (`i_overlay`)، 11 عدسة بصرية حيّة، 10 مؤثرات متجهية، جداول متجهية، خطوط أبعاد وموصلات، وفهرس مكاني `rstar` مع سجل أوامر متسلسل مقاوم للأعطال.
- **قراءة وفحص صيغ التصميم (`coree-cdr`, `coree-io`)**: دعم كامل لملفات `.cdr` (RIFF وZIP X4 وX6–2025) و`.cdt` و`.cmx` و`.cpt` مع **تمرير المقاطع غير المعروفة بايت-ببايت (Unknown-Chunk Passthrough)** وأداة **فاحص CDR** واستعادة النصوص العربية القديمة بترميز **CP1256**، بالإضافة إلى تصدير PDF بنظام CMYK مع علامات القص وSVG وEPS وDXF وHPGL `.plt` والصور النقطية.
- **دعم أصيل للغة العربية (`coree-render`)**: اتجاه RTL وتشكيل حروف أصيل و**تطويل الكشيدة** والكتابة على المسار وتحويل النص إلى منحنيات حقيقية، مع استوديو نقطي مدمج وتتبع متجهي (Quick / Outline / Centerline).
- **أدوات ما قبل الطباعة والتغليف (`coree-prod`)**: فصل ألوان CMYK وSpot، المونتاج الطباعي (N-Up / كتيّب / تبليط)، الفحص المسبق Preflight، جمع ملفات الطباعة، مسار قص **CutContour** بضغطة واحدة، 4 قوالب تغليف بارامترية (**Dielines**)، مقياس طول ومساحة القص (**Path Meter**)، باركود وQR متجهية، دمج الطباعة من CSV عربي/إنجليزي، وماكرو Rhai معزول أمنياً.
- **أمان واستقرار (`coree-storage`)**: منع كامل للشيفرة غير الآمنة `#![forbid(unsafe_code)]` في جميع الحزم السبع، حماية ضد قنابل الضغط (Zip-Bomb) واختراق المسارات (Zip-Slip)، حفظ ذري، واستعادة فورية من الأعطال عبر SQLite WAL.

### التثبيت المحلي على لينكس (بدون `sudo`)

```bash
git clone https://github.com/ahrrfy/coree.git
cd coree
./packaging/linux/install-linux.sh
~/.local/bin/coree --diagnostics
```

---

## License & Trademark Disclaimer

Licensed under the **Mozilla Public License Version 2.0 (MPL-2.0)**. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

> **Trademark Disclaimer:** Corel®, CorelDRAW®, PHOTO-PAINT®, and PowerTRACE® are registered trademarks of Corel Corporation / Alludo. `coree` is an independent clean-room open-source project and is **not** affiliated with, endorsed by, or sponsored by Corel Corporation or Alludo.
>
> **إخلاء مسؤولية العلامات التجارية:** Corel® وCorelDRAW® علامتان تجاريتان مسجلتان لشركة Corel Corporation / Alludo. مشروع `coree` هو مشروع مستقل مفتوح المصدر ومبني من الصفر ولا يرتبط بأي شكل بشركة Corel أو Alludo.
