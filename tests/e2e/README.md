# End-to-end tests

These tests drive the real Cod app on Linux. Each test makes a throwaway git repo, starts Cod on it through `cod-wayland.sh` (as a user does), presses keys and reads what the app shows. CI runs them on every pull request that changes code (the `e2e` job of the `E2E` workflow).

## Run

```
script/e2e                                   # all tests
script/e2e test_22_open_file                 # one story file
script/e2e test_22_open_file.OpenFileTest.test_open_file_with_the_file_finder
script/e2e -k file_finder                    # tests whose name contains "file_finder"
```

A selection that runs no test fails with status 5 and says so, so a typo never looks like a green run.

`script/e2e` is the same command CI runs. It builds Cod (`cargo build --profile release-fast -p zed -p cod_browser`), puts `cod`, `cod-browser` and `cod-wayland.sh` into `target/e2e-run/cod/` like the release tarball, starts its own headless sway (no window appears on your desktop) and runs `python3 -m unittest` over this folder. It exits with the test status.

Optional environment:

- `COD_E2E_BIN`: a prebuilt `zed` binary with `cod-browser` next to it. Skips the build.
- `COD_CEF_DIR`: the folder with `libcef.so`. By default `script/e2e` looks in `$CEF_PATH/<CEF_VERSION>/cef_linux_x86_64` (the version is read from `.github/workflows/linux-release.yml`), then in the cargo build folder, then in the preview data folder `~/.local/share/cod-preview/cef-<CEF_VERSION>`.
- `COD_E2E_ARTIFACTS`: where the tests save screenshots and logs. Default `target/e2e-artifacts`.

## Prerequisites

The build dependencies of Cod (`script/linux`), plus:

| Tool                                           | Ubuntu                | Arch            |
| ---------------------------------------------- | --------------------- | --------------- |
| sway (with swaymsg)                            | `sway`                | `sway`          |
| grim                                           | `grim`                | `grim`          |
| dbus-run-session                               | `dbus`                | `dbus`          |
| software Vulkan                                | `mesa-vulkan-drivers` | `vulkan-swrast` |
| python3 (3.10 or newer, standard library only) | `python3`             | `python`        |
| git                                            | `git`                 | `git`           |
| xz                                             | `xz-utils`            | `xz`            |

`script/e2e` checks these first and prints what is missing.

### Run with a prebuilt binary

A machine without the Rust toolchain can run the tests against a `zed` binary with `cod-browser` next to it, for example the `target/release-fast/` folder of a build made elsewhere with `cargo build --profile release-fast -p zed -p cod_browser`. These steps start from a clean Ubuntu 24.04 with this repo checked out, and run from the repo root.

1. Install the runtime libraries and the tools:

   ```
   sudo apt-get update
   sudo apt-get install -y --no-install-recommends \
     libnss3 libnspr4 libasound2t64 libatspi2.0-0t64 libcups2t64 libdrm2 libgbm1 libgl1-mesa-dri mesa-vulkan-drivers \
     libxkbcommon0 libxkbcommon-x11-0 libpango-1.0-0 libxcomposite1 libxdamage1 libxrandr2 \
     libwebkit2gtk-4.1-0 libfontconfig1 libvulkan1 xz-utils curl util-linux \
     sway grim dbus python3 git
   ```

2. Install the CEF runtime once into the preview data folder, where `script/e2e` finds it. It is the asset of the `cef-<CEF_VERSION>` release of this repo, checked against its `.sha256` asset:

   ```
   version=$(sed -n 's/^ *CEF_VERSION: *//p' .github/workflows/linux-release.yml | head -n 1)
   asset=cod-cef-$version-linux-x86_64.tar.xz
   curl -fLO "https://github.com/butvinm/cod/releases/download/cef-$version/$asset"
   curl -fLO "https://github.com/butvinm/cod/releases/download/cef-$version/$asset.sha256"
   echo "$(cat "$asset.sha256")  $asset" | sha256sum -c
   mkdir -p ~/.local/share/cod-preview
   tar -C ~/.local/share/cod-preview -xJf "$asset"
   mv ~/.local/share/cod-preview/cef ~/.local/share/cod-preview/cef-$version
   rm "$asset" "$asset.sha256"
   ```

   Starting a Cod preview once with its `cod-wayland.sh` installs the same folder.

3. Run the tests:

   ```
   COD_E2E_BIN=<folder>/zed script/e2e
   ```

## What every test start gets

Each Cod start has its own data folder (`--user-data-dir`) in the test's temp folder, with a `settings.json` from `cod_settings()` in `cod_e2e.py`: trusted worktrees, no automatic install of the html extension, no extension or app updates and no telemetry. A test that needs more settings adds them there.

The settings also turn on Zed's own path picker and prompts (`use_system_path_prompts: false`, `use_system_prompts: false`), because the headless sway has no desktop portal, and turn off the command history of the palette (`command_palette.use_command_history: false`), so the palette ranks matches by the query alone and earlier commands of a test never change which row a command name highlights.

By default Cod runs with `ZED_STATELESS=1`: Zed keeps its database in memory and forgets the session. `self.start_cod(folder, persistent=True)` leaves it out, so Zed keeps its database and session in the test's data folder. `self.start_cod(None)` starts Cod without a folder.

## Add a story test

1. Copy `test_22_open_file.py` to `test_<issue>_<slug>.py`. One file per story.
2. Make the fixture repos in the test with `self.make_repo(name, files)`. They live in a temp folder of that test and are deleted after it. Use invented content only.
3. Put every action in its own `with self.step("..."):`. A failure then logs `FAILED STEP: <step>` and saves the artifacts.
4. Drive the app only with `self.keys("ctrl-p")` (Zed keystroke syntax, space-separated) and `self.type("text")`. Never call into the app in a way a user cannot.
5. Run a command from the command palette with `self.run_command("pane: split and move right")`: it presses ctrl-shift-p, types the command, waits until the palette highlights exactly that command and presses enter. When the palette highlights another command, or none, it fails with `expected palette selection '...', actual '...'` and never presses enter. Type the full command name as the palette shows it.
6. Read the app with `self.state()` (fields below) and `self.wait_for(predicate, timeout, description)`. Check with `self.expect_equal("active tab", "invoice.rs", actual)`, which fails with `expected active tab 'invoice.rs', actual '...'`. To wait for a complete outcome, use `self.wait_for_values(what, expected, read, timeout)`: it waits until `read(state)` has every value of `expected` and otherwise fails with one `expected <what> <key> ..., actual ...` per value that still differs.
7. Check the starting state in its own step before the first key, as the sample does in "check that no file is open". Without it, a test can pass because the expected result was already on the screen.
8. To check what survives a restart, start Cod with `persistent=True` and call `self.restart_cod()`: it quits Cod with ctrl-q, waits until Cod and every process it started have exited (and fails when Cod exits with an error or does not exit in time), then starts Cod again with the same data folder and home and no folder, so Zed restores its last session.
9. To check what survives a crash, call `self.kill_cod()`: it kills Cod's process group with SIGKILL, with no clean quit, and every process it started. `self.start_cod_again()` then starts Cod on the same data folder and home with no folder; `connect=False` only launches it, so a test can kill it again while it starts.

### State fields

| Field               | Value                                                                                                                                                                                                                                                             |
| ------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `active_tab`        | title of the active tab of the active workspace, or null                                                                                                                                                                                                          |
| `tabs`              | tab titles of the active pane                                                                                                                                                                                                                                     |
| `worktrees`         | absolute paths of the folders of the active workspace                                                                                                                                                                                                             |
| `modal`             | `"file_finder"`, `"command_palette"`, `"other"` or null                                                                                                                                                                                                           |
| `window_count`      | number of open Cod windows                                                                                                                                                                                                                                        |
| `window_title`      | the title sway shows for Cod's window (the focused one when there are several), or null; added by `CodTestCase.state()`                                                                                                                                           |
| `sway_windows`      | sway's ids of Cod's windows, in sway's tree order; added by `CodTestCase.state()`                                                                                                                                                                                 |
| `workspace_count`   | number of workspaces the window holds                                                                                                                                                                                                                             |
| `active_workspace`  | index of the active workspace in `workspaces`                                                                                                                                                                                                                     |
| `workspaces`        | `{"name", "worktrees", "drawn"}` for each workspace in window order; `name` is null while the workspace has no name; `drawn` tells whether the last frame the window drew holds any of its center panes                                                           |
| `drawn_tabs`        | tab titles of every center pane, of any workspace, that the last drawn frame holds                                                                                                                                                                                |
| `panes`             | `{"tabs", "active_tab"}` for each center pane of the active workspace, in layout order (left to right, top to bottom, following the split tree)                                                                                                                   |
| `split`             | how the center panes are split at the root: `"vertical"` (a vertical divider, panes side by side, as "pane: split right" makes), `"horizontal"` (panes stacked), null for one pane, `"unknown"` while several panes are not laid out yet                          |
| `cursor`            | `{"row", "column"}` of the cursor in the active editor, both 1-based as the status bar shows them (column in bytes), or null                                                                                                                                      |
| `dirty_tabs`        | titles of the tabs with unsaved changes in the active workspace, in pane order                                                                                                                                                                                    |
| `unsaved_texts`     | `{"tab", "text"}` for each of those tabs: its title and the whole text its editor holds now (`text` is null for a tab that is not an editor)                                                                                                                      |
| `project_panel`     | the project panel of the active workspace: `{"open", "focused", "roots", "expanded", "selected", "rows"}`, or null when the workspace has none; rows are named `<root>/<path>` (a root row by its name), `expanded` lists the expanded folders below the roots    |
| `rename_input`      | `{"text"}` while the field of "workspace: rename workspace" is shown, else null; story #24 adds the field                                                                                                                                                         |
| `palette`           | `{"query", "match_count", "selected_index", "listed_query", "highlighted"}` while the command palette is open, else null; `listed_query` is the query its rows were matched against (null while it is matching), `highlighted` the command on the highlighted row |
| `palette_selection` | name of the command on the highlighted row when the rows belong to the typed query, else null                                                                                                                                                                     |
| `path_prompt`       | `{"query", "match_count"}` while Zed's path picker (for example of "workspace: add folder to project") is open, else null                                                                                                                                         |
| `file_finder_ready` | true when the file finder is in the last drawn frame and has the focus there, so typed keys reach its query; false otherwise (also while it is open but not drawn yet)                                                                                            |
| `palette_focused`   | true when the command palette holds the focus, false when it is open without it (typed keys do not reach its query), null while no palette is open                                                                                                                |
| `window_active`     | true when the platform reports Cod's window as the active (keyboard) window                                                                                                                                                                                       |
| `prompt`            | `{"message", "detail", "answers", "highlighted"}` while a prompt is shown in the window, else null: message and detail as Zed passes them (Markdown source, so a file name is in backticks), the answer labels, and the index of the highlighted answer           |

The driver reads the palette, the prompt and the project panel through small read-only getters added to those crates for the tests (`CommandPaletteDelegate::highlighted_command_name` and `listed_query`, `ZedPromptRenderer::message`, `detail`, `answers` and `highlighted_answer`, `ProjectPanel::listed_entries`). A pane counts as drawn when its own element is in the dispatch tree of the last frame.

With several windows, keys and `state` go to the window the platform names active; the headless sway has no keyboard, so the platform names none, and the driver takes the newest open window, the one sway focuses.

## Artifacts and failures

Every test that starts Cod saves into `$COD_E2E_ARTIFACTS/<test id>/`: `screenshot.png` (grim), `logs/` (Cod's log folder with `Zed.log` and `cod-stderr.log`), `hang_traces/` (when Zed's hang detector saved any), `cod-output.log` (the output of every start of the test, one after the other) and `state.json`. A failed test saves them at its first failed step, with `failure.txt` naming the step; a passing test saves them at its end, with `final.txt`. The sway log is `$COD_E2E_ARTIFACTS/sway.log`, and `run.txt` holds the commit, the command and the status. In CI they are the `e2e-artifacts-<commit>` artifact of every run.

When Cod stops at the end of a test, the test fails if a process carrying its driver socket is still running. After all tests, `script/e2e` lists any process of the run that is still running and fails if there is one.

## Driver protocol

Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket. Each request and each reply is one JSON line:

- `{"cmd":"keys","keys":"ctrl-p"}` presses each keystroke. Reply `{"ok":true}`.
- `{"cmd":"type","text":"invoice"}` types one key event per character. Reply `{"ok":true}`.
- `{"cmd":"state"}` replies `{"ok":true,"state":{"active_tab":"invoice.rs","tabs":["invoice.rs"],"worktrees":["/abs/path"],"modal":null,...}}` with every field of the table above except `window_title`.
- Errors reply `{"ok":false,"error":"..."}`.

The unit tests of the library are in `unit/` and need no running Cod: `python3 -m unittest discover -s tests/e2e -t tests/e2e -p 'test_cod_e2e.py'`.
