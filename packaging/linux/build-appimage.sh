#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Builds a portable Linux AppDir (and .AppImage if appimagetool is installed) for coree.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"
APPDIR="${1:-${SCRIPT_DIR}/AppDir}"
ARCH="$(uname -m)"

if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
fi

RELEASE_BIN="${REPO_ROOT}/target/release/coree"
if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Building release binary..."
    cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

rm -rf "${APPDIR}"
mkdir -p \
    "${APPDIR}/usr/bin" \
    "${APPDIR}/usr/share/applications" \
    "${APPDIR}/usr/share/icons/hicolor/scalable/apps" \
    "${APPDIR}/usr/share/mime/packages"

install -Dm755 "${RELEASE_BIN}" "${APPDIR}/usr/bin/coree"
install -Dm644 "${SCRIPT_DIR}/coree.desktop" "${APPDIR}/coree.desktop"
install -Dm644 "${SCRIPT_DIR}/coree.desktop" "${APPDIR}/usr/share/applications/coree.desktop"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${APPDIR}/coree.svg"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${APPDIR}/usr/share/icons/hicolor/scalable/apps/coree.svg"
install -Dm644 "${SCRIPT_DIR}/coree-mime.xml" "${APPDIR}/usr/share/mime/packages/coree-mime.xml"

cat > "${APPDIR}/AppRun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec "${HERE}/usr/bin/coree" "$@"
EOF
chmod 0755 "${APPDIR}/AppRun"

echo "[coree] Portable AppDir staged at ${APPDIR}"

if command -v appimagetool >/dev/null 2>&1; then
    OUT_IMAGE="${REPO_ROOT}/target/release/coree-${ARCH}.AppImage"
    ARCH="${ARCH}" appimagetool "${APPDIR}" "${OUT_IMAGE}"
    echo "[coree] Built AppImage: ${OUT_IMAGE}"
else
    echo "[coree] appimagetool not found; run: ARCH=${ARCH} appimagetool \"${APPDIR}\" \"${REPO_ROOT}/target/release/coree-${ARCH}.AppImage\""
fi
