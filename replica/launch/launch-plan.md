# Launch Plan — `coree` (`v0.3.0`)

Grounds every rollout step in public user communities verified in [reviews.csv](../reviews.csv) and [feedback.md](../feedback.md), strictly avoiding fabricated metrics, borrowed testimonials, or paid ad trademark infringement.

---

## 1. Pre-Launch Readiness Checklist (Desktop Open-Source Verification)

- **Zero-Telemetry Diagnostic Readiness:** `coree` collects zero analytics or crash-upload beacons by default. Instead, users attach plain local output from `coree --diagnostics` and `coree inspect <file.cdr> --json` cleanly inside GitHub Issue templates (`.github/ISSUE_TEMPLATE/`).
- **Binary & Package Distribution Live (`v0.3.0` on GitHub Releases):**
  - Windows (`x86_64`): `.msi` installer (`coree.exe` + `coree.com` CLI + icons/file associations) and Portable `.zip` (`register-file-associations.cmd`).
  - Linux (`x86_64`): Ubuntu/Debian `.deb` (`application/vnd.corel-draw` MIME + thumbnailer), universal `.AppImage`, `.tar.gz`, and zero-network Flatpak manifest.
- **Legal & Clean-Room Gates:**
  - `python3 listing.py replica/launch/listing.json` — `0 errors, 0 warnings`.
  - `python3 sweep.py crates/ packaging/ replica/launch/listing.json --config replica/brand.json` — clean.
  - `cargo test --workspace --all-targets` + `cargo clippy --workspace --all-targets -- -D warnings` + GitHub Actions (`ubuntu-latest` + `windows-latest`) green.

---

## 2. Target Channels Surfaced by Research (`reviews.csv`)

1. **Hacker News (`Show HN`):** Existing HN threads (`id=40709076`, `id=48044466`, `id=48975103`) show strong demand for native Linux desktop vector/CDR interoperability, perpetual ownership, and reliable `.cdr` decoding.
2. **Laser & CNC Maker Communities (`GitHub` / LightBurn / Rayforge / GRBL):** Workshops trapped between `.cdr` customer art files and Linux laser controllers (`barebaric/rayforge/issues/575`). Lead with **direct `.cdr`/`.cdr.zip` open + kerf/dedupe/hatch + GRBL/Ruida G-Code & DXF R12 bridge**.
3. **Open-Source Print & Multipage Users (`GitLab Inkscape` prepress issues `#2094`, `#2480`, `#12103`):** Do not spam unrelated threads; share release notes respectfully in Linux graphic design / FOSS prepress forums focusing on **native DeviceCMYK PDF/X + Spot `/Separation` + side-by-side Multipage View**.
4. **MENA / Arabic Print & Signage Communities:** Lead with **native HarfBuzz/`cosmic-text` RTL + Kashida justification, curved Arabic PDF/EPS export (no Tofu boxes), bilingual Arabic UI, and automatic CP1256 legacy `.cdr` recovery**.

---

## 3. Show HN / GitHub Announcement Draft (Leads with the Fixes)

**Title:** `Show HN: coree – Clean-room offline Rust vector, CMYK prepress, laser & .cdr studio for Linux and Windows`

> Print shops, sign makers, and laser workshops keep Linux/offline workstations blocked because open-source tools lack native CMYK PDF separations, `CutContour` packaging workflows, and reliable `.cdr` X6–2025 import, while commercial suites force online activation or subscriptions.
>
> `coree` (`MIT OR Apache-2.0`, 100% pure Rust `#![forbid(unsafe_code)]`, 0 network crates) runs natively on Wayland/X11 Linux and Windows:
> - Parses RIFF `v4–v13`, ZIP `X4–X5`, and ZCF `X6–2025` (`v16–v26`) `.cdr`/`.cdr.zip` files with byte-exact Unknown-Chunk Passthrough, plus PDF/AI, EPS, DXF, PLT, PSD/PSB, and SVG.
> - Exports DeviceCMYK PDF with `/Separation` spots (`CutContour`), custom ICC v2/v4 `/OutputIntents`, N-Up/Booklet/Tiling imposition, curved Arabic/Unicode glyph outlines, and kerf/hatch-optimized GRBL/Marlin/Ruida G-Code or LightBurn/RDWorks bridges.
> - Recovers unsaved edits cleanly after crashes via an atomic snapshot + SQLite WAL command journal.

---

## 4. First-10 Real User Outreach Plan (Zero Spam, Workshop-First)

1. **3 local/regional commercial print shops** handling daily `.cdr` wedding cards, shields, and offset CMYK runs (specifically testing legacy Arabic CP1256 & ZCF v21–v25 customer files read-only via `coree inspect` and PDF export verification).
2. **3 CO2 / diode / fiber laser workshops** running LightBurn, RDWorks, or LaserGRBL on Ubuntu/Windows (testing `coree laser --dedupe --kerf --hatch --weeding-box` output on real DXF/G-Code jobs).
3. **2 vinyl signage & packaging box shops** using `CutContour` plotters and FEFCO tuck-end/sticker-sheet parametric dielines + HPGL `.plt`.
4. **2 Linux desktop designers** migrating multipage layouts and testing `.deb` / `.AppImage` / GNOME MIME thumbnailer ergonomics.

**Interview rule for each of the first 10:** Ask them to test a **copy** of their trickiest real-world file, capture exact failure chunk/rendering notes via `coree inspect --json` (preserving privacy), and fix reproducible codec/rendering discrepancies in incremental patch releases.
