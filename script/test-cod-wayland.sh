#!/usr/bin/env sh
# Tests the CEF install part of cod-wayland.sh with file:// urls, in a throwaway folder. No network, no Cod binary.
# Usage: script/test-cod-wayland.sh
# Needs sh, tar, xz, sha256sum, head, mktemp. Prints PASS or FAIL per case and exits 1 if any case fails.
# It lives next to the script it tests because the repo has no shell test folder; CI does not run it (it is for whoever changes the script).
set -u

if ! command -v flock > /dev/null 2>&1; then
    echo "flock (util-linux) is not installed here: run this test on Linux" >&2
    exit 2
fi

src=$(cd "$(dirname "$0")" && pwd)
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
failed=0

# A fake release folder: the real start script, a fake cod that only exits, and a manifest written per case.
rel="$T/rel"
mkdir -p "$rel"
cp "$src/cod-wayland.sh" "$rel/cod-wayland.sh"
printf '#!/bin/sh\nexit 0\n' > "$rel/cod"
chmod +x "$rel/cod"

# Builds a good CEF folder in $1 with a SHA256SUMS file.
make_cef() {
    mkdir -p "$1/cef/locales"
    printf 'lib' > "$1/cef/libcef.so"
    printf 'icu' > "$1/cef/icudtl.dat"
    printf 'pak' > "$1/cef/locales/en-US.pak"
    (cd "$1/cef" && find . -type f ! -name SHA256SUMS | sort | xargs sha256sum > SHA256SUMS)
}

# pack <dir> <archive>: archive of the cef/ folder in <dir>.
pack() {
    (cd "$1" && tar -cf - cef | xz -1 > "$2")
}

# manifest <version> <archive> [sha256]: writes cef-manifest for the archive (its real checksum unless one is given).
manifest() {
    sum=${3:-$(sha256sum "$2" | cut -d ' ' -f 1)}
    printf 'version=%s\nurl=file://%s\nsha256=%s\n' "$1" "$2" "$sum" > "$rel/cef-manifest"
}

# run: starts the script with a fresh data folder named by $1 (kept when it exists), output in $T/out.
run() {
    WAYLAND_DISPLAY=x COD_PREVIEW_DIR="$T/$1" sh "$rel/cod-wayland.sh" > "$T/out" 2>&1
}

check() {
    if [ "$2" = ok ]; then
        echo "PASS $1"
        # The last line of output shows why a refused case was refused.
        tail -n 1 "$T/out" | cut -c1-150 | sed 's/^/     -> /'
    else
        echo "FAIL $1: $3"
        sed 's/^/    | /' "$T/out" | head -15
        failed=1
    fi
}

# Nothing may be left installed, staged or locked in data folder $1.
clean() {
    if [ -n "$(ls -A "$T/$1" 2> /dev/null | grep -E '^(cef-|\.cef-stage)' || true)" ]; then
        return 1
    fi
    return 0
}

make_cef "$T/good"
pack "$T/good" "$T/good.tar.xz"

# 1. A good archive installs, and a second start works with the archive gone.
manifest 1.2.3 "$T/good.tar.xz"
if run d1 && [ -f "$T/d1/cef-1.2.3/libcef.so" ]; then ok=ok; else ok=no; fi
check "good archive installs" "$ok" "start failed or libcef.so missing"
mv "$T/good.tar.xz" "$T/good.moved"
if run d1; then ok=ok; else ok=no; fi
check "second start needs no archive" "$ok" "start failed offline"
mv "$T/good.moved" "$T/good.tar.xz"

# 2. Bad versions are refused before anything is created.
for v in '../../victim' 'x/../y' '1..2' '.1' '1.' 'a1' '1/2' '1 2'; do
    manifest "$v" "$T/good.tar.xz"
    mkdir -p "$T/outside"
    if run dv; then ok=no; else ok=ok; fi
    if [ ! -e "$T/dv/logs" ] && [ ! -e "$T/victim" ] && clean dv; then :; else ok=no; fi
    check "bad version '$v' refused" "$ok" "start succeeded or created files"
done

# 3. A bad sha256 field and a bad url are refused.
manifest 1.2.3 "$T/good.tar.xz" "ZZ"
if run dh; then ok=no; else ok=ok; fi
check "malformed sha256 refused" "$ok" "start succeeded"
printf 'version=1.2.3\nurl=ftp://example.invalid/x\nsha256=%s\n' "$(sha256sum "$T/good.tar.xz" | cut -d ' ' -f 1)" > "$rel/cef-manifest"
if run dh; then ok=no; else ok=ok; fi
check "unsupported url refused" "$ok" "start succeeded"

# 4. A wrong archive checksum installs nothing.
manifest 1.2.3 "$T/good.tar.xz" "$(printf '%064d' 0)"
if run dc; then ok=no; else ok=ok; fi
if clean dc; then :; else ok=no; fi
check "wrong archive checksum installs nothing" "$ok" "start succeeded or left files"

# 5. Archive with a ../ member.
mkdir -p "$T/evil1"
make_cef "$T/evil1"
printf 'x' > "$T/evil1/outside.txt"
(cd "$T/evil1" && tar -cPf - cef cef/../outside.txt | xz -1 > "$T/evil1.tar.xz")
manifest 1.2.3 "$T/evil1.tar.xz"
if run de1; then ok=no; else ok=ok; fi
if clean de1; then :; else ok=no; fi
check "archive with .. member refused" "$ok" "start succeeded or left files"

# 6. Archive where cef is a symbolic link to a folder that has everything.
mkdir -p "$T/evil2/real"
make_cef "$T/evil2/real"
(cd "$T/evil2" && ln -s real/cef cef && tar -cf - cef | xz -1 > "$T/evil2.tar.xz")
manifest 1.2.3 "$T/evil2.tar.xz"
if run de2; then ok=no; else ok=ok; fi
if clean de2; then :; else ok=no; fi
check "archive with cef as a symlink refused" "$ok" "start succeeded or left files"

# 7. Archive with a symlink inside cef/.
make_cef "$T/evil3"
(cd "$T/evil3/cef" && ln -s /etc/passwd link.txt)
(cd "$T/evil3" && tar -cf - cef | xz -1 > "$T/evil3.tar.xz")
manifest 1.2.3 "$T/evil3.tar.xz"
if run de3; then ok=no; else ok=ok; fi
if clean de3; then :; else ok=no; fi
check "archive with a symlink member refused" "$ok" "start succeeded or left files"

# 8. Archive with a hard link inside cef/.
make_cef "$T/evil4"
(cd "$T/evil4/cef" && ln libcef.so hard.so)
(cd "$T/evil4" && tar -cf - cef | xz -1 > "$T/evil4.tar.xz")
manifest 1.2.3 "$T/evil4.tar.xz"
if run de4; then ok=no; else ok=ok; fi
if clean de4; then :; else ok=no; fi
check "archive with a hard link member refused" "$ok" "start succeeded or left files"

# 9. A file in the folder that fails its checksum.
make_cef "$T/evil5"
printf 'tampered' > "$T/evil5/cef/libcef.so"
(cd "$T/evil5" && tar -cf - cef | xz -1 > "$T/evil5.tar.xz")
manifest 1.2.3 "$T/evil5.tar.xz"
if run de5; then ok=no; else ok=ok; fi
if clean de5; then :; else ok=no; fi
check "file failing its checksum refused" "$ok" "start succeeded or left files"

# 10. SHA256SUMS naming a path outside the folder.
make_cef "$T/evil6"
printf '%s  ../../outside\n' "$(printf 'x' | sha256sum | cut -d ' ' -f 1)" >> "$T/evil6/cef/SHA256SUMS"
(cd "$T/evil6" && tar -cf - cef | xz -1 > "$T/evil6.tar.xz")
manifest 1.2.3 "$T/evil6.tar.xz"
if run de6; then ok=no; else ok=ok; fi
if clean de6; then :; else ok=no; fi
check "SHA256SUMS with an outside path refused" "$ok" "start succeeded or left files"

# 11. A symlink where the final folder goes is not followed or replaced.
mkdir -p "$T/target-of-link" "$T/dl"
ln -s "$T/target-of-link" "$T/dl/cef-1.2.3"
printf 'keep' > "$T/target-of-link/keep.txt"
manifest 1.2.3 "$T/good.tar.xz"
if run dl; then ok=no; else ok=ok; fi
if [ -f "$T/target-of-link/keep.txt" ] && [ ! -e "$T/target-of-link/libcef.so" ]; then :; else ok=no; fi
check "symlinked cef folder is left alone" "$ok" "start succeeded or touched the link target"

# 12. Concurrent first starts: all succeed, one complete install, no staging or lock left.
head -c 20000000 /dev/urandom > "$T/big.bin"
make_cef "$T/conc"
cp "$T/big.bin" "$T/conc/cef/resources.pak"
(cd "$T/conc/cef" && find . -type f ! -name SHA256SUMS | sort | xargs sha256sum > SHA256SUMS)
(cd "$T/conc" && tar -cf - cef | xz -1 > "$T/conc.tar.xz")
manifest 1.2.4 "$T/conc.tar.xz"
rm -rf "$T/dp"
i=0
while [ "$i" -lt 5 ]; do
    (WAYLAND_DISPLAY=x COD_PREVIEW_DIR="$T/dp" sh "$rel/cod-wayland.sh" > "$T/conc-out.$i" 2>&1; echo $? > "$T/conc-rc.$i") &
    i=$((i + 1))
done
wait
ok=ok
i=0
while [ "$i" -lt 5 ]; do
    [ "$(cat "$T/conc-rc.$i")" = 0 ] || ok=no
    i=$((i + 1))
done
[ -f "$T/dp/cef-1.2.4/libcef.so" ] && (cd "$T/dp/cef-1.2.4" && sha256sum -c --quiet SHA256SUMS > /dev/null 2>&1) || ok=no
[ -z "$(ls -A "$T/dp" | grep -E '^\.cef-stage' || true)" ] || ok=no
cat "$T/conc-out.0" > "$T/out"
check "5 concurrent first starts install once, cleanly" "$ok" "a start failed or left staging or lock files"

# 13. A second version is installed next to the first. Both are kept, and so is everything else in the folder.
manifest 1.2.5 "$T/good.tar.xz"
mkdir -p "$T/dp/other-folder"
if run dp && [ -f "$T/dp/cef-1.2.5/libcef.so" ] && [ -f "$T/dp/cef-1.2.4/libcef.so" ] && [ -d "$T/dp/other-folder" ]; then ok=ok; else ok=no; fi
check "second version installs next to the first, both kept" "$ok" "unexpected folders"

# 14. A failing final mv aborts: non-zero exit, no success message, nothing installed.
mkdir -p "$T/shim"
printf '#!/bin/sh\nexit 1\n' > "$T/shim/mv"
chmod +x "$T/shim/mv"
manifest 1.2.7 "$T/good.tar.xz"
if WAYLAND_DISPLAY=x COD_PREVIEW_DIR="$T/dm" PATH="$T/shim:$PATH" sh "$rel/cod-wayland.sh" > "$T/out" 2>&1; then ok=no; else ok=ok; fi
if grep -q "is installed" "$T/out"; then ok=no; fi
if [ -e "$T/dm/cef-1.2.7" ]; then ok=no; fi
if clean dm; then :; else ok=no; fi
check "failed final mv aborts with no success message" "$ok" "start succeeded, said installed, or left files"

# 15. A held lock is never stolen, however old the lock file is: the start waits for the holder.
manifest 1.2.8 "$T/good.tar.xz"
mkdir -p "$T/dk"
touch -t 200001010000 "$T/dk/.cef-install.lock"
(flock "$T/dk/.cef-install.lock" sleep 4) &
sleep 1
s=$(date +%s)
if run dk && [ -f "$T/dk/cef-1.2.8/libcef.so" ]; then ok=ok; else ok=no; fi
e=$(date +%s)
wait
[ $((e - s)) -ge 2 ] || ok=no
check "a held lock is waited for, not stolen" "$ok" "start did not wait for the lock holder or failed"

if [ "$failed" -eq 0 ]; then
    echo "all cases passed"
else
    echo "some cases FAILED"
    exit 1
fi
