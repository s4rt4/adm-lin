#!/usr/bin/env bash
# Pemasangan lokal-pengguna ADM (Linux/Fedora) tanpa root.
# Build rilis → salin binary ke ~/.local/bin, desktop entry, dan ikon hicolor.
#   ./packaging/linux/install.sh           # build + install
#   ./packaging/linux/install.sh --uninstall
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
BIN_DIR="${XDG_BIN_HOME:-$HOME/.local/bin}"
APP_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICON_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor/scalable/apps"

uninstall() {
    rm -f "$BIN_DIR/adm-egui" "$BIN_DIR/adm-bridge"
    rm -f "$APP_DIR/adm-egui.desktop"
    rm -f "$ICON_DIR/adm.svg"
    "$BIN_DIR/adm-bridge" unregister 2>/dev/null || true
    update-desktop-database "$APP_DIR" 2>/dev/null || true
    echo "ADM dihapus dari instalasi lokal pengguna."
}

if [[ "${1:-}" == "--uninstall" ]]; then
    uninstall
    exit 0
fi

echo ">> build rilis (adm-egui + adm-bridge)…"
cargo build --release -p adm-egui -p adm-bridge --manifest-path "$REPO/Cargo.toml"

mkdir -p "$BIN_DIR" "$APP_DIR" "$ICON_DIR"
install -m755 "$REPO/target/release/adm-egui" "$BIN_DIR/adm-egui"
install -m755 "$REPO/target/release/adm-bridge" "$BIN_DIR/adm-bridge"
install -m644 "$REPO/packaging/linux/adm-egui.desktop" "$APP_DIR/adm-egui.desktop"
install -m644 "$REPO/crates/adm-egui/assets/logo.svg" "$ICON_DIR/adm.svg"
update-desktop-database "$APP_DIR" 2>/dev/null || true

echo ">> selesai. Pastikan $BIN_DIR ada di PATH."
echo ">> daftarkan host native-messaging browser dengan:"
echo "     adm-bridge register <chrome/edge-extension-id> [firefox-extension-id]"
