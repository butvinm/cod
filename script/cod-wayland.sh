#!/usr/bin/env sh
# Starts Cod as a native Wayland client with the off-screen web view (issue #16).
# Usage: cod-wayland.sh [url] [folder]
# Unlike cod-xwayland.sh this keeps WAYLAND_DISPLAY: the browser draws into memory and GPUI paints its frames as images, so no X11 window is involved.
# Needs the cod-browser helper next to the cod executable (or COD_BROWSER_BIN) and the CEF runtime folder in COD_CEF_DIR (libcef.so, the .pak files, icudtl.dat, locales/).
# COD_CEF_DIR defaults to the cef/ folder next to this script when there is one.
# The release tarball does not carry CEF. It has a cef-manifest file (version, url, sha256) and this script downloads the CEF archive once into <data folder>/cef-<version>/.
# The archive is checked against the sha256 from the manifest, and the unpacked files against the SHA256SUMS file inside it. After that Cod starts without a network.
# The data folder is ~/.local/share/cod-preview, or COD_PREVIEW_DIR when set.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
data="${COD_PREVIEW_DIR:-$HOME/.local/share/cod-preview}"

manifest_value() {
    sed -n "s/^$1=//p" "$here/cef-manifest" | head -n 1
}

fail() {
    echo "cod-wayland.sh: $*" >&2
    exit 1
}

# Downloads, checks and unpacks the CEF runtime into "$1" (the final folder). Needs the manifest values in $version, $url and $sha256.
download_cef() {
    dir=$1
    for tool in tar xz sha256sum; do
        command -v "$tool" > /dev/null 2>&1 || fail "'$tool' is needed to unpack the browser runtime but was not found"
    done
    if ! command -v curl > /dev/null 2>&1 && ! command -v wget > /dev/null 2>&1; then
        fail "curl or wget is needed to download the browser runtime but neither was found"
    fi
    part="$data/cef-$version.part"
    rm -rf "$part"
    mkdir -p "$part"
    echo "Cod needs its browser runtime (CEF $version, about 100 MB download). Downloading it once to $dir" >&2
    echo "  from $url" >&2
    if command -v curl > /dev/null 2>&1; then
        curl -fL --retry 3 --progress-bar -o "$part/cef.tar.xz" "$url" || { rm -rf "$part"; fail "the download failed: check the network, or set COD_CEF_DIR to a folder with libcef.so"; }
    else
        wget -O "$part/cef.tar.xz" "$url" || { rm -rf "$part"; fail "the download failed: check the network, or set COD_CEF_DIR to a folder with libcef.so"; }
    fi
    echo "Checking the download..." >&2
    actual=$(sha256sum "$part/cef.tar.xz" | cut -d ' ' -f 1)
    if [ "$actual" != "$sha256" ]; then
        rm -rf "$part"
        fail "the downloaded archive has sha256 $actual but cef-manifest says $sha256: nothing was installed"
    fi
    echo "Unpacking..." >&2
    xz -dc "$part/cef.tar.xz" | tar -C "$part" -xf - || { rm -rf "$part"; fail "could not unpack the download"; }
    rm -f "$part/cef.tar.xz"
    (cd "$part/cef" && sha256sum -c --quiet SHA256SUMS) || { rm -rf "$part"; fail "a file of the unpacked browser runtime does not match its checksum: nothing was installed"; }
    test -f "$part/cef/libcef.so" || { rm -rf "$part"; fail "the archive has no libcef.so"; }
    rm -rf "$dir"
    mv "$part/cef" "$dir"
    rm -rf "$part"
    # Older CEF versions are not needed any more.
    for old in "$data"/cef-*; do
        [ -d "$old" ] && [ "$old" != "$dir" ] && rm -rf "$old"
    done
    echo "The browser runtime is installed. Cod starts without a network from now on." >&2
}

if [ -z "${WAYLAND_DISPLAY:-}" ]; then
    echo "WAYLAND_DISPLAY is not set: this script needs a Wayland session" >&2
    exit 1
fi
# A folder from the environment wins, then a cef/ folder next to this script (older tarballs shipped one).
if [ -z "${COD_CEF_DIR:-}" ] && [ -f "$here/cef/libcef.so" ]; then
    COD_CEF_DIR="$here/cef"
fi
# Otherwise the runtime named by cef-manifest lives in the data folder, and is downloaded when it is not there yet.
if [ -z "${COD_CEF_DIR:-}" ] && [ -f "$here/cef-manifest" ]; then
    version=$(manifest_value version)
    url=$(manifest_value url)
    sha256=$(manifest_value sha256)
    if [ -z "$version" ] || [ -z "$url" ] || [ -z "$sha256" ]; then
        fail "cef-manifest is incomplete"
    fi
    # The version goes into file names, so only digits separated by single dots are accepted.
    case $version in
        '' | *[!0-9.]* | .* | *. | *..*) fail "cef-manifest has an invalid version: $version" ;;
    esac
    case $sha256 in
        *[!0-9a-f]*) fail "cef-manifest has an invalid sha256" ;;
    esac
    [ "${#sha256}" -eq 64 ] || fail "cef-manifest has an invalid sha256"
    case $url in
        https://* | file://*) ;;
        *) fail "cef-manifest has an unsupported url: $url" ;;
    esac
    COD_CEF_DIR="$data/cef-$version"
    # A link in place of the folder could point anywhere, so it is never used or replaced.
    if [ -L "$COD_CEF_DIR" ] || [ -L "$data/cef-$version.part" ]; then
        fail "$COD_CEF_DIR is a symbolic link: remove it and start again"
    fi
    if [ ! -f "$COD_CEF_DIR/libcef.so" ]; then
        mkdir -p "$data"
        download_cef "$COD_CEF_DIR"
    fi
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

logs="$data/logs"
mkdir -p "$logs"
RUST_BACKTRACE=1
export RUST_BACKTRACE
echo "=== cod start $(date '+%Y-%m-%d %H:%M:%S') ===" >> "$logs/cod-stderr.log"

exec "$here/cod" --user-data-dir "$data" "$@" 2>> "$logs/cod-stderr.log"
