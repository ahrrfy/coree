# UI Component Specifications — `coree` Design System

All components consume semantic role tokens exclusively from `tokens.json` (35/35 color pairs verified WCAG 2.2 AA/AAA via `contrast.py`). Raw hex values are prohibited in `coree-app` widgets. Iconography uses clean-room open-source vector glyphs (MIT/ISC-licensed Lucide/Tabler or custom original SVG geometry) — never traced from third-party apps.

---

## 1. Toolbox (`ToolboxRail`)

- **variants:** `single-column` (44 px wide), `two-column` (80 px wide), `photo-studio-mode` (switches to raster tool set on `S10`)
- **sizes:** tool button `32 × 32 px` (`4 px` gap, `4 px` rail padding), active-tool indicator bar `3 px` left edge in `accent`
- **states:** `default`, `hover` (`surface-elevated` fill), `active-tool` (`accent` left bar + `surface-elevated` fill + `accent` icon tint), `flyout-open` (bottom-right `4 px` triangle indicator highlighted in `accent`), `focus-visible` (`2 px` `accent` ring), `disabled` (40% opacity, non-interactive)
- **tokens:** container `bg: surface`, `border-right: 1px border`; icon `text` / active `accent`; tooltip `bg: surface-elevated`, `text: text`, `font: xs`, `radius: sm`, `shadow: flyout`
- **keyboard & a11y:** Single-key studio shortcuts (`Space`/`V` Pick, `F10` Shape, `F6` Rectangle, `F7` Ellipse, `F8` Text, `P` Pen, `Z` Zoom, `G` Interactive Fill, `I` Eyedropper); `ArrowUp`/`ArrowDown` traverses rail when focused; `ArrowRight` or long-press opens attached Flyout; tooltip announces tool name + shortcut key.
- **used on:** `S02`, `S04`, `S10`

---

## 2. Tool Flyout (`ToolFlyoutDrawer`)

- **variants:** `horizontal-drawer` (anchored right of Toolbox button), `pinned-floating` (tear-off palette)
- **sizes:** item row height `30 px`, min width `190 px`, icon `16 × 16 px`, shortcut right-aligned in `mono` `xs`
- **states:** `closed`, `open` (`fast` `120ms` fade+slide), `item-hover`, `item-active` (checkmark + `accent` tint), `focus-visible`
- **tokens:** `bg: surface-elevated`, `border: 1px border-input`, `radius: md`, `shadow: flyout`, label `text`, shortcut `text-muted`
- **keyboard & a11y:** Opens on click-hold (`220ms`), flyout corner click, or `ArrowRight`; `ArrowUp`/`ArrowDown` moves focus; `Enter`/`Space` activates tool and closes drawer; `Escape` cancels and returns focus to parent Toolbox button.
- **used on:** `S02`, `S04`, `S10`

---

## 3. Context Property Bar (`ContextPropertyBar`)

- **variants:** `document-default` (page size, orientation, units, nudge, duplicate offset, snap toggles), `pick-transform` (X/Y, W/H, scale %, lock ratio, rotation, mirror, align, order), `shape-node` (add/delete/join/break, line/curve, cusp/smooth/symmetrical, reduce nodes), `text-typography` (font family, size, bold/italic, align, RTL/LTR toggle, Kashida slider), `fill-outline`, `effect-lens`, `table`, `dimension`, `bitmap-photo`
- **sizes:** fixed height `34 px` (`space: 4` horizontal padding, `space: 8` group gaps separated by `1 px` `border` vertical hairlines)
- **states:** `default`, `mixed-selection` (displays em-dash `—` in `text-muted` when multiple selected objects disagree; editing commits uniform value to all), `editing`, `invalid-input` (`danger` border + inline hint tooltip), `overflow-scrolled` (chevron reveal on narrow screens)
- **tokens:** `bg: surface`, `border-bottom: 1px border`, labels `text-muted` `xs`, values `text` `mono` `sm`, active toggles `bg: accent`, `text: on-accent`
- **keyboard & a11y:** `Tab` / `Shift+Tab` moves across controls; `Enter` commits edit and returns focus to Canvas (`S02`); `Escape` reverts edit and returns focus to Canvas; every numeric input accepts arithmetic expressions (`+10mm`, `/2`, `*1.5`).
- **used on:** `S02`, `S03`, `S04`, `S07`, `S08`, `S10`

---

## 4. Canvas Viewport & Interactive Rulers (`CanvasViewport`, `RulerStrip`)

- **variants:** `vector-enhanced`, `wireframe`, `simple-wireframe`, `pixels`, `simulate-overprints`, `multipage-view` (`S03`), `powerclip-isolation` (dimmed backdrop outside container), `photo-studio-pixel` (`S10`)
- **sizes:** fluid remaining viewport; top & left rulers `20 px` thick with `20 × 20 px` draggable zero-origin corner box
- **states:** `idle`, `panning` (middle-mouse or `H`/spacebar drag), `zooming` (marquee box in `accent`), `selecting-marquee`, `dragging-object` (live snap guide lines in `accent-secondary` + coordinate HUD pill), `node-editing` (hollow cusp squares, smooth circles, Bézier control handles in `accent`), `cutcontour-highlight` (`cutcontour` dashed overlay), `rendering-tiles` (non-blocking subtle corner spinner only if frame > `150ms`)
- **tokens:** pasteboard `canvas-well`, active page paper `#ffffff` (or document background) with crisp `1 px` `border` + subtle drop shadow, bleed box `danger` dashed hairline, margin/guidelines `accent` / `accent-secondary`, ruler `bg: surface`, tick marks `border-input`, numbers `text-muted` `mono` `xs`
- **keyboard & a11y:** Arrow keys nudge by document nudge distance (`Shift+Arrow` super-nudge, `Ctrl+Arrow` micro-nudge); `Ctrl+Plus/Minus/0/F2/F4/Shift+F2` zoom presets; double-click ruler origin resets `(0, 0)`; drag from ruler pulls new guide; screen-reader live region announces selected object count, type, dimensions, and layer name.
- **used on:** `S02`, `S03`, `S04`, `S10`, `S11`, `S12`

---

## 5. Page Tabs (`PageTabBar`)

- **variants:** `bottom-document-strip` (left of horizontal scrollbar)
- **sizes:** height `26 px`, tab min-width `72 px`, padding `0 10 px`, `+` add-page button `26 × 26 px`
- **states:** `active-page` (`bg: surface-elevated`, `text: text`, `2 px` top bar in `accent`), `inactive-page` (`bg: surface`, `text: text-muted`), `hover`, `dragging-reorder` (vertical `2 px` `accent` insertion bar), `renaming-inline`, `master-badge` (`accent-secondary` pill on Master pages)
- **tokens:** container `bg: surface`, `border-top: 1px border`; font `sm`
- **keyboard & a11y:** `PageUp` / `PageDown` switches previous/next page globally; `F2` on focused tab renames page; right-click / `Shift+F10` opens page context menu (Insert Before/After, Duplicate, Rename, Orientation, Delete); deleting last remaining page is disabled with tooltip reason.
- **used on:** `S02`, `S03`

---

## 6. Rearrangeable Dockers (`DockerPane`, `DockerTabStrip`)

- **variants:** `docked-right`, `docked-left`, `tabbed-stack`, `floating-window`, `collapsed-icon-rail`
- **sizes:** default width `300 px` (min `240 px`, max `520 px`), header height `30 px`, collapsible accordion sections with `12 px` internal padding
- **states:** `expanded`, `collapsed`, `active-tab`, `inactive-tab`, `drag-docking-preview` (semi-transparent `accent` drop zone overlay), `empty` (clear guidance when no object is selected, e.g. Lens Docker on `S09` explains "Select a closed vector shape to apply a Lens"), `disabled-control`
- **tokens:** `bg: surface`, active tab `bg: surface-elevated` + `2 px` `accent` underline, section divider `border`, header `text` `sm/600`, metadata `text-muted` `xs`
- **keyboard & a11y:** Direct docker shortcuts (`Alt+Shift+O` Objects, `Alt+Enter` Properties, `Alt+F3` Lens, `Ctrl+T` Text); `Ctrl+Tab` cycles stacked docker tabs; collapsible headers use toggle button semantics with `Space`/`Enter` and visible `focus-visible` ring.
- **used on:** `S02`, `S04`, `S05`, `S06`, `S07`, `S08`, `S09`, `S14`, `S16`, `S17`

---

## 7. Color Palette Swatch Bar (`ColorPaletteSwatchBar`)

- **variants:** `docked-right-strip` (vertical `32 px`), `expanded-grid` (multi-column flyout), `show-color-names` (wide list showing swatch + exact CMYK/Spot name, directly fixing user request `tracedraw/issues/3790`), `cutcontour-production-strip` (`CutContour`, `PerfCutContour`, `Crease`, `BleedGuide`, `Registration`)
- **sizes:** swatch cell `22 × 22 px` (`2 px` gap), top "No Fill / No Outline" slash swatch `22 × 22 px`, bottom expand chevron
- **states:** `default`, `hover` (crisp `1 px` `text` ring + tooltip with swatch name, color space & exact C/M/Y/K or Spot channel), `active-fill` (inner indicator), `active-stroke` (outer border indicator), `hold-tint-popup` (long-press opens 5×5 shade/tint matrix)
- **tokens:** container `bg: surface`, `border-left: 1px border`; spot color swatches display a bottom-right white triangle dot (`4 px`) so spot inks (`CutContour`) are distinguishable from process CMYK at a glance (not relying on color alone).
- **keyboard & a11y:** Left-click sets Fill, Right-click sets Outline (classic studio muscle memory); keyboard `Enter` sets Fill, `Shift+Enter` sets Outline; `Arrow` keys traverse swatches; tooltips expose exact numerical values for color-blind accessibility.
- **used on:** `S02`, `S06`, `S14`

---

## 8. Dialogs & Production Sheets (`DialogModal`)

- **variants:** `standard-modal` (New Document, Export Options `S15`, Missing Fonts), `wide-studio-dialog` (Bitmap Trace Studio `S11` with SplitPreviewCanvas, Print Preview & Imposition `S12`), `preflight-audit-sheet` (`S13`), `packaging-wizard` (`S14`), `confirm-destructive` (Delete Page, Discard Recovery)
- **sizes:** `sm` `420 px`, `md` `640 px`, `lg` `960 px`, `xl` `88vw × 84vh` (capped at `1400 × 900 px`), header `44 px`, footer `52 px`
- **states:** `open`, `validating`, `validation-error` (inline `danger` message + error summary banner), `busy-progress` (determinate progress bar + `Cancel` button keeping modal responsive), `clean-pass` (`success` status pill)
- **tokens:** backdrop `rgba(0, 0, 0, 0.62)`, surface `surface-elevated`, `border: 1px border-input`, `radius: lg`, `shadow: modal`, title `lg` `text`, body `base` `text`, footer `bg: surface`, `border-top: 1px border`
- **keyboard & a11y:** Focus traps strictly inside dialog while open; initial focus lands on primary input or safe default; `Escape` dismisses (with unsaved-change guard if dirty); `Enter` submits primary CTA unless focused on multiline input; returns focus to invoking trigger on close.
- **used on:** `S01`, `S11`, `S12`, `S13`, `S14`, `S15`, `S18`

---

## 9. Command Palette (`CommandPalette`)

- **variants:** `quick-action` (`Ctrl+K` — tools, menus, effects, dockers, lenses, Rhai macros, preferences), `object-jump` (`Ctrl+Shift+O` — search scene nodes by name/layer/spot color)
- **sizes:** centered top-anchored modal `560 px` wide, max-height `440 px`, search input height `44 px`, result row height `34 px`
- **states:** `closed`, `empty-recent` (shows recently executed commands), `filtering-matches` (fuzzy match highlights in `accent`), `active-row` (`bg: surface`, `2 px` left `accent` bar), `no-results` (clear empty state)
- **tokens:** `bg: surface-elevated`, `border: 1px border-input`, `radius: lg`, `shadow: modal`, input `base` `text`, category badge `xs` `text-muted`, shortcut keycap `bg: bg`, `border: 1px border`, `mono` `xs`
- **keyboard & a11y:** Global `Ctrl+K` (or `F1` fallback) toggles open/closed; `ArrowUp`/`ArrowDown` wraps through matches; `Enter` executes and closes; `Escape` closes immediately; live count of matching commands announced cleanly.
- **used on:** All screens (`S01`–`S18`)

---

## 10. Status Bar & Live Path Meter HUD (`StatusBar`)

- **variants:** `compact-studio` (default), `expanded-path-meter` (click Path Meter pill to expand per-layer cut/crease breakdown)
- **sizes:** fixed height `26 px`, full window width, 4 structured zones:
  1. Left: Active selection summary (`2 Curves on Layer 1` / `RTL Paragraph Text`) + cursor `(X, Y)` in `mono` `xs`.
  2. Center-left: **Live Path Meter HUD** pill (`Cut: 1,420.5 mm | 84 nodes | ~18s @ 80mm/s`) highlighted with `cutcontour` badge when a CutContour layer or path is selected.
  3. Center-right: Active Fill & Outline live swatches (double-click opens `S06` Fill/Outline editor) + Document Color Mode (`CMYK / FOGRA39`) + Offline/Local badge (`Offline · Local WAL`).
  4. Right: Autosave status (`Saved` in `success` / `Recording Macro` pulsing `accent-secondary` dot) + Zoom % dropdown.
- **states:** `default`, `selection-active`, `cutcontour-active`, `macro-recording`, `autosave-dirty`, `preflight-alert` (clickable `warning`/`danger` count pill opening `S13`)
- **tokens:** `bg: surface`, `border-top: 1px border`, text `text-muted` `xs`, active metrics `text` `mono` `xs`
- **keyboard & a11y:** Status pills are real focusable buttons (`Enter`/`Space` opens Preflight, Path Meter popover, Fill/Outline, or Zoom menu); numeric readouts use tabular numerals (`font-variant-numeric: tabular-nums`) so coordinates never jitter during mouse motion.
- **used on:** `S02`, `S03`, `S04`, `S10`, `S14`

---

## 11. Shared Form & Data Primitives

- **Button:** variants `primary` (`bg: accent`, `text: on-accent`), `secondary` (`bg: surface-elevated`, `border: border-input`, `text: text`), `ghost` (transparent -> hover `surface-elevated`), `toggle-icon`, `danger` (`bg: danger`, `text: on-danger`); sizes `xs 24px` (toolbar), `sm 30px` (dockers), `md 36px` (dialogs); states `default`, `hover`, `active`, `focus-visible` (`2px` `accent` ring), `disabled`, `loading` (keeps accessible label). Used on all screens.
- **NumericSpinbox & UnitInput:** supports `mm`, `cm`, `in`, `pt`, `px`, `deg`, `%`, horizontal label-scrub drag, mouse-wheel step (`Shift` ×10, `Alt` ×0.1), math expression evaluation, `mixed` (`—`) state, and `danger` invalid state. Used on `S02`–`S18`.
- **TreeView (Objects & CDR Chunk Inspector):** collapsible rows (`26 px`), drag-and-drop reordering with drop-target line in `accent`, per-row visibility/lock/print/cut icon toggles (`S05`), and `Parsed` (`success`) vs `Preserved Raw` (`warning`) unknown-chunk pills (`S16`). Full `ArrowUp/Down/Left/Right` keyboard navigation.
- **SplitPreviewCanvas:** draggable vertical swipe divider (`accent` handle) comparing original raster vs Bitmap Trace vectors (`S11`) or full composite vs CMYK/Spot separation plates (`S12`), synchronized pan/zoom.
- **Toast & Non-Blocking Banner:** variants `info` (`accent`), `success` (`success`), `warning` (`warning`), `error` (`danger`), `crash-recovery` (`accent-secondary`); dismissible via `Escape` or close button; never steals focus from active canvas drawing.
