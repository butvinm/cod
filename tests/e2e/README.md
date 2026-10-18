# End-to-end tests

These tests drive the real Cod app on Linux. Each test makes a throwaway git repo, starts Cod on it through `cod-wayland.sh` (as a user does), presses keys and reads what the app shows. CI runs them on every pull request that changes code (the `e2e` job of the `E2E` workflow).

## Run

```
script/e2e                                   # all tests
script/e2e test_22_open_file                 # one story file
script/e2e test_22_open_file.OpenFileTest.test_open_file_with_the_file_finder
script/e2e -k invoice                        # tests whose name contains "invoice"
```

`script/e2e` is the same command CI runs. It builds Cod (`cargo build --profile release-fast -p zed -p cod_browser`), puts `cod`, `cod-browser` and `cod-wayland.sh` into `target/e2e-run/cod/` like the release tarball, starts its own headless sway (no window appears on your desktop) and runs `python3 -m unittest` over this folder. It exits with the test status.

Optional environment:

- `COD_E2E_BIN`: a prebuilt `zed` binary with `cod-browser` next to it. Skips the build.
- `COD_CEF_DIR`: the folder with `libcef.so`. By default `script/e2e` looks in `$CEF_PATH/<CEF_VERSION>/cef_linux_x86_64` (the version is read from `.github/workflows/linux-release.yml`), then in the cargo build folder, then in the preview data folder `~/.local/share/cod-preview/cef-<CEF_VERSION>`.
- `COD_E2E_ARTIFACTS`: where failures are saved. Default `target/e2e-artifacts`.

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

## What every test start gets

Each Cod start has its own data folder (`--user-data-dir`) in the test's temp folder, with a `settings.json` from `cod_settings()` in `cod_e2e.py`: trusted worktrees, no automatic install of the html extension, no extension or app updates and no telemetry. A test that needs more settings adds them there.

## Add a story test

1. Copy `test_22_open_file.py` to `test_<issue>_<slug>.py`. One file per story.
2. Make the fixture repos in the test with `self.make_repo(name, files)`. They live in a temp folder of that test and are deleted after it. Use invented content only.
3. Put every action in its own `with self.step("..."):`. A failure then logs `FAILED STEP: <step>` and saves the artifacts.
4. Drive the app only with `self.keys("ctrl-p")` (Zed keystroke syntax, space-separated) and `self.type("text")`. Never call into the app in a way a user cannot.
5. Read the app with `self.state()` (`active_tab`, `tabs`, `worktrees`, `modal`) and `self.wait_for(predicate, timeout, description)`. Check with `self.expect_equal("active tab", "invoice.rs", actual)`, which fails with `expected active tab 'invoice.rs', actual '...'`.
6. Check the starting state in its own step before the first key, as the sample does in "check that no file is open". Without it, a test can pass because the expected result was already on the screen.

## Failures

A failed test saves into `$COD_E2E_ARTIFACTS/<test id>/`: `screenshot.png` (grim), `logs/` (Cod's log folder with `Zed.log` and `cod-stderr.log`), `hang_traces/` (when Zed's hang detector saved any), `cod-output.log`, `state.json` and `failure.txt`. The sway log is `$COD_E2E_ARTIFACTS/sway.log`. In CI they are the `e2e-artifacts` artifact of the run.

## Driver protocol

Cod started with `COD_E2E_SOCKET=<path>` listens on that Unix socket. Each request and each reply is one JSON line:

- `{"cmd":"keys","keys":"ctrl-p"}` presses each keystroke. Reply `{"ok":true}`.
- `{"cmd":"type","text":"invoice"}` types one key event per character. Reply `{"ok":true}`.
- `{"cmd":"state"}` replies `{"ok":true,"state":{"active_tab":"invoice.rs","tabs":["invoice.rs"],"worktrees":["/abs/path"],"modal":null}}`.
- Errors reply `{"ok":false,"error":"..."}`.

The unit tests of the library are in `unit/` and need no running Cod: `python3 -m unittest discover -s tests/e2e -t tests/e2e -p 'test_cod_e2e.py'`.
