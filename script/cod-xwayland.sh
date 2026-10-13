#!/usr/bin/env sh
# Starts the Cod preview from its unpacked release folder.
# Usage: cod-xwayland.sh [url] [folder]
# The web view needs X11 for now, so this unsets WAYLAND_DISPLAY and GPUI uses X11 (XWayland on a Wayland desktop).
# Cod keeps its settings and data in its own folder, so it never touches the user's own Zed settings.
set -eu

here=$(cd "$(dirname "$0")" && pwd)

unset WAYLAND_DISPLAY

if [ "$#" -gt 0 ]; then
    COD_SPIKE_URL=$1
    shift
else
    COD_SPIKE_URL=https://salsa.debian.org/debian/devscripts/-/merge_requests/463
fi
export COD_SPIKE_URL

# Crashes must leave evidence: stderr (GTK, X errors, WebKit, panics) goes to a file next to Zed's own logs, with Rust backtraces on.
logs="$HOME/.local/share/cod-preview/logs"
mkdir -p "$logs"
RUST_BACKTRACE=1
export RUST_BACKTRACE
echo "=== cod start $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$logs/cod-stderr.log"

exec "$here/cod" --user-data-dir "$HOME/.local/share/cod-preview" "$@" 2>> "$logs/cod-stderr.log"
