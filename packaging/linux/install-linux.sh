#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
# Zero-sudo local Linux installer for coree.
# Installs binary, app + MIME-type icons, Desktop Entry, shared-mime-info rules,
# GNOME/freedesktop thumbnailer, and xdg-mime default handlers into $HOME/.local.

set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd -- "${SCRIPT_DIR}/../.." && pwd)"

if [[ -d "${HOME}/.cargo/bin" ]]; then
    export PATH="${HOME}/.cargo/bin:${PATH}"
fi

REBUILD=0
CUSTOM_BIN=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --rebuild)
            REBUILD=1
            shift
            ;;
        --bin)
            if [[ $# -lt 2 ]]; then
                echo "Error: --bin requires a path argument" >&2
                exit 2
            fi
            CUSTOM_BIN="$2"
            shift 2
            ;;
        -h|--help)
            echo "Usage: $0 [--rebuild] [--bin /path/to/coree]"
            echo "Installs coree locally into \$HOME/.local without sudo."
            exit 0
            ;;
        *)
            echo "Unknown option: $1" >&2
            exit 2
            ;;
    esac
done

if [[ -n "${CUSTOM_BIN}" ]]; then
    RELEASE_BIN="${CUSTOM_BIN}"
elif [[ -x "${SCRIPT_DIR}/../../coree" && ! -f "${REPO_ROOT}/Cargo.toml" ]]; then
    # Extracted release tarball layout: <pkg>/coree + <pkg>/packaging/linux/install-linux.sh
    RELEASE_BIN="${SCRIPT_DIR}/../../coree"
else
    RELEASE_BIN="${REPO_ROOT}/target/release/coree"
    if [[ "${REBUILD}" -eq 1 || ! -x "${RELEASE_BIN}" ]]; then
        echo "[coree] Building release binary..."
        cargo build --release --manifest-path "${REPO_ROOT}/Cargo.toml"
    fi
fi

if [[ ! -x "${RELEASE_BIN}" ]]; then
    echo "[coree] Error: release binary not found or not executable at ${RELEASE_BIN}" >&2
    exit 1
fi

BIN_DIR="${HOME}/.local/bin"
ICON_APP_DIR="${HOME}/.local/share/icons/hicolor/scalable/apps"
ICON_MIME_DIR="${HOME}/.local/share/icons/hicolor/scalable/mimetypes"
APP_DIR="${HOME}/.local/share/applications"
MIME_DIR="${HOME}/.local/share/mime/packages"
THUMB_DIR="${HOME}/.local/share/thumbnailers"

install -Dm755 "${RELEASE_BIN}" "${BIN_DIR}/coree"
install -Dm644 "${SCRIPT_DIR}/coree.svg" "${ICON_APP_DIR}/coree.svg"

MIME_ICON_NAMES=(
    application-vnd.corel-draw
    application-x-cdr
    application-x-coree
    application-x-cdt
    application-x-cmx
    application-x-cpt
)
for mime_icon in "${MIME_ICON_NAMES[@]}"; do
    install -Dm644 "${SCRIPT_DIR}/coree.svg" "${ICON_MIME_DIR}/${mime_icon}.svg"
done

install -Dm644 "${SCRIPT_DIR}/coree-mime.xml" "${MIME_DIR}/coree-mime.xml"

mkdir -p "${APP_DIR}" "${THUMB_DIR}"

# Safely escape binary path per freedesktop Desktop Entry Exec quoting rules
# and substitute via awk ENVIRON so spaces, &, |, and backslashes in $HOME never break.
BIN_EXEC="${BIN_DIR}/coree"
BIN_EXEC_QUOTED="${BIN_EXEC//\\/\\\\}"
BIN_EXEC_QUOTED="${BIN_EXEC_QUOTED//\"/\\\"}"
BIN_EXEC_QUOTED="${BIN_EXEC_QUOTED//\$/\\\$}"
BIN_EXEC_QUOTED="${BIN_EXEC_QUOTED//\`/\\\`}"

COREE_DESKTOP_EXEC="Exec=\"${BIN_EXEC_QUOTED}\" %F" \
    awk 'BEGIN { rep = ENVIRON["COREE_DESKTOP_EXEC"] } /^Exec=/ { print rep; next } { print }' \
    "${SCRIPT_DIR}/coree.desktop" > "${APP_DIR}/coree.desktop"
chmod 0644 "${APP_DIR}/coree.desktop"

COREE_THUMB_TRY="TryExec=${BIN_EXEC}" \
COREE_THUMB_EXEC="Exec=\"${BIN_EXEC_QUOTED}\" thumbnail %i %o --size %s" \
    awk 'BEGIN { t = ENVIRON["COREE_THUMB_TRY"]; e = ENVIRON["COREE_THUMB_EXEC"] }
         /^TryExec=/ { print t; next }
         /^Exec=/ { print e; next }
         { print }' \
    "${SCRIPT_DIR}/coree.thumbnailer" > "${THUMB_DIR}/coree.thumbnailer"
chmod 0644 "${THUMB_DIR}/coree.thumbnailer"

if command -v update-mime-database >/dev/null 2>&1; then
    update-mime-database "${HOME}/.local/share/mime" || true
fi

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "${APP_DIR}" || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -f -t "${HOME}/.local/share/icons/hicolor" >/dev/null 2>&1 || true
fi

if command -v xdg-mime >/dev/null 2>&1; then
    mkdir -p "${XDG_CONFIG_HOME:-${HOME}/.config}"
    xdg-mime default coree.desktop \
        application/vnd.corel-draw \
        application/x-cdr \
        application/x-coree \
        application/x-cdt \
        application/x-cmx \
        application/x-cpt || true
fi

echo "[coree] Installed successfully:"
echo "  Binary:      ${BIN_DIR}/coree"
echo "  App Icon:    ${ICON_APP_DIR}/coree.svg"
echo "  MIME Icons:  ${ICON_MIME_DIR}/ (${#MIME_ICON_NAMES[@]} types)"
echo "  Desktop:     ${APP_DIR}/coree.desktop"
echo "  MIME Rules:  ${MIME_DIR}/coree-mime.xml"
echo "  Thumbnailer: ${THUMB_DIR}/coree.thumbnailer"

case ":${PATH}:" in
    *":${BIN_DIR}:"*)
        ;;
    *)
        echo "[coree] Tip: add ${BIN_DIR} to PATH (e.g. export PATH=\"\$HOME/.local/bin:\$PATH\")."
        ;;
esac
