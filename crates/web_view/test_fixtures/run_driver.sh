#!/usr/bin/env bash
# Runs the off-screen web view driver (issue #16) under a headless Wayland compositor and exits with its status.
# Status 0 means every acceptance check reported and passed; 1 means a check failed or did not report; 124 means the run timed out.
# Usage: run_driver.sh <zed binary> <folder to open>
# Needs: sway (wlroots headless backend; Weston headless has no wl_seat, which GPUI requires), dbus-run-session, lavapipe (mesa-vulkan-drivers), the cod-browser helper next to the zed binary, COD_CEF_DIR (folder with libcef.so), grim, which the AC1 and AC6 checks use to capture the screen and look for the fixture's banner pixels (COD_SPIKE_SHOTS=<folder> also saves a screenshot per check).
set -uo pipefail

zed=${1:?usage: run_driver.sh <zed binary> <folder>}
folder=${2:?usage: run_driver.sh <zed binary> <folder>}
here=$(cd "$(dirname "$0")" && pwd)
: "${COD_CEF_DIR:?COD_CEF_DIR must name the folder with libcef.so}"

work=$(mktemp -d)
runtime="$work/runtime"
data="$work/data"
mkdir -p "$runtime" "$data/config"
chmod 700 "$runtime"
echo '{ "session": { "trust_all_worktrees": true } }' > "$data/config/settings.json"

export XDG_RUNTIME_DIR="$runtime"
WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman \
    sway -c "$here/sway.conf" > "$work/sway.log" 2>&1 &
sway_pid=$!
cleanup() {
    kill "$sway_pid" 2>/dev/null
    rm -rf "$work"
}
trap cleanup EXIT

for _ in $(seq 1 50); do
    socket=$(ls "$runtime" 2>/dev/null | grep -E '^wayland-[0-9]+$' | head -1)
    [ -n "$socket" ] && break
    sleep 0.1
done
[ -n "${socket:-}" ] || { echo "the headless compositor did not start" >&2; exit 1; }

unset DISPLAY
export WAYLAND_DISPLAY="$socket"
export XDG_SESSION_TYPE=wayland
export ZED_STATELESS=1 ZED_ALLOW_EMULATED_GPU=1 RUST_LOG=${RUST_LOG:-info}
export COD_WEBVIEW_BACKEND=offscreen
export COD_SPIKE_DRIVE=offscreen
export COD_SPIKE_URL="file://$here/offscreen.html"

timeout "${DRIVER_TIMEOUT:-180}" dbus-run-session -- "$zed" --user-data-dir "$data" "$folder"
status=$?
grep 'driver:' "$data/logs/Zed.log" | grep -v screenshot | cut -d' ' -f4-
echo "driver exit status $status"
exit "$status"
