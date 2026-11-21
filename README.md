# coree

**Clean-Room Open-Source Vector Graphics, Multipage Layout, Laser Production & Print Studio — Linux-First, Cross-Platform, 100% Offline**

**كورِي (`coree`) — حزمة رسوميات متجهية وتخطيط صفحات وإنتاج طباعي وقص بالليزر مفتوحة المصدر ومبنية بالكامل من الصفر — لينكس أولاً، تعمل محلياً دون اتصال بالإنترنت**

[![CI](https://github.com/ahrrfy/coree/actions/workflows/ci.yml/badge.svg)](https://github.com/ahrrfy/coree/actions/workflows/ci.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/License-MIT%20OR%20Apache--2.0-3ea6ff.svg)](LICENSE-MIT)
[![Rust: #![forbid(unsafe_code)]](https://img.shields.io/badge/Rust-forbid(unsafe__code)-f6ad3c.svg)](SECURITY.md)

---

## Visual Showcase / معرض المخرجات الفعلية

All images below are rendered directly by `coree render` (verified 100.0% pixel-deterministic):

| Live Optical Lenses & Mesh Fill | Arabic RTL/Kashida + Parametric Dieline + QR/EAN-13 | Multipage View |
| :---: | :---: | :---: |
| ![Lenses and Vectors](docs/images/showcase_lenses_vectors.png) | ![Arabic Packaging Dieline](docs/images/showcase_arabic_packaging.png) | ![Multipage View](docs/images/showcase_multipage.png) |

---

## English Overview

`coree` is a clean-room, open-source desktop vector illustration, multipage layout, integrated raster studio, vector tracing, laser/engraving plugin, and commercial print-production suite written in pure Rust and dual-licensed under **`MIT OR Apache-2.0`**. Built for print shops, laser cutting & fiber engraving workshops, sign makers, packaging/vinyl studios, Arabic/MENA & international designers on **Linux** and **Windows**, `coree` runs **100% offline** with **zero subscriptions, zero accounts, zero cloud lock-in, and zero telemetry**.

- **Repository:** [https://github.com/ahrrfy/coree](https://github.com/ahrrfy/coree)
- **License:** Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE) (Rust ecosystem gold standard: MIT's maximum global adoption + Apache-2.0's explicit patent grant & trademark non-grant; see [NOTICE](NOTICE))
- **Feature Parity:** **100.0 / 100** (`68/68` must-have capabilities complete across all 13 feature areas — see [replica/parity.md](replica/parity.md))
- **Verified Quality:** `#![forbid(unsafe_code)]` across all 7 crates, **298/298** automated unit/integration/E2E/adversarial tests passing (`0` open S1/S2 bugs across 88 verified audit fixes), `cargo clippy --workspace --all-targets -- -D warnings` clean, **35/35** WCAG 2.2 AA/AAA contrast pairs passing, and **100.0%** deterministic pixel-identical rendering verified via `imgdiff.py`.

---

### What's New in `v0.2.0`

- **Authentic ZCF `.cdr` X6–2025 (v16–26) Codec:** Full indirect pointer dereferencing (`content/dataFileList.dat`), `styles/document.cdss`, `color/docPalette.xml`, UTF-16LE & CP1256 Arabic text, embedded PNG thumbnail extraction (`extract_cdr_preview_png`), and transparent `.cdr.zip` / `.cdt.zip` magic-byte opening (verified on real v23 CorelDRAW 2021 artwork with 4,746 cubic Bézier handles!).
- **Pro Industry Importers & Exporters:** Native pure-Rust **PDF 1.0–2.0 & PDF-compatible `.ai`** importer (verified on real 44 MB 60-page PDFs), **EPS / PostScript** importer (DOS binary & PS), **AutoCAD DXF (R12/R2000+) & HPGL `.plt`** importers, and streaming multi-layer **Photoshop `.psd`/`.psb`** importer (verified on real 295 MB 9-layer PSDs).
- **Pure-Rust ICC v2/v4 Color Management & Live Soft-Proofing:** Real ICC v2/v4 profile parsing (verified on 52/52 real system ICC profiles), built-in `sRGB`, `Display P3`, `ISO Coated v2 CMYK`, and `US Web Coated SWOP CMYK` profiles, live CMYK soft-proofing, and out-of-gamut warning overlay.
- **Laser Cutting, Engraving & Plotter Plugins:** Dedicated toolpath planner (duplicate-line removal, curve joining, kerf compensation, rotated/cross hatch fill, inner-loops-first ordering, travel optimization, weeding box, GRBL/Marlin/Ruida G-Code & DXF R12/PLT/SVG/EPS export) + extensible `plugin.json`/`plugin.toml`/Rhai architecture with **7 built-in bridge presets**: **LightBurn**, **RDWorks / Ruida**, **EZCad Fiber**, **LaserDRW / M2 Nano**, **LaserGRBL**, **InkCut / VisiCut**, and **Vinyl Plotter (HPGL)**.
- **62 Real Interactive Tools & Studio Workspaces:** Every single tool across all 15 Toolbox flyout groups live and verified (including **Healing Brush**, **Dodge/Burn**, **Raster Flood Fill**, **10 PostScript Procedural Fills**, **Autofit Page to Content**, and **Glyphs Docker**), plus 6 Workspace Presets (`DefaultClassic`, `Lite`, `Illustration`, `PageLayout`, `LaserSignStudio`, `TouchCompact`), right-click context menu, and muscle-memory shortcuts (`P`, `L/R/T/B/C/E`, `F2`/`F3`/`F4`/`F9`, `Space`, `Ctrl+K`/`L`/`Q`/`D`/`R`/`J`).
- **Native OS Integration (Linux & Windows):** Fixes Linux `.cdr` ZIP-icon misclassification via canonical `application/vnd.corel-draw` `<sub-class-of type="application/zip"/>` + GNOME `coree.thumbnailer`, adds **Flatpak**, native **`.deb`**, and **`.AppImage`** packages on Linux, and ships **Windows `.msi`** + **Portable `.zip`** (with embedded `coree.ico`, `PerMonitorV2` DPI + `UTF-8` Arabic filename manifest, and 1-click `register-file-associations.cmd`).

---

### Architecture (7-Crate Pure-Rust Workspace)

| Crate | Purpose |
| --- | --- |
| [`crates/coree-core`](crates/coree-core) | Scene graph, 8 physical units (`mm`..`CdrTenthMicron`), 6 color models, pure-Rust **ICC v2/v4** engine (`sRGB`/`DisplayP3`/`ISO Coated v2`/`SWOP`), 10 analytic **PostScript fills**, 8 parametric shapes, 9 `i_overlay` Boolean ops, all 11 Lenses, 10 live vector effects, 19 raster filters, Arabic RTL/Kashida model, `VectorTable`, Dimensions/Connectors, **Autofit Page**, R-Tree `SpatialIndex`, 28 serializable `CommandOp`s, transactional undo/redo |
| [`crates/coree-cdr`](crates/coree-cdr) | Clean-room `.cdr` (RIFF v4–v13, ZIP/OPC X4–X5, **ZCF X6–2025 v16–26** indirect pointer dereferencing, `document.cdss`, `docPalette.xml`), `.cdt`, `.cmx` (16/32-bit RIFF & RIFX), and `.cpt` codecs, byte-exact **Unknown-Chunk Passthrough**, **CP1256** legacy Arabic decoder, **Embedded Thumbnail Extractor**, **CDR Inspector**, and Zip-Bomb/Zip-Slip/amplification hardening |
| [`crates/coree-io`](crates/coree-io) | Native `.coree` archive, **PDF 1.0–2.0 / `.ai`** import + DeviceCMYK/RGB PDF 1.7 print-marks export, **EPS** import & export, **DXF (R12/R2000+)** & **HPGL `.plt`** import & export, streaming multi-layer **PSD/PSB** import, SVG/SVGZ import & export, PNG/JPEG/WebP/TIFF/BMP + PNG `pHYs` DPI, palette `.gpl`/`.ase`/`.pal` I/O, atomic `fsync` saves |
| [`crates/coree-storage`](crates/coree-storage) | Bundled SQLite 3 (WAL mode, 10-table schema), SHA-256 autosave snapshots, deterministic `command_journal` crash recovery, recents/palettes/templates/layouts/preferences, local SFNT font scanner (Arabic/Kashida detection), non-blocking Rayon background job pool |
| [`crates/coree-render`](crates/coree-render) | Backend-agnostic `CanvasBackend` (`tiny-skia`), Rayon 256×256 `DirtyTileCache`, live **ICC Soft-Proofing & Gamut Warning**, `cosmic-text` Arabic RTL/BiDi/Kashida shaping + **Glyphs Catalog API** + real glyph curves, all 11 live optical Lenses, all 10 vector effects, 10 SDF PostScript fills, Raster Studio (Paint/Eraser/Clone/Smudge/**Healing**/**Dodge/Burn**/**Flood Fill**/masks/19 filters/cutout/resample), 3-mode Vector Trace, Smart Fill, hit-testing & snapping |
| [`crates/coree-prod`](crates/coree-prod) | **Laser Cutting, Engraving & Plotter Engine** (dedupe, join, kerf, hatch, inner-first ordering, travel optimization, weeding box, GRBL/Marlin/Ruida G-Code & DXF/PLT/SVG/EPS) + **7 Built-in Laser/Plotter Plugins** (`LightBurn`, `RDWorks`, `EZCad`, `LaserDRW`, `LaserGRBL`, `InkCut/VisiCut`, `HPGL Plotter`) + custom JSON/TOML/Rhai plugins, CMYK/Spot separations & overprint, N-Up/Booklet/Tiling imposition, Print Marks, Preflight, Collect for Output, `CutContour`, 4 Parametric Dielines, Path Meter, QR & 1D Barcodes, bilingual CSV Print Merge, CIE76 Find & Replace, sandboxed Rhai macros |
| [`crates/coree-app`](crates/coree-app) | Native cross-platform desktop UI (`eframe 0.37` / `egui 0.37` / `egui_dock 0.22`, pure-Rust `rfd` XDG Portal), **62 real interactive tools** in 15 Toolbox flyout groups, 6 Studio Workspace Presets, Glyphs Docker, Laser & Plugins Docker, right-click context menu, full muscle-memory shortcuts, WCAG AA Dark/Light/HighContrast themes, native window icon & Win32 `build.rs` resources, and 11-subcommand headless CLI (`coree`) |

---

### Installation & Packaging

#### Option 1 — Linux: Prebuilt Packages (GitHub Releases)
Download the latest release assets from [GitHub Releases](https://github.com/ahrrfy/coree/releases):

- **Ubuntu / Debian (`.deb` — Recommended for Debian/Ubuntu/Mint/Pop!_OS):**
  ```bash
  sudo apt install ./coree_0.2.0_amd64.deb
  ```
  Installs `/usr/bin/coree`, desktop entry, canonical `application/vnd.corel-draw` MIME rules (eliminates ZIP icons on `.cdr` files!), SVG app & MIME icons, GNOME/freedesktop `coree.thumbnailer`, and AppStream metainfo.
- **Universal Portable `.AppImage` (Any Linux distro, zero install):**
  ```bash
  chmod +x coree-v0.2.0-x86_64.AppImage
  ./coree-v0.2.0-x86_64.AppImage
  ```
- **Offline-Sandboxed Flatpak:**
  Build locally from [packaging/linux/flatpak/io.github.ahrrfy.coree.yml](packaging/linux/flatpak/io.github.ahrrfy.coree.yml) (strictly sandboxed with Wayland/X11/DRI and **no network access**):
  ```bash
  flatpak-builder --user --install --force-clean build-dir packaging/linux/flatpak/io.github.ahrrfy.coree.yml
  flatpak run io.github.ahrrfy.coree
  ```

#### Option 2 — Linux: Build from Source & Zero-Sudo Local Install
`coree` dynamically loads Wayland/X11/OpenGL runtime libraries, bundles SQLite, and uses pure-Rust XDG Desktop Portals (`rfd`) — **no `-dev` C header packages (`libgtk-3-dev`, `libx11-dev`, `libfontconfig-dev`) are required**; only Rust stable ([rustup.rs](https://rustup.rs)) and `gcc` (`build-essential`).

```bash
git clone https://github.com/ahrrfy/coree.git
cd coree

# Builds release binary & installs ~/.local/bin/coree, app + MIME icons,
# desktop entry, canonical .cdr MIME rules, GNOME thumbnailer & xdg-mime defaults (no sudo)
./packaging/linux/install-linux.sh

# Verify local installation
~/.local/bin/coree --diagnostics
```

Build native `.deb` or `.AppImage` locally without `sudo`:
```bash
./packaging/linux/build-deb.sh
./packaging/linux/build-appimage.sh
```

#### Option 3 — Windows (`x86_64`): MSI Installer or Portable ZIP
Download from [GitHub Releases](https://github.com/ahrrfy/coree/releases):
- **Windows Installer (`.msi`):** Double-click `coree-v0.2.0-x86_64-windows-setup.msi` — installs `coree.exe` (with embedded `coree.ico`, `PerMonitorV2` HiDPI & `UTF-8` Arabic filename manifest), Start Menu & Desktop shortcuts, `PATH` entry, and full Windows Registry file associations (`coree.Document` + `OpenWithProgids` for `.coree`, `.cdr`, `.cdt`, `.cmx`, `.cpt`, `.svg`, `.pdf`, `.eps`, `.dxf`, `.plt`, `.psd`).
- **Portable `.zip` (Zero Admin):** Extract `coree-v0.2.0-x86_64-windows-portable.zip` anywhere and run `coree.exe`. Optionally double-click `register-file-associations.cmd` to register `.coree`/`.cdr`/`.cdt`/`.cmx`/`.cpt`/`.svg`/`.dxf`/`.plt` in `HKCU` without Administrator privileges!

To build from source on Windows (MSVC, MinGW-w64, or WiX v4 `.msi`), see [packaging/windows/build-windows.md](packaging/windows/build-windows.md).

---

### Headless CLI Reference

Launching `coree` with no subcommand opens the native desktop studio GUI (optionally passing files to open directly as tabs: `coree artwork.cdr layout.coree`). All production pipelines are also available offline via CLI:

```bash
# Print build version, license, supported formats, plugin count, and indexed local fonts
coree --diagnostics

# Generate a rich showcase sample (.coree, cdr-riff, cdr-x4, cdr-x6, cdt, cmx, svg)
coree generate-sample demo.cdr --format cdr-x6

# Inspect .cdr/.cdr.zip/.cdt/.cmx/.cpt/.coree/.svg/.pdf/.ai/.eps/.dxf/.plt/.psd structure
coree inspect demo.cdr --json

# Universal offline converter (.coree, .cdr, .cdt, .cmx, .svg, .pdf, .ai, .eps, .dxf, .plt, .psd, .gcode, rasters)
coree convert demo.cdr press_ready.pdf --cmyk --crop-marks
coree convert brochure.pdf editable.coree
coree convert design.psd preview.png --dpi 300

# Deterministic headless renderer (single page or side-by-side Multipage View)
coree render demo.coree page0.png --page 0 --dpi 150 --view enhanced
coree render brochure.coree spread.png --multipage --dpi 120

# Fast OS thumbnail extractor (uses embedded CDR/coree previews or renders Page 0)
coree thumbnail artwork.cdr preview.png --size 256

# Laser Cutting, Engraving & Plotter toolpath export (GRBL/Marlin/Ruida G-Code, DXF R12, PLT, SVG, EPS)
coree laser sign.cdr cut_job.gcode --format gcode --dialect grbl --dedupe 0.05 --join 0.05 --kerf 0.15
coree laser shield.cdr.zip lightburn_ready.dxf --plugin lightburn-bridge --weeding-box

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

**`coree`** حزمة رسوميات متجهية وتخطيط صفحات متعددة واستوديو صور نقطية وقص ونقش بالليزر وإنتاج طباعي مفتوحة المصدر ومبنية بالكامل من الصفر (Clean-Room) بلغة **Rust** وبترخيص مزدوج **`MIT OR Apache-2.0`**. صُمّمت لمطابع الأوفست والديجيتال وورش القص بالليزر والفايبر والراوتر والفينيل ومصممي اللافتات والعبوات والخط العربي على **لينكس** و**ويندوز**، وتعمل **محلياً بالكامل بنسبة 100%** دون اشتراكات أو حسابات أو سحابة إجبارية أو تتبع.

### أبرز مزايا الإصدار `v0.2.0`
- **دعم حقيقي وكامل لملفات `.cdr` الحديثة (ZCF X6–2025) والقديمة (`coree-cdr`):** فكّ مؤشرات ZCF غير المباشرة وملفات التنسيق `document.cdss` ولوحات الألوان `docPalette.xml`، واستعادة النصوص العربية (UTF-16LE وCP1256)، واستخراج الصور المصغّرة المدمجة، والفتح المباشر لملفات `.cdr.zip` و`.cdt` و`.cmx` و`.cpt` مع تمرير المقاطع غير المعروفة بايت-ببايت.
- **استيراد وتصدير شامل لجميع صيغ السوق (`coree-io`):** استيراد حقيقي لملفات **PDF / Adobe Illustrator (`.ai`)** و**EPS** و**AutoCAD DXF** و**HPGL `.plt`** و**Photoshop `.psd`/`.psb` متعددة الطبقات**، إضافة إلى **SVG** وتصدير **PDF بنظام CMYK مع علامات القص** والصور النقطية.
- **إدارة ألوان ICC v2/v4 ومعاينة طباعية حيّة (`coree-core`, `coree-render`):** محرك ICC نقي بلغة Rust مع ملفات `sRGB` و`Display P3` و`ISO Coated v2 CMYK` و`US Web Coated SWOP` ومعاينة فورية للألوان خارج النطاق الطباعي (Gamut Warning).
- **بلاجنات القص بالليزر والنقش والبلوتر (`coree-prod`):** محرك مسارات ليزر احترافي (حذف الخطوط المكررة، وصل المسارات المفتوحة، تعويض سماكة الشعاع Kerf، التهشير Hatch/Cross-Hatch للنقش، قص الثقوب الداخلية أولاً، تقليل حركة الانتقال، مربع التفريغ Weeding Box، وتصدير **G-Code** لـ GRBL/Marlin/Ruida و**DXF R12** و**PLT**) مع **7 إضافات جاهزة**: **LightBurn** و**RDWorks / Ruida** و**EZCad Fiber** و**LaserDRW** و**LaserGRBL** و**InkCut / VisiCut** و**HPGL Vinyl Plotter**، وإمكانية إضافة بلاجنات مخصصة بصيغة JSON/TOML أو سكربتات Rhai.
- **62 أداة تفاعلية حقيقية ومساحات عمل احترافية (`coree-app`):** تشمل فرشاة المعالجة (Healing) والتفتيح/التغميق (Dodge/Burn) والتعبئة النقطية (Flood Fill) و10 تعبئات PostScript إجرائية، وملاءمة الصفحة للمحتوى بضغطة واحدة (Autofit Page)، ولوحة الرموز والخطوط (Glyphs Docker)، و6 مساحات عمل جاهزة واختصارات لوحة المفاتيح الاحترافية.
- **تكامل كامل مع لينكس وويندوز:** إنهاء مشكلة ظهور ملفات `.cdr` كملفات ZIP على لينكس مع معاينة مصغّرة حقيقية في مدير الملفات، وتوفير حزم **Flatpak** و**`.deb`** و**`.AppImage`** على لينكس، ومثبّت **`.msi`** ونسخة **`.zip` محمولة** على ويندوز مع دعم كامل لأسماء الملفات العربية (`UTF-8`).

### التثبيت على لينكس وويندوز

- **لينكس (Ubuntu/Debian `.deb`):** `sudo apt install ./coree_0.2.0_amd64.deb`
- **لينكس (التثبيت المحلي من المصدر بدون `sudo`):**
  ```bash
  git clone https://github.com/ahrrfy/coree.git
  cd coree
  ./packaging/linux/install-linux.sh
  ~/.local/bin/coree --diagnostics
  ```
- **ويندوز:** شغّل المثبّت `coree-v0.2.0-x86_64-windows-setup.msi` أو فك ضغط النسخة المحمولة `coree-v0.2.0-x86_64-windows-portable.zip` (مع سكربت ربط الملفات بضغطة واحدة `register-file-associations.cmd`).

---

## License & Trademark Disclaimer

Dual-licensed under the **MIT License** ([LICENSE-MIT](LICENSE-MIT)) or the **Apache License, Version 2.0** ([LICENSE-APACHE](LICENSE-APACHE)), at your option. See [NOTICE](NOTICE) for full attribution and trademark notices.

> **Trademark Disclaimer:** Corel®, CorelDRAW®, PHOTO-PAINT®, and PowerTRACE® are registered trademarks of Corel Corporation / Alludo. Adobe®, Illustrator®, and Photoshop® are registered trademarks of Adobe Inc. `coree` is an independent clean-room open-source project and is **not** affiliated with, endorsed by, or sponsored by Corel Corporation, Alludo, Adobe Inc., or any external laser/plotter software vendor.
>
> **إخلاء مسؤولية العلامات التجارية:** Corel® وCorelDRAW® علامتان تجاريتان مسجلتان لشركة Corel Corporation / Alludo. مشروع `coree` هو مشروع مستقل مفتوح المصدر ومبني من الصفر ولا يرتبط بأي شكل بشركة Corel أو Alludo.
