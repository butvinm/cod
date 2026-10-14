#!/usr/bin/env sh
# Starts Cod as a native Wayland client with the off-screen web view (issue #16).
# Usage: cod-wayland.sh [url] [folder]
# Unlike cod-xwayland.sh this keeps WAYLAND_DISPLAY: the browser draws into memory and GPUI paints its frames as images, so no X11 window is involved.
# Needs a build with `--features offscreen-web-view` and the CEF runtime folder in COD_CEF_DIR (libcef.so, the .pak files, icudtl.dat, locales/).
set -eu

here=$(cd "$(dirname "$0")" && pwd)

if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "WAYLAND_DISPLAY is not set: this script needs a Wayland session" >&2
    exit 1
fi
if [ -z "${COD_CEF_DIR:-}" ]; then
    echo "COD_CEF_DIR is not set: point it at the folder with libcef.so" >&2
    exit 1
fi

COD_WEBVIEW_BACKEND=offscreen
export COD_WEBVIEW_BACKEND
LD_LIBRARY_PATH="$COD_CEF_DIR${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
export LD_LIBRARY_PATH

if [ "$#" -gt 0 ]; then
    COD_SPIKE_URL=$1
    shift
else
    COD_SPIKE_URL=https://en.wikipedia.org/wiki/Levenshtein_distance
fi
export COD_SPIKE_URL

logs="$HOME/.local/share/cod-preview/logs"
mkdir -p "$logs"
RUST_BACKTRACE=1
export RUST_BACKTRACE
echo "=== cod start $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$logs/cod-stderr.log"

exec "$here/cod" --user-data-dir "$HOME/.local/share/cod-preview" "$@" 2>> "$logs/cod-stderr.log"
