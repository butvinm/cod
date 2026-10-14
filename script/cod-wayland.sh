#!/usr/bin/env sh
# Starts Cod as a native Wayland client with the off-screen web view (issue #16).
# Usage: cod-wayland.sh [url] [folder]
# Unlike cod-xwayland.sh this keeps WAYLAND_DISPLAY: the browser draws into memory and GPUI paints its frames as images, so no X11 window is involved.
# Needs the cod-browser helper next to the cod executable (or COD_BROWSER_BIN) and the CEF runtime folder in COD_CEF_DIR (libcef.so, the .pak files, icudtl.dat, locales/).
# COD_CEF_DIR defaults to the cef/ folder next to this script when the release tarball ships one.
set -eu

here=$(cd "$(dirname "$0")" && pwd)

if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "WAYLAND_DISPLAY is not set: this script needs a Wayland session" >&2
    exit 1
fi
# The release tarball ships the CEF runtime in cef/ next to this script.
if [ -z "${COD_CEF_DIR:-}" ] && [ -f "$here/cef/libcef.so" ]; then
    COD_CEF_DIR="$here/cef"
fi
if [ -z "${COD_CEF_DIR:-}" ]; then
    echo "COD_CEF_DIR is not set: point it at the folder with libcef.so" >&2
    exit 1
fi
export COD_CEF_DIR

COD_WEBVIEW_BACKEND=offscreen
export COD_WEBVIEW_BACKEND

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
