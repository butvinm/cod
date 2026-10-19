#!/usr/bin/env sh
# Installs Cod's launcher and icons for the current user (issue #57). It ships at the top of the Linux tarball.
# Run it from the unpacked folder, for example: ./cod/install-launcher.sh
# It copies share/icons and share/applications into $XDG_DATA_HOME (default ~/.local/share),
# points Exec and TryExec of the launcher at the absolute path of cod-wayland.sh next to this script,
# and refreshes the desktop and icon caches when update-desktop-database and gtk-update-icon-cache are installed.
# Run it again after moving the folder.
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
launcher=$here/cod-wayland.sh
desktop=dev.zed.Zed.desktop
data=${XDG_DATA_HOME:-$HOME/.local/share}

fail() {
    echo "install-launcher.sh: $1" >&2
    exit 1
}

[ -x "$launcher" ] || fail "$launcher is missing or not executable"
[ -f "$here/share/applications/$desktop" ] || fail "$here/share/applications/$desktop is missing"
[ -d "$here/share/icons/hicolor" ] || fail "$here/share/icons/hicolor is missing"

# Exec is quoted, so spaces are fine. These characters would need escapes in a desktop file, so they are refused.
case $launcher in
    *'"'* | *'`'* | *'$'* | *'\'* | *'%'* | *'
'*)
        fail "the folder path $here contains one of \" \` \$ \\ % or a line break; move the folder and run this again"
        ;;
esac

mkdir -p "$data/icons" "$data/applications"
cp -R "$here/share/icons/hicolor" "$data/icons/"

target=$data/applications/$desktop
tmp=$data/applications/.$desktop.tmp
while IFS= read -r line || [ -n "$line" ]; do
    case $line in
        Exec=*) printf 'Exec="%s"\n' "$launcher" ;;
        TryExec=*) printf 'TryExec=%s\n' "$launcher" ;;
        *) printf '%s\n' "$line" ;;
    esac
done < "$here/share/applications/$desktop" > "$tmp"
chmod 644 "$tmp"
mv "$tmp" "$target"

if command -v update-desktop-database > /dev/null 2>&1; then
    update-desktop-database -q "$data/applications" || true
fi
if command -v gtk-update-icon-cache > /dev/null 2>&1; then
    gtk-update-icon-cache -q -t -f "$data/icons/hicolor" || true
fi

echo "installed $target (Exec=\"$launcher\")"
echo "installed the Cod icons into $data/icons/hicolor"
