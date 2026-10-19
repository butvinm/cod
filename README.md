# Cod

Cod is a desktop workspace for agent-driven development. It is the Zed editor plus a small set of its own: agent chats, web pages in panes, review, a command-line tool and workspaces. It is a personal tool built in the open by one person and a team of agents. It should look and feel like Zed: fast, quiet, keyboard first, very few settings.

Cod is a fork of [Zed](https://github.com/zed-industries/zed) at tag [v1.23.2](https://github.com/zed-industries/zed/tree/v1.23.2). It stays frozen at that version and does not merge from upstream. Zed is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked, and so is this fork (see the `LICENSE-*` files).

The earlier Electron prototype lives at [butvinm/cod-electron](https://github.com/butvinm/cod-electron).

Cod is in an early preview. Right now it is stock Zed 1.23.2 plus a web page in a pane (the `web_view` crate).

## Try it on Linux

The preview runs on x86_64 Linux. It is built on Ubuntu 24.04, so it needs glibc 2.39 or newer (for example Ubuntu 24.04, Debian 13, Fedora 40 or a current Arch).

On a Wayland desktop, use `cod-wayland.sh` (recommended). Cod runs as a native Wayland client and web pages are drawn by an off-screen browser (Chromium Embedded Framework and the `cod-browser` helper). The browser runtime is about 100 MB, so it is not in the tarball: the first `cod-wayland.sh` start downloads it once into `~/.local/share/cod-preview/cef-<version>/`, checks it by checksum, and later starts (and later previews with the same browser version) need no download. Old `cef-<version>` folders are never deleted automatically (about 300 MB each unpacked): when Cod is not running, delete the ones you no longer need with `rm -r ~/.local/share/cod-preview/cef-<old version>`. The download needs `curl` or `wget`, `xz`, `sha256sum` and `flock` (util-linux).

1. Install the libraries the browser needs. On Arch: `sudo pacman -S nss nspr alsa-lib at-spi2-core cups libdrm mesa libxkbcommon pango libxcomposite libxdamage libxrandr`. The `cod` binary is still linked against WebKitGTK, so it also needs `webkit2gtk-4.1` on Arch (`libwebkit2gtk-4.1-0` on Ubuntu 24.04 or Debian 13).
2. Download `cod-linux-x86_64.tar.xz` (about 85 MB) from the latest `preview-*` prerelease on the [Releases](https://github.com/butvinm/cod/releases) page. The `cef-*` prereleases only hold the browser runtime for the start script.
3. Unpack it: `tar -xJf cod-linux-x86_64.tar.xz`.
4. Start it: `./cod/cod-wayland.sh`. The first time it downloads the browser runtime. It opens a Wikipedia page in a pane.

To open another page and a folder: `./cod/cod-wayland.sh <url> <folder>`.

To start Cod from your desktop's app launcher with the Cod icon, run `./cod/install-launcher.sh` once. It copies the icons and the launcher into `~/.local/share` (or `$XDG_DATA_HOME`) and points the launcher at `cod-wayland.sh` in the unpacked folder, so run it again if you move that folder. Stock Zed's launcher uses the same file name, so if `~/.local/share/applications/dev.zed.Zed.desktop` exists and Cod did not install it, the script changes nothing and stops with an error; move that file away first if you want Cod's launcher instead. To remove the launcher, delete `~/.local/share/applications/dev.zed.Zed.desktop` and `~/.local/share/icons/hicolor/*/apps/cod.png`.

Without a Wayland session (X11), or if the off-screen browser does not work for you, use `./cod/cod-xwayland.sh [url] [folder]` instead. It makes Cod use X11 (through XWayland on a Wayland desktop) with the older WebKitGTK web view.

Both start scripts keep its settings and data in `~/.local/share/cod-preview`, so it never touches your own Zed settings. Zed's log and the program's error output go to `~/.local/share/cod-preview/logs/`, and panics with a backtrace go to `cod-stderr.log` there. If Cod quits unexpectedly, attach the end of `cod-stderr.log` to your report. For a native crash (the window vanishes with no panic in that log), a core dump is available if systemd-coredump is installed and enabled: run `coredumpctl list cod` and `coredumpctl info cod` and attach the `info` output, not the core file. To remove Cod, delete the unpacked `cod` folder and `~/.local/share/cod-preview`.

## Building

Cod builds like Zed: see [Building Zed for Linux](./docs/src/development/linux.md) and [Building Zed for macOS](./docs/src/development/macos.md). On Linux the web view also needs `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libxdo-dev`. The Linux preview is built by `.github/workflows/linux-release.yml`.
