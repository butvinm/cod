# Cod

Cod is a desktop workspace for agent-driven development. It is the Zed editor plus a small set of its own: agent chats, web pages in panes, review, a command-line tool and workspaces. It is a personal tool built in the open by one person and a team of agents. It should look and feel like Zed: fast, quiet, keyboard first, very few settings.

Cod is a fork of [Zed](https://github.com/zed-industries/zed) at tag [v1.23.2](https://github.com/zed-industries/zed/tree/v1.23.2). It stays frozen at that version and does not merge from upstream. Zed is licensed primarily under GPL-3.0-or-later, with Apache-2.0 components where marked, and so is this fork (see the `LICENSE-*` files).

The earlier Electron prototype lives at [butvinm/cod-electron](https://github.com/butvinm/cod-electron).

Cod is in an early preview. Right now it is stock Zed 1.23.2 plus a web page in a pane (the `web_view` crate).

## Try it on Linux

The preview runs on x86_64 Linux with X11. On a Wayland desktop it runs through XWayland.

1. Install WebKitGTK, for example on Ubuntu 24.04 or Debian 13: `sudo apt install libwebkit2gtk-4.1-0`.
2. Download `cod-linux-x86_64.tar.gz` from the latest prerelease on the [Releases](https://github.com/butvinm/cod/releases) page.
3. Unpack it: `tar -xzf cod-linux-x86_64.tar.gz`.
4. Start it: `./cod/cod-xwayland.sh`. It opens a public GitLab merge request in a pane.

To open another page and a folder: `./cod/cod-xwayland.sh <url> <folder>`.

The start script makes Cod use X11 (the web page needs X11 for now) and keeps its settings and data in `~/.local/share/cod-preview`, so it never touches your own Zed settings. To remove Cod, delete the unpacked `cod` folder and `~/.local/share/cod-preview`.

## Building

Cod builds like Zed: see [Building Zed for Linux](./docs/src/development/linux.md) and [Building Zed for macOS](./docs/src/development/macos.md). On Linux the web view also needs `libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev libjavascriptcoregtk-4.1-dev libxdo-dev`. The Linux preview is built by `.github/workflows/linux-release.yml`.
