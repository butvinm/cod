#!/usr/bin/env sh
# Installs Cod's launcher and icons for the current user (issue #57). It ships at the top of the Linux tarball.
# Run it from the unpacked folder, for example: ./cod/install-launcher.sh
# It copies share/icons/hicolor/*/apps/cod.png and share/applications/dev.zed.Zed.desktop into $XDG_DATA_HOME (default ~/.local/share),
# points Exec and TryExec of the launcher at the absolute path of cod-wayland.sh next to this script,
# and refreshes the desktop and icon caches when update-desktop-database and gtk-update-icon-cache are installed.
# Run it again after moving the folder.
# Stock Zed's install.sh uses the same launcher file name. Cod's launcher carries the line X-Cod-Launcher=true.
# If the launcher file exists without that line, or a cod.png icon exists while no Cod launcher does,
# the script refuses before writing anything, so another app's files stay unchanged.
set -eu

here=$(cd "$(dirname "$0")" && pwd -P)
launcher=$here/cod-wayland.sh
desktop=dev.zed.Zed.desktop
marker=X-Cod-Launcher=true
data=${XDG_DATA_HOME:-$HOME/.local/share}
target=$data/applications/$desktop

fail() {
    echo "install-launcher.sh: $1" >&2
    exit 1
}

[ -x "$launcher" ] || fail "$launcher is missing or not executable"
[ -f "$here/share/applications/$desktop" ] || fail "$here/share/applications/$desktop is missing"
grep -qx "$marker" "$here/share/applications/$desktop" || fail "$here/share/applications/$desktop has no $marker line"
[ -d "$here/share/icons/hicolor" ] || fail "$here/share/icons/hicolor is missing"

# Exec is quoted, so spaces are fine. These characters would need escapes in a desktop file, so they are refused.
case $launcher in
    *'"'* | *'`'* | *'$'* | *'\'* | *'%'* | *'
'*)
        fail "the folder path $here contains one of \" \` \$ \\ % or a line break; move the folder and run this again"
        ;;
esac

# Every check that can refuse runs before the first write.
if [ -e "$target" ] || [ -L "$target" ]; then
    # An existing launcher is updated only when an earlier run of this script installed it.
    grep -qx "$marker" "$target" 2> /dev/null \
        || fail "$target exists and was not installed by Cod (no $marker line), probably a Zed launcher; nothing was changed. Move or delete it and run this again"
else
    # Without a Cod launcher, any cod.png in the icon folder belongs to something else.
    for icon in "$here"/share/icons/hicolor/*/apps/cod.png; do
        size=${icon%/apps/cod.png}
        size=${size##*/}
        existing=$data/icons/hicolor/$size/apps/cod.png
        if [ -e "$existing" ] || [ -L "$existing" ]; then
            fail "$existing exists and was not installed by Cod (there is no Cod launcher at $target); nothing was changed. Move or delete it and run this again"
        fi
    done
fi

for icon in "$here"/share/icons/hicolor/*/apps/cod.png; do
    size=${icon%/apps/cod.png}
    size=${size##*/}
    mkdir -p "$data/icons/hicolor/$size/apps"
    cp "$icon" "$data/icons/hicolor/$size/apps/cod.png"
done

mkdir -p "$data/applications"
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
