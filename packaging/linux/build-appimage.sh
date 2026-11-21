#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Builds a portable Linux AppDir (and .AppImage when appimagetool is available) for coree.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
APPDIR="${1:-${SCRIPT_DIR}/AppDir}"
OUT_IMAGE="${2:-}"
ARCH="$(uname -m)"

if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
fi

RELEASE_BIN="${COREE_BIN:-${REPO_ROOT}/target/release/coree}"
if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Building release binary..."
    cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Error: release binary not found or not executable at ${RELEASE_BIN}" >&2
    exit 1
fi

rm -rf "${APPDIR}"
mkdir -p \
    "${APPDIR}/usr/bin" \
    "${APPDIR}/usr/share/applications" \
    "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
    "${APPDIR}/usr/share/icons/hicolor/scalable/mimetypes" \
    "${APPDIR}/usr/share/mime/packages" \
    "${APPDIR}/usr/share/thumbnailers" \
    "${APPDIR}/usr/share/metainfo" \
    "${APPDIR}/usr/share/licenses/coree"

install -Dm755 "${RELEASE_BIN}" "${APPDIR}/usr/bin/coree"
install -Dm644 "${SCRIPT_DIR}/coree.desktop" "${APPDIR}/coree.desktop"
install -Dm644 "${SCRIPT_DIR}/coree.desktop" "${APPDIR}/usr/share/applications/coree.desktop"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${APPDIR}/coree.svg"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${APPDIR}/.DirIcon"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/coree.svg"

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
        "${APPDIR}/usr/share/icons/hicolor/scalable/mimetypes/${mime_icon}.svg"
done

install -Dm644 "${SCRIPT_DIR}/coree-mime.xml" "${APPDIR}/usr/share/mime/packages/coree-mime.xml"
install -Dm644 "${SCRIPT_DIR}/coree.thumbnailer" "${APPDIR}/usr/share/thumbnailers/coree.thumbnailer"
install -Dm644 "${SCRIPT_DIR}/flatpak/io.github.ahrrfy.coree.metainfo.xml" \
    "${APPDIR}/usr/share/metainfo/io.github.ahrrfy.coree.metainfo.xml"

install -Dm644 "${REPO_ROOT}/LICENSE-MIT" "${APPDIR}/usr/share/licenses/coree/LICENSE-MIT"
install -Dm644 "${REPO_ROOT}/LICENSE-APACHE" "${APPDIR}/usr/share/licenses/coree/LICENSE-APACHE"
install -Dm644 "${REPO_ROOT}/NOTICE" "${APPDIR}/usr/share/licenses/coree/NOTICE"

cat > "${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
export XDG_DATA_DIRS="${HERE}/usr/share:${XDG_DATA_DIRS:-/usr/local/share:/usr/share}"
exec "${HERE}/usr/bin/coree" "$@"
EOF
chmod 0755 "${APPDIR}/AppRun"

echo "[coree] Portable AppDir staged at ${APPDIR}"

DEFAULT_OUT="${REPO_ROOT}/dist/coree-${ARCH}.AppImage"
TARGET_IMAGE="${OUT_IMAGE:-${DEFAULT_OUT}}"

if command -v appimagetool >/dev/null 2>&1; then
    mkdir -p "$(dirname -- "${TARGET_IMAGE}")"
    ARCH="${ARCH}" appimagetool --no-appstream "${APPDIR}" "${TARGET_IMAGE}" \
        || ARCH="${ARCH}" appimagetool "${APPDIR}" "${TARGET_IMAGE}"
    echo "[coree] Built AppImage: ${TARGET_IMAGE}"
else
    echo "[coree] appimagetool not found; run: ARCH=${ARCH} appimagetool \"${APPDIR}\" \"${TARGET_IMAGE}\""
fi
