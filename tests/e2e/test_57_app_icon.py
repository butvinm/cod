"""Story #57: the About window of the real app shows the official Cod icon.

The test opens the About window from the command palette ("zed: about") with keys only, then reads the screen from outside the app: sway's window tree for the About window's place and grim for the pixels.
It saves about-window.png (the whole compositor output) and about-icon-crop.png (the icon region of the About window) into the test's artifact folder, for passing runs too.
It compares the drawn icon loosely with crates/zed/resources/app-icon.png scaled to the drawn size, using the Python standard library only.
"""

from __future__ import annotations

import glob
import json
import os
import shutil
import struct
import subprocess
import time
import unittest
import zlib
from pathlib import Path
from typing import Any

from cod_e2e import REPO_ROOT, CodTestCase, artifact_name, artifacts_root, log

LANTERN_NOTES = {
    "README.md": "# lantern-notes\n\nAn invented notebook that keeps short notes about lamps.\n",
    "notes/first.md": "# First note\n\nThe brass lamp needs a new wick.\n",
}

# The About window of crates/zed/src/zed.rs (open_about_window): 440 x 300 logical pixels, padding p_4 (16 px), the icon img(..).size_16() (64 px) centered at the top.
# The headless sway of the tests has one output at scale 1, so logical pixels are output pixels.
ABOUT_TITLE = "About Zed"
WINDOW_PADDING = 16
ICON_SIZE = 64
# The icon region is captured with this margin on every side, so a small layout difference still keeps the whole icon in the crop.
MARGIN = 10
REFERENCE_ICON = REPO_ROOT / "crates" / "zed" / "resources" / "app-icon.png"
# Mean absolute difference per RGB channel (0-255) between the drawn icon and the reference scaled to the drawn size.
# Loose on purpose: the renderer samples the 512 px image differently from the box filter here.
MAX_ICON_DIFFERENCE = 20.0
# The same difference must also be well below the difference between the drawn crop and a plain window background, so a missing or blank icon cannot pass.
MAX_SHARE_OF_BACKGROUND = 0.6


def read_png(path: Path) -> tuple[int, int, int, list[bytes]]:
    """Decodes an 8-bit, non-interlaced RGB or RGBA PNG. Returns width, height, channels and the rows."""
    data = path.read_bytes()
    if data[:8] != b"\x89PNG\r\n\x1a\n":
        raise ValueError(f"{path} is not a PNG")
    offset = 8
    header = b""
    compressed = bytearray()
    while offset < len(data):
        (length,) = struct.unpack(">I", data[offset : offset + 4])
        kind = data[offset + 4 : offset + 8]
        body = data[offset + 8 : offset + 8 + length]
        offset += 12 + length
        if kind == b"IHDR":
            header = body
        elif kind == b"IDAT":
            compressed += body
        elif kind == b"IEND":
            break
    width, height, depth, color, _, _, interlace = struct.unpack(">IIBBBBB", header)
    channels = {2: 3, 6: 4}.get(color)
    if depth != 8 or channels is None or interlace != 0:
        raise ValueError(f"{path}: unsupported PNG (depth {depth}, color type {color}, interlace {interlace})")
    raw = zlib.decompress(bytes(compressed))
    stride = width * channels
    rows: list[bytes] = []
    previous = bytearray(stride)
    position = 0
    for _ in range(height):
        kind = raw[position]
        line = bytearray(raw[position + 1 : position + 1 + stride])
        position += 1 + stride
        if kind == 1:
            for i in range(channels, stride):
                line[i] = (line[i] + line[i - channels]) & 0xFF
        elif kind == 2:
            for i in range(stride):
                line[i] = (line[i] + previous[i]) & 0xFF
        elif kind == 3:
            for i in range(stride):
                left = line[i - channels] if i >= channels else 0
                line[i] = (line[i] + ((left + previous[i]) >> 1)) & 0xFF
        elif kind == 4:
            for i in range(stride):
                a = line[i - channels] if i >= channels else 0
                b = previous[i]
                c = previous[i - channels] if i >= channels else 0
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                predictor = a if pa <= pb and pa <= pc else (b if pb <= pc else c)
                line[i] = (line[i] + predictor) & 0xFF
        elif kind != 0:
            raise ValueError(f"{path}: unknown PNG filter {kind}")
        rows.append(bytes(line))
        previous = line
    return width, height, channels, rows


def write_png(path: Path, width: int, height: int, pixels: list[tuple[int, int, int]]) -> None:
    """Writes an 8-bit RGB PNG from row-major pixels."""
    raw = bytearray()
    for y in range(height):
        raw.append(0)
        for r, g, b in pixels[y * width : (y + 1) * width]:
            raw += bytes((r, g, b))

    def chunk(kind: bytes, body: bytes) -> bytes:
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body) & 0xFFFFFFFF)

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    path.write_bytes(b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header) + chunk(b"IDAT", zlib.compress(bytes(raw), 9)) + chunk(b"IEND", b""))


def rgb_pixels(width: int, height: int, channels: int, rows: list[bytes]) -> list[tuple[int, int, int]]:
    return [(row[x * channels], row[x * channels + 1], row[x * channels + 2]) for row in rows[:height] for x in range(width)]


def scaled_reference(size: int, background: tuple[int, int, int]) -> list[tuple[int, int, int]]:
    """app-icon.png scaled to size x size with a box filter and drawn over the background color."""
    width, height, channels, rows = read_png(REFERENCE_ICON)
    if channels != 4 or width != height or width % size != 0:
        raise ValueError(f"{REFERENCE_ICON}: expected a square RGBA icon whose side divides by {size}, got {width} x {height}, {channels} channels")
    block = width // size
    count = block * block
    pixels = []
    for y in range(size):
        for x in range(size):
            red = green = blue = alpha = 0
            for row in rows[y * block : (y + 1) * block]:
                for px in range(x * block, (x + 1) * block):
                    a = row[px * 4 + 3]
                    red += row[px * 4] * a
                    green += row[px * 4 + 1] * a
                    blue += row[px * 4 + 2] * a
                    alpha += a
            coverage = alpha / (255 * count)
            pixels.append(
                tuple(
                    round(premultiplied / (255 * count) + base * (1 - coverage))
                    for premultiplied, base in zip((red, green, blue), background)
                )
            )
    return pixels


def mean_difference(
    crop: list[tuple[int, int, int]], crop_width: int, left: int, top: int, reference: list[tuple[int, int, int]], size: int, step: int = 1
) -> float:
    """Mean absolute RGB difference between the size x size block of the crop at (left, top) and the reference, sampling every step-th pixel."""
    total = 0
    samples = 0
    for y in range(0, size, step):
        for x in range(0, size, step):
            drawn = crop[(top + y) * crop_width + left + x]
            expected = reference[y * size + x]
            total += abs(drawn[0] - expected[0]) + abs(drawn[1] - expected[1]) + abs(drawn[2] - expected[2])
            samples += 3
    return total / samples


def best_match(crop: list[tuple[int, int, int]], crop_width: int, reference: list[tuple[int, int, int]], size: int, margin: int) -> tuple[float, int, int]:
    """The offset inside the crop where the icon matches the reference best, and the full mean difference there."""
    coarse = min(
        ((mean_difference(crop, crop_width, dx, dy, reference, size, step=4), dx, dy) for dy in range(2 * margin + 1) for dx in range(2 * margin + 1)),
    )
    _, dx, dy = coarse
    return mean_difference(crop, crop_width, dx, dy, reference, size), dx, dy


def sway_socket() -> str:
    """The IPC socket of the private headless sway of script/e2e (SWAYSOCK is unset on purpose, so it is found in XDG_RUNTIME_DIR)."""
    runtime = os.environ.get("XDG_RUNTIME_DIR", "")
    sockets = sorted(glob.glob(os.path.join(runtime, "sway-ipc.*.sock")))
    if not sockets:
        raise AssertionError(f"no sway IPC socket in XDG_RUNTIME_DIR {runtime!r}")
    return sockets[0]


def sway_windows(tree: dict[str, Any]) -> list[dict[str, Any]]:
    """Every window (a node with a pid) of a sway tree, tiled or floating."""
    found = []
    stack = [tree]
    while stack:
        node = stack.pop()
        if node.get("pid") is not None and node.get("type") in ("con", "floating_con"):
            found.append(node)
        stack.extend(node.get("nodes", []))
        stack.extend(node.get("floating_nodes", []))
    return found


class AppIconTest(CodTestCase):
    def setUp(self) -> None:
        super().setUp()
        # The library empties the test's artifact folder when it saves its own artifacts (at the end, or at the first failed step).
        # So the icon files are written here first and copied in by a cleanup registered before start_cod, which runs after the library's.
        self.icon_artifacts = self.workdir / "icon-artifacts"
        self.icon_artifacts.mkdir()
        self.addCleanup(self._copy_icon_artifacts)

    def _copy_icon_artifacts(self) -> None:
        files = sorted(self.icon_artifacts.iterdir())
        if not files:
            return
        target = artifacts_root() / artifact_name(self.id())
        target.mkdir(parents=True, exist_ok=True)
        for path in files:
            shutil.copy2(path, target / path.name)
        log(f"  icon artifacts saved to {target}: {', '.join(path.name for path in files)}")

    def grim(self, path: Path, region: str | None = None) -> None:
        command = ["grim"] + (["-g", region] if region else []) + [str(path)]
        result = subprocess.run(command, capture_output=True, text=True, timeout=20)
        if result.returncode != 0:
            raise AssertionError(f"{' '.join(command)} failed with status {result.returncode}: {result.stderr.strip()}")

    def sway_tree(self) -> dict[str, Any]:
        result = subprocess.run(["swaymsg", "-s", sway_socket(), "-r", "-t", "get_tree"], capture_output=True, text=True, timeout=10)
        if result.returncode != 0:
            raise AssertionError(f"swaymsg get_tree failed with status {result.returncode}: {result.stderr.strip()}")
        return json.loads(result.stdout)

    def about_window(self) -> dict[str, Any] | None:
        for window in sway_windows(self.sway_tree()):
            rect = window.get("rect", {})
            if window.get("name") == ABOUT_TITLE and rect.get("width", 0) > 0 and rect.get("height", 0) > 0:
                return window
        return None

    def test_about_window_shows_the_cod_icon(self) -> None:
        with self.step("create the fixture repo lantern-notes"):
            repo = self.make_repo("lantern-notes", LANTERN_NOTES)

        with self.step("start Cod on lantern-notes"):
            self.start_cod(repo)

        with self.step("wait for the lantern-notes worktree"):
            self.wait_for(
                lambda state: os.path.realpath(repo) in [os.path.realpath(path) for path in state["worktrees"]],
                30,
                "the lantern-notes folder to open",
            )

        with self.step("check that only the workspace window is open"):
            start = self.state()
            self.expect_equal("modal", None, start["modal"])
            if "window_count" in start:
                self.expect_equal("window count", 1, start["window_count"])
            self.expect_equal("About windows in the sway tree", [], [w["name"] for w in sway_windows(self.sway_tree()) if w.get("name") == ABOUT_TITLE])

        with self.step("press ctrl-shift-p"):
            self.keys("ctrl-shift-p")
            self.wait_for(lambda state: state["modal"] == "command_palette", 10, "the command palette to open")

        with self.step("type zed: about"):
            self.type("zed: about")

        with self.step("press enter"):
            self.keys("enter")
            self.wait_for(lambda state: state["modal"] is None, 10, "the command palette to close")

        with self.step("wait for the About window"):
            deadline = time.monotonic() + 15
            window = self.about_window()
            while window is None and time.monotonic() < deadline:
                time.sleep(0.2)
                window = self.about_window()
            if window is None:
                names = [w.get("name") for w in sway_windows(self.sway_tree())]
                raise AssertionError(f"timed out after 15 s waiting for a window named {ABOUT_TITLE!r}; sway windows: {names!r}")
            state = self.state()
            if "window_count" in state:
                self.expect_equal("window count", 2, state["window_count"])
            rect = window["rect"]
            log(f"  About window: app_id {window.get('app_id')!r}, rect {rect}")

        artifacts = self.icon_artifacts
        with self.step("take a screenshot of the compositor output"):
            self.grim(artifacts / "about-window.png")

        icon_left = rect["x"] + (rect["width"] - ICON_SIZE) // 2 - MARGIN
        icon_top = rect["y"] + WINDOW_PADDING - MARGIN
        crop_size = ICON_SIZE + 2 * MARGIN
        region = f"{icon_left},{icon_top} {crop_size}x{crop_size}"
        crop_path = artifacts / "about-icon-crop.png"

        with self.step("compare the drawn icon with app-icon.png"):
            # The image may be decoded and drawn a few frames after the window maps, so the crop is taken again until it matches or the time is up.
            deadline = time.monotonic() + 15
            while True:
                self.grim(crop_path, region)
                width, height, channels, rows = read_png(crop_path)
                self.expect_equal("crop size", (crop_size, crop_size), (width, height))
                crop = rgb_pixels(width, height, channels, rows)
                # The corner of the crop is inside the window's padding, so it shows the window background.
                background = crop[0]
                reference = scaled_reference(ICON_SIZE, background)
                difference, dx, dy = best_match(crop, width, reference, ICON_SIZE, MARGIN)
                flat = mean_difference(crop, width, dx, dy, [background] * (ICON_SIZE * ICON_SIZE), ICON_SIZE)
                log(f"  icon difference {difference:.2f} at offset ({dx - MARGIN}, {dy - MARGIN}); background-only difference {flat:.2f}; background {background}")
                if difference < MAX_ICON_DIFFERENCE and difference < flat * MAX_SHARE_OF_BACKGROUND:
                    break
                if time.monotonic() >= deadline:
                    break
                time.sleep(0.5)
            write_png(artifacts / "about-icon-expected.png", ICON_SIZE, ICON_SIZE, reference)
            (artifacts / "about-icon-check.json").write_text(
                json.dumps(
                    {
                        "about_window_rect": rect,
                        "crop_region": region,
                        "icon_offset_in_crop": [dx, dy],
                        "background": background,
                        "mean_abs_difference": round(difference, 3),
                        "background_only_difference": round(flat, 3),
                        "max_allowed_difference": MAX_ICON_DIFFERENCE,
                    },
                    indent=2,
                )
                + "\n"
            )
            # The whole output again, now that the icon is drawn (or the time is up).
            self.grim(artifacts / "about-window.png")
            self.assertLess(
                difference,
                MAX_ICON_DIFFERENCE,
                f"the drawn icon differs from app-icon.png by {difference:.2f} per channel (allowed {MAX_ICON_DIFFERENCE}); see about-icon-crop.png and about-icon-expected.png",
            )
            self.assertLess(
                difference,
                flat * MAX_SHARE_OF_BACKGROUND,
                f"the drawn icon ({difference:.2f}) is not much closer to app-icon.png than a plain background ({flat:.2f}) is",
            )


if __name__ == "__main__":
    unittest.main()
