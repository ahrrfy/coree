# Landing Page Copy — `coree`

Grounds every section in the positioning angle from [fixes.md](../fixes.md) and voice guidelines from [brand.md](../brand.md). Design tokens follow [tokens.json](../design/tokens.json) (`#111318` Graphite background, `#191c24` Studio surfaces, `#3ea6ff` Electric Cobalt primary CTA, `#f6ad3c` Studio Amber secondary highlight, `#ff66c4` `CutContour` marker; 35/35 WCAG 2.2 AA/AAA contrast pairs passing).

---

## 1. Hero

- **Eyebrow:** `100% Offline · Native Linux & Windows · MIT OR Apache-2.0`
- **Headline:** **Open `.cdr`, design in CMYK, and cut directly to PDF/X or laser — with zero subscriptions.**
- **Subheadline:** Built in pure Rust for commercial print shops, sign makers, packaging converters, Arabic RTL studios, and laser workshops. Local-first desktop studio with lossless `.cdr` round-tripping, native Spot `CutContour` prepress, and instant crash recovery.
- **Primary CTA:** `Download coree v0.3.1 (Linux & Windows)` -> `https://github.com/ahrrfy/coree/releases`
- **Secondary Link:** `Inspect Source & CLI Docs` -> `https://github.com/ahrrfy/coree`
- **Product Screenshots (100% deterministic `coree render` output):**
  - `docs/images/showcase_lenses_vectors.png` — Live optical lenses (`FishEye`, `Magnify`, `Invert`, `HeatMap`, `Wireframe`), multi-stop gradients, 4×4 Bézier Mesh Fill, Drop Shadow, and stepped Contour.
  - `docs/images/showcase_arabic_packaging.png` — Arabic RTL & Kashida typography, parametric packaging dieline (`CutContour` + `Crease`), vector QR/EAN-13 barcodes, live dimensions, and Path Meter.
  - `docs/images/showcase_multipage.png` — Side-by-side Multipage View with shared `AllPages` Master Layer, vector tables, and CMYK/Spot swatches.

---

## 2. The Problem

1. **Production studios get locked out of their own client files and budgets.** Recurring annual fees, upfront credit-card trials, and mandatory online license activation lock print operators out of urgent deadlines whenever billing terms change or an offline shop PC loses server access.
2. **File corruption and Linux incompatibility force expensive workarounds.** Legacy `.cdr` archives break across versions or turn into garbled non-Latin glyphs, crashes wipe hours of work mid-save, and Linux workshops are pushed onto heavy Windows VMs or editors missing CMYK PDF/X separations and `CutContour` laser workflows.

---

## 3. How It Works

1. **Open any production file directly (`File > Open` / CLI).** Load RIFF v4–v13, ZIP X4–X5, ZCF X6–2025 `.cdr`/`.cdr.zip`, `.cdt`, `.cmx`, `.cpt`, PDF/AI, EPS, DXF, PLT, multi-layer PSD/PSB, or `.svg` natively — preserving unrecognized vendor chunks verbatim and recovering legacy Windows-1256 Arabic encodings automatically.
2. **Design, trace, soften, and adjust with classic desktop precision.** Work across side-by-side multipage spreads with rearrangeable dockers, right-click palette fill/outline swatches, 65 interactive vector & raster tools (`F10` Shape, `F11` Edit Fill, `F12` Outline Pen, `Ctrl+K` Break Apart, 11 live Lenses, Vector Trace, Healing/Dodge/Burn, Arabic RTL/Kashida), backed on every click by a crash-safe SQLite WAL command journal.
3. **Export press-ready CMYK PDF/X, separations, or direct laser/cutter toolpaths.** Generate one-click `CutContour` / `Crease` spots, N-Up/Booklet/Tiling imposition, Preflight audits, embedded ICC `/OutputIntents` PDFs with curved glyphs, or kerf-compensated GRBL/Marlin/Ruida G-Code and DXF R12 / HPGL `.plt` via 7 built-in laser bridges.

---

## 4. Features (Led by Resolved Shop Pain Points)

- **Native Linux & Windows Pure-Rust Binary (`Fix 1`):** Compiles cleanly on Wayland/X11 Linux (zero Wine, zero C `-dev` headers, native `xdg-desktop-portal` dialogs, `.deb`, `.AppImage`, offline Flatpak, proper `application/vnd.corel-draw` MIME icons/thumbnails) and Windows x86_64 (`.msi` installer, `coree.exe` + `coree.com` dual CLI/GUI launcher, `PerMonitorV2` HiDPI, UTF-8 Arabic path manifest).
- **100% Offline, Forever Open Source (`Fix 2`):** Dual-licensed `MIT OR Apache-2.0` with zero cloud servers, zero license checks, zero telemetry sockets, and `#![forbid(unsafe_code)]` across all 7 workspace crates.
- **Unknown-Chunk Passthrough & Live CDR Binary Inspector (`Fix 3`):** Byte-exact preservation of unrecognized chunks across `.cdr`/`.cdt`/`.cmx` round-trips, paired with the live CDR Inspector docker (`coree inspect file.cdr --json`) and embedded PNG/DIB thumbnail extraction.
- **Zero-Lost-Click Crash Recovery & Sandboxed Rhai Macros (`Fix 4`):** Atomic temp-file + `fsync` saves with Unix `0700`/`0600` owner-only storage hardening, SQLite WAL command replay after crash/power loss, and deterministic `.rhai` macro recording/batch CLI execution.
- **R-Tree Spatial Index & Parallel Dirty-Tile Engine (`Fix 5`):** `rstar` R-Tree spatial culling combined with Rayon multi-threaded `256×256` dirty-tile rendering and shared `OnceLock` font DB keeps multi-thousand-object spreads responsive.
- **Press-Ready `CutContour`, Parametric Dielines, ICC & Laser Plugins (`Fix 6`):** Live Path Meter HUD, 4 parametric packaging dielines, CMYK/Spot (`/Separation`) & soft-proofed ICC v2/v4 PDF export, CUPS/Windows Spooler native printing (`Ctrl+P`), and built-in **LightBurn**, **RDWorks**, **EZCad**, **LaserDRW**, **LaserGRBL**, **InkCut**, and **HPGL Plotter** bridges.
- **Native Arabic RTL / Kashida & Legacy Codepage Decoding (`Fix 7`):** HarfBuzz-grade shaping via `cosmic-text`, bilingual English/Arabic live UI with `unicode-bidi` mixed-run ordering, independent Latin/RTL font selection (`document.cdss`), and automatic `CP1256` recovery on pre-Unicode `.cdr` files.

---

## 5. Pricing

See full evidence & market comparison in [pricing.md](pricing.md):

| Tier | Who It Is For | Price | What Is Included |
| :--- | :--- | :--- | :--- |
| **Community Studio (`MIT OR Apache-2.0`)** | Individual designers, Linux workstations, print & laser shops | **$0 forever** | Full desktop GUI, all 65 tools, `.cdr`/PDF/EPS/DXF/PSD codecs, CMYK PDF/X & laser exporters, CLI, full Rust source. Zero accounts, zero limits. |
| **Workshop Maintenance** | Commercial print, packaging & signage shops | Optional support contract | Offline priority bugfixes for real shop `.cdr` edge cases, custom JSON/TOML/Rhai machine bridge profiles, reproducible `.msi`/`.deb` builds. |
| **Enterprise Custom Engineering** | Fleet deployments & packaging factories | Custom scoped statement of work | Air-gapped `.msi`/Flatpak packaging, proprietary hardware dialect modules, custom ICC/preflight rules. |

---

## 6. FAQ

- **Can I import and round-trip existing `.cdr`, `.ai`, `.pdf`, `.psd`, `.eps`, `.dxf`, and `.plt` files?**
  Yes. `coree` includes pure-Rust decoders for RIFF `v4–v13`, ZIP `X4–X5`, and ZCF `X6–2025` (`v16–26`) `.cdr`/`.cdr.zip`/`.cdt`/`.cmx`/`.cpt` files (preserving unknown binary chunks byte-for-byte on re-export), alongside pure-Rust importers for PDF 1.0–2.0 / PDF-compatible `.ai`, multi-layer `.psd`/`.psb`, EPS, SVG/SVGZ, AutoCAD DXF (`R12/R2000+`), and HPGL `.plt`.
- **Why is the binary ~31 MB (~10 MB `.deb` / ~21 MB `.msi`) instead of >2 GB?**
  `coree` ships pure compiled Rust machine code with zero `.NET`/CLR, Chromium/WebView2, COM/VBA runtimes, AI weights, or bundled stock-clipart libraries; it uses your operating system's own font library directly.
- **Does it require an internet connection or login?**
  Never. `coree` contains zero networking or telemetry dependencies and works identically in air-gapped printrooms (`unshare -n`).
- **Does Linux still show `.cdr` files as plain `.zip` archives?**
  No. Installing `coree` registers `application/vnd.corel-draw` with priority-90 magic rules (`<sub-class-of type="application/zip"/>`), native SVG MIME icons, and `coree.thumbnailer` previews in GNOME/GTK file managers.

---

## 7. Final Call to Action

**Own your files, your OS, and your press workflow.**
`Download coree v0.3.1` (`.deb`, `.AppImage`, `.msi`, portable `.zip`) -> `https://github.com/ahrrfy/coree/releases/tag/v0.3.1`
