#!/usr/bin/env bash
# Install tui-launcher as a Wayland login session: a greeter (greetd/SDDM/…)
# will offer "XMB Launcher", which runs the launcher fullscreen inside the
# `cage` kiosk compositor. Requires root to write into the sessions dir.
set -euo pipefail

session_dir=/usr/share/wayland-sessions
here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)
src="$here/dist/tui-launcher.desktop"

if ! command -v cage >/dev/null 2>&1; then
  echo "cage is not installed — it hosts the launcher as a kiosk session." >&2
  echo "  Arch:  sudo pacman -S cage" >&2
  exit 1
fi
if ! command -v tui-launcher >/dev/null 2>&1; then
  echo "warning: 'tui-launcher' is not on PATH; install it with 'cargo install --path .'" >&2
fi

echo "Installing $src -> $session_dir/ (needs sudo)"
sudo install -Dm644 "$src" "$session_dir/tui-launcher.desktop"
echo "Done. Pick 'XMB Launcher' at your display manager's session chooser."
echo "To try it now without logging out:  cage -- tui-launcher --kiosk"
