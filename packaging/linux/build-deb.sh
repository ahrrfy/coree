#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Zero-sudo native Ubuntu/Debian .deb package builder for coree using dpkg-deb.

set -euo pipefail
umask 022

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
fi

# Extract default version from workspace Cargo.toml unless overridden via --version or COREE_VERSION
DEFAULT_VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' "${REPO_ROOT}/Cargo.toml" | head -n 1)"
VERSION="${COREE_VERSION:-${DEFAULT_VERSION:-0.3.1}}"
ARCH="${COREE_DEB_ARCH:-$(dpkg --print-architecture 2>/dev/null || echo amd64)}"
OUT_DIR="${REPO_ROOT}/dist"
BINARY_PATH="${REPO_ROOT}/target/release/coree"
SKIP_BUILD=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --version)
            if [[ $# -lt 2 ]]; then
                echo "Error: --version requires an argument" >&2
                exit 2
            fi
            VERSION="${2#v}"
            shift 2
            ;;
        --arch)
            if [[ $# -lt 2 ]]; then
                echo "Error: --arch requires an argument" >&2
                exit 2
            fi
            ARCH="$2"
            shift 2
            ;;
        --out-dir)
            if [[ $# -lt 2 ]]; then
                echo "Error: --out-dir requires an argument" >&2
                exit 2
            fi
            OUT_DIR="$2"
            shift 2
            ;;
        --bin)
            if [[ $# -lt 2 ]]; then
                echo "Error: --bin requires a path argument" >&2
                exit 2
            fi
            BINARY_PATH="$2"
            SKIP_BUILD=1
            shift 2
            ;;
        --no-build)
            SKIP_BUILD=1
            shift
            ;;
        -h|--help)
            echo "Usage: $0 [--version X.Y.Z] [--arch amd64] [--out-dir dist] [--bin /path/to/coree] [--no-build]"
            echo "Builds a native Debian/Ubuntu .deb package without sudo."
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 2
            ;;
    esac
done

VERSION="${VERSION#v}"

DPKG_DEB="$(command -v dpkg-deb || true)"
if [[ -z "${DPKG_DEB}" ]]; then
    echo "[coree] Error: dpkg-deb not found. Install dpkg (e.g. sudo apt install dpkg)." >&2
    exit 1
fi

if [[ "${SKIP_BUILD}" -eq 0 && ! -x "${BINARY_PATH}" ]]; then
    echo "[coree] Building release binary..."
    cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

if [[ ! -x "${BINARY_PATH}" ]]; then
    echo "[coree] Error: release binary not found or not executable at ${BINARY_PATH}" >&2
    exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

PKG_ROOT="${WORK_DIR}/coree_${VERSION}_${ARCH}"
mkdir -p \
    "${PKG_ROOT}/DEBIAN" \
    "${PKG_ROOT}/usr/bin" \
    "${PKG_ROOT}/usr/share/applications" \
    "${PKG_ROOT}/usr/share/mime/packages" \
    "${PKG_ROOT}/usr/share/icons/hicolor/scalable/apps" \
    "${PKG_ROOT}/usr/share/icons/hicolor/scalable/mimetypes" \
    "${PKG_ROOT}/usr/share/thumbnailers" \
    "${PKG_ROOT}/usr/share/metainfo" \
    "${PKG_ROOT}/usr/share/doc/coree"

install -Dm755 "${BINARY_PATH}" "${PKG_ROOT}/usr/bin/coree"
install -Dm644 "${SCRIPT_DIR}/coree.desktop" "${PKG_ROOT}/usr/share/applications/coree.desktop"
install -Dm644 "${SCRIPT_DIR}/coree-mime.xml" "${PKG_ROOT}/usr/share/mime/packages/coree-mime.xml"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${PKG_ROOT}/usr/share/icons/hicolor/scalable/apps/coree.svg"

MIME_ICON_NAMES=(
    application-vnd.corel-draw
    application-x-cdr
    application-x-coree
    application-x-cdt
    application-x-cmx
    application-x-cpt
)
for mime_icon in "${MIME_ICON_NAMES[@]}"; do
    install -Dm644 "${SCRIPT_DIR}/coree.svg" \
        "${PKG_ROOT}/usr/share/icons/hicolor/scalable/mimetypes/${mime_icon}.svg"
done

install -Dm644 "${SCRIPT_DIR}/coree.thumbnailer" "${PKG_ROOT}/usr/share/thumbnailers/coree.thumbnailer"
install -Dm644 "${SCRIPT_DIR}/flatpak/io.github.ahrrfy.coree.metainfo.xml" \
    "${PKG_ROOT}/usr/share/metainfo/io.github.ahrrfy.coree.metainfo.xml"

install -Dm644 "${REPO_ROOT}/LICENSE-MIT" "${PKG_ROOT}/usr/share/doc/coree/LICENSE-MIT"
install -Dm644 "${REPO_ROOT}/LICENSE-APACHE" "${PKG_ROOT}/usr/share/doc/coree/LICENSE-APACHE"
install -Dm644 "${REPO_ROOT}/NOTICE" "${PKG_ROOT}/usr/share/doc/coree/NOTICE"
install -Dm644 "${REPO_ROOT}/README.md" "${PKG_ROOT}/usr/share/doc/coree/README.md"
install -Dm644 "${REPO_ROOT}/CHANGELOG.md" "${PKG_ROOT}/usr/share/doc/coree/CHANGELOG.md"

cat > "${PKG_ROOT}/usr/share/doc/coree/copyright" <<'EOF'
Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: coree
Source: https://github.com/ahrrfy/coree

Files: *
Copyright: 2026 coree contributors
License: MIT or Apache-2.0
 See /usr/share/doc/coree/LICENSE-MIT and /usr/share/doc/coree/LICENSE-APACHE
 for full license texts, and /usr/share/doc/coree/NOTICE for trademark notices.
EOF

INSTALLED_SIZE="$(du -sk "${PKG_ROOT}/usr" | awk '{print $1}')"

cat > "${PKG_ROOT}/DEBIAN/control" <<EOF
Package: coree
Version: ${VERSION}
Section: graphics
Priority: optional
Architecture: ${ARCH}
Installed-Size: ${INSTALLED_SIZE}
Maintainer: coree contributors <noreply@github.com>
Homepage: https://github.com/ahrrfy/coree
Depends: libc6, libgcc-s1, libgl1 | libegl1, libx11-6 | libwayland-client0, shared-mime-info, desktop-file-utils
Description: Clean-room offline vector graphics, laser production & print studio
 coree is an independent open-source (MIT OR Apache-2.0) pure-Rust desktop
 vector illustration, multipage layout, integrated raster studio, laser
 cutting/engraving plugin, and commercial print-production suite.
 .
 Includes native CDR (RIFF v4-v13, ZIP X4-X5, ZCF X6-2025), CDT, CMX, CPT,
 PDF/AI, EPS, DXF, HPGL PLT, PSD, and SVG support, ICC v2/v4 CMYK soft-proofing,
 Arabic RTL/Kashida typography, and GNOME/freedesktop thumbnail integration.
EOF

cat > "${PKG_ROOT}/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
exit 0
EOF
chmod 0755 "${PKG_ROOT}/DEBIAN/postinst"

cat > "${PKG_ROOT}/DEBIAN/postrm" <<'EOF'
#!/bin/sh
set -e
if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database /usr/share/mime || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database /usr/share/applications || true
fi
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
exit 0
EOF
chmod 0755 "${PKG_ROOT}/DEBIAN/postrm"

mkdir -p "${OUT_DIR}"
OUT_DEB="${OUT_DIR}/coree_${VERSION}_${ARCH}.deb"
"${DPKG_DEB}" --build --root-owner-group "${PKG_ROOT}" "${OUT_DEB}"

echo "[coree] Built Debian package: ${OUT_DEB}"
