"""Helpers for the workspace tabs (#25) and the overview (#26): reading the title bar, clicking at a window position, and the session status test fixture.

`set_entry` is the one driver request that writes app state: it stands in for the scripted fake agent provider of #28 until that lands (see crates/cod_e2e_driver/src/cod_tabs_state.rs). Everything else is read only or user input.
"""

from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

# The widest a workspace tab gets, in logical pixels (#25 open question 1).
TAB_MAX_WIDTH = 280.0

# The dot of each status, as the driver reports it (#25 AC4): the theme's warning, error, info and success colors.
DOTS = {"needs_input": "yellow", "error": "red", "working": "blue", "done": "green", "idle": None}

# Zed's own title bar items that #25 AC2 removes, by the names the driver reports in title_bar.items.
REMOVED_ITEMS = ["project_name", "branch_picker", "sign_in", "user_menu"]


def title_bar(state: dict[str, Any]) -> dict[str, Any]:
    """The title bar of the active workspace as the driver reports it."""
    bar = state.get("title_bar")
    if not isinstance(bar, dict):
        raise AssertionError(f"expected a title bar in the state, actual {bar!r}")
    return bar


def tab_names(state: dict[str, Any]) -> list[str]:
    return [tab["name"] for tab in title_bar(state)["tabs"]]


def tab_dots(state: dict[str, Any]) -> list[str | None]:
    return [tab["dot"] for tab in title_bar(state)["tabs"]]


def active_tabs(state: dict[str, Any]) -> list[bool]:
    return [tab["active"] for tab in title_bar(state)["tabs"]]


def drawn(bounds: dict[str, Any] | None) -> bool:
    """Whether the bounds belong to an element the last frame drew with a size."""
    return isinstance(bounds, dict) and bounds["width"] > 0 and bounds["height"] > 0


def center(bounds: dict[str, Any]) -> tuple[float, float]:
    return (bounds["x"] + bounds["width"] / 2, bounds["y"] + bounds["height"] / 2)


class CodStatusFixtures:
    """Mixin for CodTestCase subclasses."""

    driver: Any

    def set_entry(self, workspace_index: int, key: str, status: str, title: str | None = None, kind: str = "session", context: str = "") -> None:
        """Sets the session (or page) `key` of the workspace at `workspace_index` of state["workspaces"] to `status`, replacing an earlier entry with the same key."""
        if status not in DOTS:
            raise ValueError(f"unknown status {status!r}")
        self.driver.request(
            "set_entry",
            workspace_index=workspace_index,
            key=key,
            kind=kind,
            title=title if title is not None else key,
            status=status,
            context=context,
        )

    def click_at(self, x: float, y: float) -> None:
        """Moves the mouse to the window position and clicks the left button there."""
        self.driver.request("click", x=x, y=y)

    def click_tab(self, state: dict[str, Any], index: int) -> None:
        """Clicks the middle of the tab at `index` as it was drawn in `state`."""
        tabs = title_bar(state)["tabs"]
        bounds = tabs[index]["bounds"]
        if not drawn(bounds):
            raise AssertionError(f"expected tab {index} to be drawn, actual bounds {bounds!r}")
        self.click_at(*center(bounds))

    def click_add_button(self, state: dict[str, Any]) -> None:
        """Clicks the middle of the "+" button as it was drawn in `state`."""
        bounds = title_bar(state)["add_button"]["bounds"]
        if not drawn(bounds):
            raise AssertionError(f"expected the + button to be drawn, actual bounds {bounds!r}")
        self.click_at(*center(bounds))

    def hover_at(self, x: float, y: float) -> None:
        """Moves the mouse to the window position without pressing a button (issue #77: hover background, close control)."""
        self.driver.request("hover", x=x, y=y)

    def hover_tab(self, state: dict[str, Any], index: int) -> None:
        """Hovers the middle of the tab at `index` as it was drawn in `state`."""
        tabs = title_bar(state)["tabs"]
        bounds = tabs[index]["bounds"]
        if not drawn(bounds):
            raise AssertionError(f"expected tab {index} to be drawn, actual bounds {bounds!r}")
        self.hover_at(*center(bounds))


def screenshot_ppm(path: Path) -> None:
    """Saves a full native screenshot in PPM (issue #77: plain pixel sampling, no image library)."""
    result = subprocess.run(["grim", "-t", "ppm", str(path)], capture_output=True, text=True, timeout=20)
    if result.returncode != 0:
        raise AssertionError(f"grim failed with status {result.returncode}: {result.stderr.strip()}")


def read_ppm(path: Path) -> tuple[int, int, bytes]:
    """Parses a binary (P6) PPM file: (width, height, raw RGB bytes, 8 bits per channel)."""
    data = path.read_bytes()
    if not data.startswith(b"P6"):
        raise AssertionError(f"{path} is not a binary PPM (P6)")
    fields: list[bytes] = []
    offset = 2
    while len(fields) < 3:
        while offset < len(data) and data[offset : offset + 1].isspace():
            offset += 1
        if offset < len(data) and data[offset : offset + 1] == b"#":
            while offset < len(data) and data[offset : offset + 1] != b"\n":
                offset += 1
            continue
        start = offset
        while offset < len(data) and not data[offset : offset + 1].isspace():
            offset += 1
        fields.append(data[start:offset])
    width, height, maxval = (int(field) for field in fields)
    if maxval != 255:
        raise AssertionError(f"{path} has maxval {maxval}, expected 255")
    offset += 1  # the single whitespace byte after maxval
    return width, height, data[offset : offset + width * height * 3]


def ppm_pixel(width: int, height: int, pixels: bytes, x: int, y: int) -> tuple[int, int, int]:
    """The (r, g, b) of the pixel at (x, y) in a buffer `read_ppm` returned."""
    if not (0 <= x < width and 0 <= y < height):
        raise AssertionError(f"pixel ({x}, {y}) is outside the {width}x{height} screenshot")
    index = (y * width + x) * 3
    return (pixels[index], pixels[index + 1], pixels[index + 2])


def hex_color(rgb: tuple[int, int, int]) -> str:
    return "#{:02x}{:02x}{:02x}".format(*rgb)


def bounds_pixel(width: int, height: int, pixels: bytes, scale: float, bounds: dict[str, Any], dx: float = 0.5, dy: float = 0.5) -> tuple[int, int, int]:
    """The pixel at a fraction (dx, dy) of `bounds` (a driver bounds dict in logical pixels), at the screenshot's `scale`. (0.5, 0.5) is the center."""
    x = round((bounds["x"] + bounds["width"] * dx) * scale)
    y = round((bounds["y"] + bounds["height"] * dy) * scale)
    return ppm_pixel(width, height, pixels, x, y)


def logical_pixel(width: int, height: int, pixels: bytes, scale: float, x: float, y: float) -> tuple[int, int, int]:
    """The pixel at a window position in logical pixels, at the screenshot's `scale`."""
    return ppm_pixel(width, height, pixels, round(x * scale), round(y * scale))


def ink_vertical_center(width: int, height: int, pixels: bytes, scale: float, bounds: dict[str, Any]) -> float:
    """The vertical center, in logical pixels, of the ink (any pixel that differs from the box's own most common color) inside `bounds`.

    Used to check that a tab's drawn content (its label and dot) sits in the middle of the tab's
    own box (issue #77 AC(d)), without needing new bounds of the label or the dot: the background
    occupies most of the box, so its most common color is read as the background and any
    different pixel counts as ink.
    """
    left = max(0, round(bounds["x"] * scale))
    right = min(width, round((bounds["x"] + bounds["width"]) * scale))
    top = max(0, round(bounds["y"] * scale))
    bottom = min(height, round((bounds["y"] + bounds["height"]) * scale))
    if left >= right or top >= bottom:
        raise AssertionError(f"bounds {bounds!r} at scale {scale} are empty in a {width}x{height} screenshot")
    counts: dict[tuple[int, int, int], int] = {}
    sampled: list[tuple[int, int, tuple[int, int, int]]] = []
    for y in range(top, bottom):
        for x in range(left, right, 2):
            color = ppm_pixel(width, height, pixels, x, y)
            counts[color] = counts.get(color, 0) + 1
            sampled.append((x, y, color))
    background = max(counts, key=lambda color: counts[color])
    ink_rows = sorted({y for x, y, color in sampled if color_distance(color, background) > 20})
    if not ink_rows:
        raise AssertionError(f"no ink found in bounds {bounds!r} (background {hex_color(background)})")
    return (ink_rows[0] + ink_rows[-1]) / 2 / scale


def color_distance(a: tuple[int, int, int], b: tuple[int, int, int]) -> float:
    """Euclidean distance between two (r, g, b) colors, for a tolerance check against a theme token."""
    return sum((x - y) ** 2 for x, y in zip(a, b)) ** 0.5
