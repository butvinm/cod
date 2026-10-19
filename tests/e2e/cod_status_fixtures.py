"""Helpers for the workspace tabs (#25) and the overview (#26): reading the title bar, clicking at a window position, and the session status test fixture.

`set_entry` is the one driver request that writes app state: it stands in for the scripted fake agent provider of #28 until that lands (see crates/cod_e2e_driver/src/cod_tabs_state.rs). Everything else is read only or user input.
"""

from __future__ import annotations

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
