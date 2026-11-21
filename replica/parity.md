## Parity: 95.0 / 100

features 95.0  (86 counted, must-haves 62 of 68 done)

Not shippable yet: 6 must-have features are not done.

## By area, weakest first
- codecs                        64.7  (6 features)
- color-fill                    87.5  (7 features)
- typography                    90.9  (4 features)
- photo-studio                  91.2  (6 features)
- pages                         92.3  (5 features)
- workspace                    100.0  (6 features)
- selection                    100.0  (3 features)
- drawing                      100.0  (13 features)
- technical                    100.0  (3 features)
- effects                      100.0  (10 features)
- lenses                       100.0  (12 features)
- powertrace                   100.0  (3 features)
- prepress                     100.0  (8 features)

## Missing, in build order
- [must] codecs: CAD and Plotter codecs: DXF (R12/R2000+) and HPGL .plt export/import, partial  (S15 DXF R12/R2000+ & HPGL .plt export (with CutContour layer filter) implemented; DXF/PLT import deferred)
- [must] codecs: PDF and PDF/X-1a / PDF/X-4 native CMYK and Spot Color export and PDF import, partial  (S12 S15 Native DeviceCMYK/RGB PDF 1.7 + Print Marks export implemented; PDF import deferred)
- [must] codecs: Raster image codecs: PNG JPEG WebP TIFF (CMYK/RGB) BMP and PSD layer import, partial  (S10 S15 PNG/JPEG/WebP/TIFF/BMP import & export + PNG pHYs DPI implemented; PSD layer import deferred)
- [must] codecs: SVG 1.1/2.0 and EPS/PostScript import and export, partial  (S15 Full SVG 1.1/2.0 import & export + EPS export implemented; EPS import deferred)
- [must] color-fill: Color Models (CMYK RGB Lab HSB Grayscale Spot channels) and ICC Color Management, partial  (S06 S18 All 6 color models + CIE76 Delta-E + ICC profile metadata implemented; full LCMS2 ICC LUT transforms deferred)
- [must] photo-studio: Pixel Paint Eraser Clone Stamp Healing Smudge Dodge/Burn and Flood Fill brushes, partial  (S10 Paint/Eraser/CloneStamp/Smudge brushes implemented; Healing/Dodge-Burn/Raster Flood Fill deferred)
- [should] color-fill: Vector Pattern Bitmap Pattern Two-Color Pattern Texture and PostScript Fills, partial  (S06 Vector/Bitmap/Two-Color/Texture fills implemented; PostScript Fill interpreter deferred)
- [should] pages: Facing pages booklet layout and Autofit Page to content, partial  (S03 facing_pages + saddle-stitch booklet imposition implemented; one-click Autofit Page button deferred)
- [should] typography: Glyphs docker and local Font Manager (collections preview search missing-font substitution), partial  (S07 Local SFNT Font Manager with OS scan & Arabic filter implemented; dedicated Glyphs grid docker deferred)

## Left out on purpose (not scored)
- Corel Cloud storage online file sync and web review comments: offline-first design with zero cloud accounts or remote servers
- CorelDRAW Web browser editor and iPad companion app: desktop-first native Linux and Windows scope only
- User accounts license activation servers and subscription billing: zero accounts zero DRM MPL-2.0 open-source license
- Licensed Pantone books and proprietary encrypted vendor color libraries: proprietary trademarked color libraries; users import custom spot palettes instead
- Proprietary Corel clipart stock photos licensed Monotype fonts and cloud AI credits: copyrighted commercial content; users bring local fonts and assets
- Windows-only COM/VBA/VSTA macro scripting runtime: proprietary Windows COM runtime; replaced by cross-platform Rhai scripting

## Yours, not in the original (not scored)
- Native Linux-first desktop build (Wayland + X11 zero Wine pure Rust xdg-desktop-portal)
- 100% offline-first zero-account zero-telemetry MPL-2.0 operation (never phones home)
- Unknown-Chunk Passthrough in .cdr/.cdt/.cmx round-trip saves (preserve unrecognized chunks verbatim)
- Live CDR Binary/Chunk Inspector docker (hex/tree view of RIFF/ZIP chunks version codepage warnings)
- Native Arabic RTL / HarfBuzz bidirectional text shaping Kashida justification and CP1256 legacy .cdr decoder
- First-class CutContour / PerfCutContour / Crease spot presets and Parametric Packaging Dielines (FEFCO)
- Live Path Meter HUD (total cut length in mm/m node count bounding area estimated laser/plotter time)
- Crash-safe SQLite WAL Serializable Command Journal Atomic File Saves and instant crash recovery
- Cross-platform Rhai Macro Recorder and Script Runner (record DocumentCommand log into editable .rhai scripts)
- R-Tree (rstar) spatial index and Rayon multi-threaded dirty-tile rendering for 10000+ object files

## Deterministic Visual Rendering Verification (`imgdiff.py`)

Two independent release-binary renders (`./target/release/coree render ... --dpi 150`) of `showcase_lenses_vectors.coree` (containing parametric shapes, multi-stop Fountain gradients, a 4×4 Bezier Mesh Fill, Drop Shadow, Stepped Contour, and 5 live optical Lenses: `FishEye`, `Magnify`, `Invert`, `HeatMap`, `Wireframe`) were compared using `imgdiff.py --mode pixel --tolerance 0`:

- **Resolution:** `1417 × 945` px (`150 DPI`, `ViewMode::Enhanced`)
- **Pixel Determinism Score (`imgdiff.py --mode pixel --tolerance 0`):** **`100.0 / 100`** (`verdict: matches`, `changed_pixels: 0`, `height_delta_pct: 0.0`, `regions: []`)
- **Byte-for-Byte SHA-256 (Run 1 == Run 2):** `a68acf8909f23a33119fcd6c28c0d1b6adbcf7adf2bce8390968c8d6ef54de9f`
- **Generated Showcase Assets:**
  1. `showcase_lenses_vectors.png` (`1417 × 945` @ 150 DPI) — Vector shapes, Fountain gradients, 4×4 Mesh Fill, Drop Shadow, Contour, and 5 live optical Lenses (`FishEye`, `Magnify 2.2×`, `Invert`, `HeatMap`, `Wireframe`)
  2. `showcase_arabic_packaging.png` (`1654 × 1181` @ 150 DPI) — Arabic RTL + Kashida typography, Parametric Tuck-End Box Dieline (`CutContour` + `Crease`), vector QR Code & EAN-13 Barcode, live Dimensions & Path Meter
  3. `showcase_multipage.png` (`2287 × 497` @ 120 DPI) — 3-page Multipage View side-by-side with shared `AllPages` Master Layer, `VectorTable`, CMYK/Spot swatches, Sticker Sheet dieline & QR Code
