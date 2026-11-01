#!/usr/bin/env bash
# Installs the per-user launcher + icon so the desktop shows the OpenAY Mic
# logo in the dock/app grid: window icons baked into the binary are ignored
# by GNOME's dock — it matches running windows to a .desktop entry via
# StartupWMClass, and a loose ELF binary always gets the generic icon.
# Per-user install, no root. Idempotent: re-run after moving the folder.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUI="$HERE/openay-gui"
ICON_SRC="$HERE/openay-mic.png"
[ -f "$GUI" ] || { echo "error: $GUI not found (run from the dist folder)" >&2; exit 1; }
[ -f "$ICON_SRC" ] || { echo "error: $ICON_SRC not found" >&2; exit 1; }

DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
ICON_DIR="$DATA/icons/hicolor/256x256/apps"
APPS_DIR="$DATA/applications"
mkdir -p "$ICON_DIR" "$APPS_DIR"
cp -f "$ICON_SRC" "$ICON_DIR/openay-mic.png"

cat > "$APPS_DIR/openay-gui.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=OpenAY Mic
Comment=Virtual microphone console (The Chain)
Exec=$GUI
Icon=openay-mic
Terminal=false
Categories=AudioVideo;Audio;
StartupWMClass=openay-gui
DESKTOP

# GTK flags newly added launchers as untrusted until approved.
gio set "$APPS_DIR/openay-gui.desktop" metadata::trusted true 2>/dev/null || true
update-desktop-database "$APPS_DIR" 2>/dev/null || true
echo "installed: 'OpenAY Mic' in the app grid and dock (icon: openay-mic)"
echo "launch via the app grid once so the dock pins the right window; on"
echo "GNOME a shell reload may be needed (Alt-F2, 'rt', Enter)."
