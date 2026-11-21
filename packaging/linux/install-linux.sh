#!/usr/bin/env bash
# SPDX-License-Identifier: MPL-2.0
# Zero-sudo local Linux installer for coree.
# Installs binary, icon, Desktop Entry, and MIME types into $HOME/.local.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
fi

REBUILD=0
for arg in "$@"; do
    case "${arg}" in
        --rebuild)
            REBUILD=1
            ;;
        -h|--help)
            echo "Usage: $0 [--rebuild]"
            echo "Installs coree locally into \$HOME/.local without sudo."
            exit 0
            ;;
        *)
            echo "Unknown option: ${arg}" >&2
            exit 2
            ;;
    esac
done

RELEASE_BIN="${REPO_ROOT}/target/release/coree"
if [[ "${REBUILD}" -eq 1 || ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Building release binary..."
    cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
fi

if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Error: release binary not found at ${RELEASE_BIN}" >&2
    exit 1
fi

BIN_DIR="${HOME}/.local/bin"
ICON_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
APP_DIR="${HOME}/.local/share/applications"
MIME_DIR="${HOME}/.local/share/mime/packages"

install -Dm755 "${RELEASE_BIN}" "${BIN_DIR}/coree"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${ICON_DIR}/coree.svg"
install -Dm644 "${SCRIPT_DIR}/coree-mime.xml" "${MIME_DIR}/coree-mime.xml"

mkdir -p "${APP_DIR}"
sed "s|^Exec=.*|Exec=${BIN_DIR}/coree %F|" \
    "${SCRIPT_DIR}/coree.desktop" > "${APP_DIR}/coree.desktop"
chmod 0644 "${APP_DIR}/coree.desktop"

if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database "${HOME}/.local/share/mime" || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "[coree] Installed successfully:"
echo "  Binary:  ${BIN_DIR}/coree"
echo "  Icon:    ${ICON_DIR}/coree.svg"
echo "  Desktop: ${APP_DIR}/coree.desktop"
echo "  MIME:    ${MIME_DIR}/coree-mime.xml"

case ":${PATH}:" in
    *":${BIN_DIR}:"*)
        ;;
    *)
        echo "[coree] Tip: add ${BIN_DIR} to PATH (e.g. export PATH=\"\$HOME/.local/bin:\$PATH\")."
        ;;
esac
