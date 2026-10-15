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

# Succeeds when every member of the tar.xz archive "$1" is a plain file or folder inside cef/.
# Absolute names, .. components, links of any kind and special files are refused before anything is extracted.
check_archive() {
    names=$(xz -dc "$1" | tar -tf -) || return 1
    types=$(xz -dc "$1" | tar -tvf -) || return 1
    [ -n "$names" ] || return 1
    printf '%s\n' "$names" | while IFS= read -r name; do
        case $name in
            cef | cef/ | cef/*) ;;
            *) exit 1 ;;
        esac
        case /$name/ in
            */../* | //*) exit 1 ;;
        esac
    done || return 1
    # The first character of a verbose listing is the type: '-' file, 'd' folder, 'l' symbolic link, 'h' hard link and so on.
    # bsdtar lists hard links as files with "link to" in the line.
    printf '%s\n' "$types" | while IFS= read -r line; do
        case $line in
            -* | d*) ;;
            *) exit 1 ;;
        esac
        case $line in
            *" link to "* | *" -> "*) exit 1 ;;
        esac
    done
}

# Succeeds when "$1" is a real folder (not a link) whose SHA256SUMS lists only plain relative paths inside it.
check_unpacked() {
    [ -d "$1" ] && [ ! -L "$1" ] || return 1
    [ -f "$1/SHA256SUMS" ] && [ ! -L "$1/SHA256SUMS" ] || return 1
    [ -f "$1/libcef.so" ] && [ ! -L "$1/libcef.so" ] || return 1
    if grep -vE '^[0-9a-f]{64}  \./[A-Za-z0-9_][A-Za-z0-9_./-]*$' "$1/SHA256SUMS" > /dev/null; then
        return 1
    fi
    if grep -F '..' "$1/SHA256SUMS" > /dev/null; then
        return 1
    fi
    return 0
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
    command -v flock > /dev/null 2>&1 || fail "'flock' (util-linux) is needed to install the browser runtime safely but was not found"
    mkdir -p "$data" || fail "cannot create $data"
    part=
    cleanup() {
        [ -z "$part" ] || rm -rf "$part"
    }
    trap cleanup EXIT
    trap 'exit 1' INT TERM HUP
    # One install at a time. The kernel releases the lock when this process ends, however it ends, so there is no stale lock.
    # The lock file is never removed. File descriptor 9 is closed again before Cod is started.
    exec 9> "$data/.cef-install.lock" || fail "cannot open the install lock in $data"
    if ! flock -n 9; then
        echo "Another Cod start is installing the browser runtime: waiting for it..." >&2
        flock 9 || fail "cannot take the install lock"
    fi
    # Another start may have finished the install while this one waited.
    if [ -f "$dir/libcef.so" ]; then
        exec 9>&-
        return 0
    fi
    # Staging folders of killed starts are only removed when old, since a live install may own a newer one.
    find "$data" -maxdepth 1 -name '.cef-stage.*' -mmin +1440 -exec rm -rf {} + 2> /dev/null || true
    part=$(mktemp -d "$data/.cef-stage.XXXXXX") || fail "cannot create a staging folder in $data"
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
    check_archive "$part/cef.tar.xz" || { rm -rf "$part"; fail "the archive has unsafe contents: nothing was installed"; }
    xz -dc "$part/cef.tar.xz" | tar -C "$part" -xf - || { rm -rf "$part"; fail "could not unpack the download"; }
    rm -f "$part/cef.tar.xz"
    check_unpacked "$part/cef" || { rm -rf "$part"; fail "the unpacked browser runtime is not what was expected: nothing was installed"; }
    (cd "$part/cef" && sha256sum -c --quiet SHA256SUMS) || { rm -rf "$part"; fail "a file of the unpacked browser runtime does not match its checksum: nothing was installed"; }
    test -f "$part/cef/libcef.so" || { rm -rf "$part"; fail "the archive has no libcef.so"; }
    # Only a damaged folder without libcef.so can be here (the caller checked), and it is never a link.
    if [ -e "$dir" ] && [ ! -L "$dir" ]; then
        rm -rf "$dir" || fail "cannot remove the damaged folder $dir"
    fi
    mv "$part/cef" "$dir" || fail "cannot move the browser runtime to $dir: nothing was installed"
    # Success is only reported when the final folder is really there. Older versions are kept (see the README).
    if [ ! -f "$dir/libcef.so" ] || [ -L "$dir" ]; then
        fail "the browser runtime is not in $dir after the install: something is wrong with that folder"
    fi
    rm -rf "$part" || fail "cannot remove the staging folder $part"
    part=
    exec 9>&-
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
    if [ -L "$COD_CEF_DIR" ]; then
        fail "$COD_CEF_DIR is a symbolic link: remove it and start again"
    fi
    if [ ! -f "$COD_CEF_DIR/libcef.so" ]; then
        # Not in a condition, so that set -e stays active inside; every step also checks its own result.
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
