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
| sway                                           | `sway`                | `sway`          |
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

## Add a story test

1. Copy `test_22_open_file.py` to `test_<issue>_<slug>.py`. One file per story.
2. Make the fixture repos in the test with `self.make_repo(name, files)`. They live in a temp folder of that test and are deleted after it. Use invented content only, and give each story its own repo name.
3. Put every action in its own `with self.step("..."):`. A failure then logs `FAILED STEP: <step>` and saves the artifacts.
4. Drive the app only with `self.keys("ctrl-p")` (Zed keystroke syntax, space-separated) and `self.type("text")`. Never call into the app in a way a user cannot.
5. Read the app with `self.state()` (`active_tab`, `tabs`, `worktrees`, `modal`) and `self.wait_for(predicate, timeout, description)`. Check with `self.expect_equal("active tab", "invoice.rs", actual)`, which fails with `expected active tab 'invoice.rs', actual '...'`.
6. Check the starting state in its own step before the first key, as the sample does in "check that no file is open". Without it, a test can pass because the expected result was already on the screen.

## Artifacts and failures

Every test that starts Cod saves into `$COD_E2E_ARTIFACTS/<test id>/`: `screenshot.png` (grim), `logs/` (Cod's log folder with `Zed.log` and `cod-stderr.log`), `hang_traces/` (when Zed's hang detector saved any), `cod-output.log` and `state.json`. A failed test saves them at its first failed step, with `failure.txt` naming the step; a passing test saves them at its end, with `final.txt`. The sway log is `$COD_E2E_ARTIFACTS/sway.log`, and `run.txt` holds the commit, the command and the status. In CI they are the `e2e-artifacts-<commit>` artifact of every run.

When Cod stops at the end of a test, the test fails if a process carrying its driver socket is still running. After all tests, `script/e2e` lists any process of the run that is still running and fails if there is one.

## Driver protocol

Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket. Each request and each reply is one JSON line:

- `{"cmd":"keys","keys":"ctrl-p"}` presses each keystroke. Reply `{"ok":true}`.
- `{"cmd":"type","text":"invoice"}` types one key event per character. Reply `{"ok":true}`.
- `{"cmd":"state"}` replies `{"ok":true,"state":{"active_tab":"invoice.rs","tabs":["invoice.rs"],"worktrees":["/abs/path"],"modal":null}}`.
- Errors reply `{"ok":false,"error":"..."}`.

The unit tests of the library are in `unit/` and need no running Cod: `python3 -m unittest discover -s tests/e2e -t tests/e2e -p 'test_cod_e2e.py'`.
