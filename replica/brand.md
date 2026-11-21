# Brand Specification — `coree`

Grounds `coree`'s identity in the recommended positioning angle from [fixes.md](file:///run/media/ahmed/DATA/coreldraw%20me/replica/fixes.md):
> For print shops, sign makers, and laser/vinyl cutting workshops who are sick of expensive subscriptions, server lockouts, and Windows-only lock-in just to open `.cdr` files and drive production machinery, **`coree`** is a fast, native Linux & Windows pure-Rust vector production studio with lossless `.cdr` round-tripping, native CMYK prepress, and built-in `CutContour` dielines — free forever under MPL-2.0, completely offline.

---

## 1. Naming Exploration & Finalists

### 20 Initial Candidates (Across 5 Styles)

- **Descriptive:** `CutPress`, `VectorPlate`, `PrintCurve`, `DielineStudio`
- **Compound:** `PathForge`, `NodePress`, `InkPlate`, `SplineCraft`
- **Invented:** `coree`, `Kalyx`, `Vexra`, `Tramo`
- **Metaphor:** `Caliper`, `Burin`, `Platen`, `Gantry`
- **Verb / Imperative:** `Scribe`, `Score`, `Bevel`, `Traceit`

### Shortlist (5 Finalists)

1. **`coree`** *(selected project name)* — Short, 5-letter lowercase invented name easy to spell and type as a Linux CLI binary (`coree`).
   - **Honest Trademark Clearance Caution:** Because `coree` shares the prefix `core-` with Corel Corporation (Class 9 graphics software), **a trademark attorney MUST review phonetic/visual similarity under USPTO/EUIPO/CIPO standards before any commercial trademark filing or paid promotion**. If legal clearance advises greater distance before a commercial launch, `Platen` or `Burin` are prepared zero-overlap fallback names below.
2. **`Platen`** — Metaphor from commercial printing/die-cutting press plates; unmistakable print-shop heritage, zero overlap with Corel.
3. **`Burin`** — Precision engraving chisel metaphor; crisp 5-letter CLI binary (`burin`), ties directly to laser/CNC/vector cutting.
4. **`NodePress`** — Compound linking Bézier node precision + commercial prepress.
5. **`Caliper`** — Universal precision measurement & packaging dieline metaphor.

---

## 2. Trademark, Domain, Store & Handle Checks

> **Honest Status Rule:** No live legal/registrar/registry searches were executed during offline design specification. Every check below is marked **`to run`**. Screening checks are **not** legal clearance; consult a trademark attorney before investing in a public brand launch.

| candidate | US trademark (`tmsearch.uspto.gov`, class 9 & 42) | EU trademark (`euipo.europa.eu` / TMview) | Canada (`ised-isde.canada.ca`) | Global (WIPO Global Brand DB) | Domain (`whois` / registrar) | Linux / App Stores (Flathub, Snap, MS Store, Mac) | Handles (GitHub, X, Mastodon, YouTube) | Web (`"<name>" vector graphics`) |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| **`coree`** | **to run** *(flag: assess similarity to `Corel` in Class 9)* | **to run** | **to run** | **to run** | **to run** (`coree.org`, `coree.app`) | **to run** | **to run** (`github.com/ahrrfy/corelee` repo exists; org handle to run) | **to run** |
| **`Platen`** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** |
| **`Burin`** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** |
| **`NodePress`** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** |
| **`Caliper`** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** | **to run** |

---

## 3. Clean-Room Brand Palette (WCAG 2.2 Verified)

`coree` deliberately avoids Corel's signature emerald green (`#009c4a`, hue ~148°) and bright balloon trade dress. Instead, `coree` adopts a **Neutral Graphite + Electric Cobalt + Studio Amber** industrial precision palette defined in [tokens.json](file:///run/media/ahmed/DATA/coreldraw%20me/replica/design/tokens.json):

| role | token | hex | purpose | contrast check (`contrast.py`) |
| --- | --- | --- | --- | --- |
| Canvas Surround | `bg` | `#111318` | Neutral dark graphite base preventing CMYK chroma bleed | `text` on `bg`: **16.85:1 (AAA)** |
| Panel Surface | `surface` | `#191c24` | Dockers, Toolbox, Property Bar, Status Bar | `text` on `surface`: **15.45:1 (AAA)** |
| Elevated Surface | `surface-elevated` | `#232734` | Dialogs, Flyouts, Command Palette, Active Tabs | `text` on `surface-elevated`: **13.50:1 (AAA)** |
| Primary Brand Accent | `accent` | `#3ea6ff` | Electric Cobalt — primary CTA, active tool, Bézier selection, focus ring (~208° hue, 60° away from Corel green) | `on-accent` (`#07101c`) on `accent`: **7.37:1 (AAA)**; `accent` on `surface`: **6.57:1 (AA)** |
| Secondary Studio Accent | `accent-secondary` | `#f6ad3c` | Studio Amber — snap guides, crash recovery, macro recording | `on-accent-secondary` on `accent-secondary`: **9.70:1 (AAA)** |
| CutContour Production Accent | `cutcontour` | `#ff66c4` | High-visibility magenta indicator for `CutContour` & packaging dielines | `cutcontour` on `surface`: **6.45:1 (AA)** |
| Semantic Status | `success` / `warning` / `danger` | `#38d9a9` / `#fbbf24` / `#ff7078` | Preflight pass, out-of-gamut/unknown-chunk warnings, parser/preflight errors | All **AA/AAA** (35/35 pairs pass, **0 failures**) |

---

## 4. Original SVG Logo Brief

- **One-line concept:** A crisp geometric registration-crosshair fused with a sweeping open cubic Bézier caliper curve forming a subtle lowercase `c` — signaling industrial prepress registration accuracy and fearless vector curve control.
- **Mark type:** Standalone geometric symbol + clean geometric lowercase sans wordmark **`coree`** (set in `Inter` SemiBold/Bold with tight studio tracking `-0.03em`).
- **Colorway:**
  - Primary dark-background mark: Electric Cobalt (`#3ea6ff`) primary Bézier arc + pure white (`#f1f4f9`) registration crosshair + subtle Studio Amber (`#f6ad3c`) anchor node dot on `#111318` graphite squircle.
  - Monochrome / single-ink stamp: 100% solid `#111318` or `#f1f4f9` (must remain unmistakable in 1-bit vinyl cut or monochrome print header).
- **Scale & legibility requirements:**
  - Must read cleanly at **`16 × 16 px`** (favicon, window titlebar, Linux panel tray) with no hairlines thinner than `1.5 px` at 16px grid.
  - Must scale crisply to **`1024 × 1024 px`** desktop app icon without gradient banding.
- **Deliverables:**
  1. `assets/brand/coree-mark.svg` & `assets/brand/coree-lockup.svg` (clean, hand-optimized SVG `< 4 KB`, zero embedded raster).
  2. `1024 × 1024` opaque PNG app icon (for macOS/Windows/store metadata) + Linux `hicolor` icon PNGs (`16`, `32`, `48`, `128`, `256`, `512`) + Windows `.ico`.
  3. Favicon set (`favicon.svg`, `32×32`, `180×180`).
  4. `1200 × 630` repository social preview card (showing the `coree` mark, tagline, and dark studio vector wireframe grid).
- **Non-resemblance rule (mandatory visual review):** Place Corel's green hot-air balloon / pencil-tip mark side-by-side with every draft and verify **zero shared silhouette, zero balloon/pencil motif, zero emerald green (`#009c4a`), and zero shared letterform trick**.

---

## 5. Brand Voice Guide

### Three Voice Pillars

1. **Direct, not blunt** — Tell print operators and designers exactly what happened (millimeters, chunk counts, DPI, color space) without marketing fluff or robotic terseness.
2. **Craft-focused, not elitist** — Respect press-room terminology (`bleed`, `overprint`, `CutContour`, `Kashida`, `CP1256`) while making every tool approachable with clear inline hints.
3. **Sovereign, not preachy** — Quietly prove local ownership ("Saved locally", "Zero network required") instead of lecturing users about subscriptions or competitors.

### Five Do / Don't Copy Pairs

| Do (coree voice) | Don't (avoid) |
| --- | --- |
| `Export PDF/X-1a (CMYK · 3 mm bleed)` | `Unleash your creative vision to the cloud!` |
| `Opened legacy CDR v12 (CP1256). 4 unrecognized chunks preserved verbatim.` | `Warning: Some unsupported elements may have been lost.` |
| `Preflight found 2 low-DPI bitmaps (< 300 DPI). Click to inspect.` | `Oops! Something looks a little off with your print file!` |
| `Restored 14 unsaved edits from local crash journal.` | `Syncing your workspace with your account...` |
| `100% offline. No account needed.` | `Upgrade to Pro to unlock CMYK separations!` |

### Ten Rewritten High-Visibility UI Strings

1. **Welcome primary button (`S01`):** `New Print or Vector Document`
2. **Welcome offline assurance subtitle (`S01`):** `Local-first vector, photo & prepress studio — zero accounts, zero subscriptions.`
3. **Crash recovery banner (`S01`, `S17`):** `Recovered unsaved session "packaging-box.cdr" (18 logged edits). Restore or inspect timeline.`
4. **Empty canvas guidance (`S02`):** `Press F6 for Rectangle, P for Pen, F8 for Text, or Ctrl+K to jump to any tool.`
5. **Empty Lens Docker state (`S09`):** `Select a closed vector shape to attach any of the 11 non-destructive Lenses.`
6. **Legacy `.cdr` codepage import prompt (`S15`):** `Legacy non-Unicode CDR detected. Select original text codepage (e.g., Arabic CP1256 or Cyrillic CP1251) with live preview.`
7. **Zip-Bomb / safety rejection error (`S15`):** `Import halted for safety: archive exceeds the 512 MiB uncompressed ceiling or contains an unsafe relative path.`
8. **Missing font substitution dialog (`S07`):** `2 document fonts are not installed locally. Choose temporary substitutions — original font names remain preserved on save.`
9. **Preflight clean pass (`S13`):** `Press ready — 0 errors across CMYK color space, 300 DPI bitmaps, fonts, hairlines, and 3 mm bleed.`
10. **Live Path Meter summary (`S02`, `S14`):** `CutContour: 1,420.5 mm · Crease: 680.0 mm · 84 nodes · Est. cut time: 18 s.`

---

## 6. Rebrand Sweep Gate (`brand.json`)

[brand.json](file:///run/media/ahmed/DATA/coreldraw%20me/replica/brand.json) configures `replica-brand/sweep.py` with Corel's trademarks, company names, domains, and signature green palette so CI and release checks ensure zero leftover third-party brand strings or colors ship inside `crates/` UI/source code:

```bash
python3 /home/ahmed/.gemini/config/plugins/replica-skill/skills/replica-brand/sweep.py . --config replica/brand.json
```

*(Note: `sweep.py` intentionally skips `replica/` planning docs; legal trademark notices in root `NOTICE` / `README.md` are reviewed explicitly by eye while all `crates/` code, icons, and UI strings must remain 100% clean.)*
