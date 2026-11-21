# Pricing & Market Positioning — `coree`

> **Verification Date:** `2026-11-22` (live public pricing checked via web search prior to drafting). Prices shown in USD vary by region, local taxes/VAT, and seasonal promotions.

---

## 1. Public Market Pricing Comparison

| Product | Model | Public Price | Platforms | Offline Perpetual Rights | Direct Source & Date Read |
| :--- | :--- | :--- | :--- | :--- | :--- |
| **CorelDRAW Graphics Suite** | Annual Subscription | **~$439.00 / yr** *(promotional first-year discounts ~\$305)* | Windows, macOS, Web | Stops updates/web when lapsed; periodic online checks reported by users | `https://www.coreldraw.com` *(read 2026-11-22)* |
| **CorelDRAW Graphics Suite** | One-Time Purchase | **~$1,030.00** *(commercial single license)* | Windows, macOS | Version-locked; excludes next major version upgrades and subscriber cloud/web tools | `https://www.coreldraw.com` *(read 2026-11-22)* |
| **Adobe Illustrator (Single App)** | Creative Cloud Subscription | **$22.99 / mo** *(annual plan, billed monthly)* | Windows, macOS, iPad | Subscription-only; no perpetual buyout option | `https://www.adobe.com` *(read 2026-11-22)* |
| **Inkscape** | Free & Open Source (`GPL`) | **$0.00** | Linux, Windows, macOS | Permanent offline; lacks native CMYK PDF/X-1a separations & native `.cdr` ZCF write | `https://inkscape.org` *(read 2026-11-22)* |
| **`coree`** | Free & Open Source (`MIT OR Apache-2.0`) | **$0.00 forever** | **Linux-first** & **Windows** | **100% offline, permanent, air-gapped, full Rust source available** | `https://github.com/ahrrfy/coree` *(2026-11-22)* |

---

## 2. User Evidence on Pricing, Billing & Lockout (`replica/feedback.md`)

Across **378 real user items** collected across 5 public sources:
- **Predatory subscriptions & prohibitive one-time pricing:** **87 reviews across 3 sources** (`app-store`, `hacker-news`, `gitlab-inkscape`; #1 ranked complaint overall, avg **1.8★** on rated reviews).
- **Forced online server authentication / "cannot connect to server" lockouts:** **21 mentions across 3 sources** (`app-store`, `github`, `hacker-news`).
- **Trial credit-card traps & painful subscription cancellation:** **19 reviews across 1 source** (`app-store`, avg **1.1★**; marked **thin** — 1 source).

---

## 3. `coree` Packaging & Optional Commercial Support Tiers (Max 3)

Because `coree` guarantees in [replica/recon.md](../recon.md) and [LICENSE-MIT](../../LICENSE-MIT) / [LICENSE-APACHE](../../LICENSE-APACHE) that **all core desktop software, formats, prepress tools, and laser bridges remain 100% free and offline forever** (no license keys, no Stripe checkout inside the desktop binary, no paywalled CMYK/PDF/laser features), commercial tiers fund strictly **optional human engineering & priority workshop support**:

### Tier 1 — Community Studio (`$0` Forever · Individual & Shop Self-Serve)
- **For:** Freelance designers, print operators, laser/vinyl hobbyists, Linux & Windows workshops.
- **Included:** Full `coree` desktop GUI, headless CLI (`coree` / `coree.com`), all 65 interactive tools, native `.cdr`/`.cdt`/`.cmx`/`.cpt`/PDF/EPS/DXF/PLT/PSD/SVG support, CMYK/Spot PDF/X, ICC soft-proofing, 7 laser/plotter plugins, Rhai scripting, and all updates via GitHub Releases (`MIT OR Apache-2.0`).
- **Accounts / Cards Required:** None. Zero trial clocks. Zero network calls.

### Tier 2 — Commercial Workshop Maintenance (`Flat Annual Studio Retainer`, Optional)
- **For:** High-volume print houses, signage shops, and packaging converters running air-gapped production lines.
- **Included:** Direct priority triage & regression fixes for proprietary real-world legacy `.cdr` shop templates (`Unknown-Chunk` decoders), custom machine plugin profiles (`JSON`/`TOML`/`Rhai`), and signed offline `.msi` / `.deb` fleet packages.
- **Billing Anti-Lock-In Rules (Eliminating Review Complaints):** Flat per-site pricing with zero surprise per-seat jumps, zero DRM or software deactivation if support lapses (installed binaries keep working permanently), plain annual invoice or optional 1-click renewal with transparent 30-day notice. *(No automated subscription paywall is embedded in the product; users request support externally if needed.)*

### Tier 3 — Custom Print / Laser OEM Integration (`Scoped Engineering SOW`, Optional)
- **For:** Multi-factory packaging companies or hardware controller vendors.
- **Included:** Dedicated machine protocol dialects, bespoke imposition/dieline libraries, custom ICC calibration QA, and air-gapped enterprise packaging compliance.
