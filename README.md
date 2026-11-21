# coree

**Clean-Room Open-Source Vector Graphics & Print Production Suite — Linux-First, Cross-Platform, 100% Offline**

**كورِي (`coree`) — حزمة رسوميات متجهية وإنتاج طباعي مفتوحة المصدر ومبنية من الصفر — لينكس أولاً، تعمل دون اتصال بالإنترنت بالكامل**

---

## English Overview

`coree` is a clean-room, open-source desktop vector graphics, page layout, bitmap studio, and print-production suite written in Rust under the **MPL-2.0** license. Designed for professional print shops, sign makers, packaging designers, and illustrators on **Linux** and **Windows**, `coree` runs **100% locally** with **zero subscriptions, zero accounts, zero cloud lock-in, and zero telemetry**.

### Key Capabilities

- **Vector & Page Layout Engine (`coree-core`)**: Multi-page documents, layers, master pages, Bezier/B-spline/node editing, boolean operations (`i_overlay`), power-clipping, tables, dimension lines, architectural scales, snapping, and crash-safe serializable command journal with Rhai macro recording.
- **Legacy & Modern File Format Interoperability (`coree-cdr`, `coree-io`)**: Clean-room `.cdr`, `.cdt`, `.cmx`, and `.cpt` reading/inspection with **Unknown-Chunk Passthrough** and built-in **CDR Inspector**, legacy **CP1256** Arabic text recovery, plus SVG, PDF, EPS, DXF, PLT (HPGL), and raster (PNG/JPEG/WebP/TIFF/BMP) import/export.
- **Arabic RTL & Typography**: First-class bidirectional text layout, Arabic shaping, **Kashida** justification, and text-on-path powered by `cosmic-text`.
- **High-Performance Rendering (`coree-render`)**: R-Tree spatial indexing (`rstar`), dirty-tile rendering with `tiny-skia`, 11 non-destructive live lenses, vector/bitmap effects, integrated raster studio, and vectorization (trace).
- **Prepress & Print Production (`coree-prod`)**: Imposition, preflight checks, spot/CMYK color workflows, barcode & QR generation, print merge, **CutContour** spot layers, **Parametric Dielines**, and **Path Meter** (perimeter/cut-length calculation).
- **Local Crash-Safe Storage (`coree-storage`)**: Embedded SQLite (`rusqlite` bundled) for autosave snapshots, command journals, custom palettes, templates, workspace layouts, and preferences.
- **Security Hardened**: `#![forbid(unsafe_code)]` across all library crates, Zip-Bomb/Zip-Slip guards, bounded parsing, and atomic file saves.

### Workspace Structure

```text
crates/
├── coree-core     # Document model, geometry, commands, macros (#![forbid(unsafe_code)])
├── coree-cdr      # Clean-room CDR/CDT/CMX/CPT codecs & CDR Inspector (#![forbid(unsafe_code)])
├── coree-io       # SVG/PDF/DXF/PLT/raster I/O & hardened archive handling (#![forbid(unsafe_code)])
├── coree-storage  # Local SQLite persistence, autosave & journals (#![forbid(unsafe_code)])
├── coree-render   # Spatial R-Tree, dirty-tile renderer, lenses & effects (#![forbid(unsafe_code)])
├── coree-prod     # Prepress, imposition, preflight, CutContour, dielines, barcodes (#![forbid(unsafe_code)])
└── coree-app      # Native desktop studio UI (`coree` binary via eframe/egui/egui_dock)
```

### Building from Source

#### Prerequisites
- Rust toolchain (`stable`, with `clippy` and `rustfmt`) via [rustup](https://rustup.rs)

#### Linux (Ubuntu / Debian / Fedora / Arch)
`coree` uses dynamically loaded Wayland/X11/OpenGL runtime libraries, bundled SQLite, and pure-Rust XDG Desktop Portals (`rfd` `xdg-portal`) — **no `-dev` C header packages (`libgtk-3-dev`, `libx11-dev`, etc.) are required**; only a C compiler (`gcc` / `build-essential`) and standard desktop runtime libraries are needed.

```bash
cargo build --release
./target/release/coree
```

#### Windows (MSVC)
Ensure the Visual Studio C++ Build Tools and Rust (`x86_64-pc-windows-msvc`) are installed:

```powershell
cargo build --release
.\target\release\coree.exe
```

#### Running Checks & Tests

```bash
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

---

## نظرة عامة بالعربية

**`coree`** هي حزمة مفتوحة المصدر مبنية بالكامل من الصفر (Clean-Room) بلغة **Rust** وتحت رخصة **MPL-2.0**، مخصصة لتصميم الرسوميات المتجهية وتجهيز المطبوعات واللافتات والعبوات على **لينكس** و**ويندوز**. تعمل الحزمة **محلياً بالكامل (Offline-First)** دون أي اشتراكات أو حسابات أو سحابة إجبارية أو تتبع.

### أبرز المزايا

- **محرك رسوميات متجهية وصفحات متعددة**: تحرير منحنيات بيزييه والعُقد، العمليات المنطقية، الجداول، خطوط الأبعاد، الطبقات، وسجل أوامر قابل للتسلسل ومقاوم للأعطال مع تسجيل الماكرو عبر Rhai.
- **دعم صيغ التصميم والطباعة**: قراءة وفحص ملفات `.cdr` و`.cdt` و`.cmx` و`.cpt` مع **تمرير المقاطع غير المعروفة (Unknown-Chunk Passthrough)** وأداة **فاحص CDR**، واستعادة النصوص العربية القديمة بترميز **CP1256**، بالإضافة إلى SVG وPDF وDXF وPLT والصور النقطية.
- **دعم أصيل للغة العربية**: اتجاه من اليمين إلى اليسار (RTL)، وتشكيل الحروف، والمدّ (**الكشيدة**)، والكتابة على المسار.
- **أدوات ما قبل الطباعة والإنتاج (Prepress)**: المونتاج (Imposition)، الفحص المسبق (Preflight)، الباركود ورموز QR، دمج الطباعة، مسارات القص **CutContour**، قوالب **Dielines** البارامترية، ومقياس طول المسار **Path Meter**.
- **أمان واستقرار**: منع كامل للشيفرة غير الآمنة `#![forbid(unsafe_code)]` في جميع المكتبات الأساسية، وحماية من قنابل الضغط (Zip-Bomb) واختراق المسارات (Zip-Slip)، وحفظ ذري تلقائي عبر SQLite محلي.

### البناء والتشغيل

```bash
# بناء النسخة المحسّنة
cargo build --release

# تشغيل الاختبارات وفحص الجودة
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

---

## License & Trademark Disclaimer

Licensed under the **Mozilla Public License Version 2.0 (MPL-2.0)**. See [LICENSE](LICENSE) and [NOTICE](NOTICE).

> **Trademark Disclaimer:** Corel® and CorelDRAW® are registered trademarks of Corel Corporation / Alludo. `coree` is an independent clean-room open-source project and is **not** affiliated with, endorsed by, or sponsored by Corel Corporation or Alludo.
>
> **إخلاء مسؤولية العلامات التجارية:** Corel® وCorelDRAW® علامتان تجاريتان مسجلتان لشركة Corel Corporation / Alludo. مشروع `coree` هو مشروع مستقل مفتوح المصدر ومبني من الصفر ولا يرتبط بأي شكل بشركة Corel أو Alludo.
