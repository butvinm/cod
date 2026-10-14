//! Spike-only driver, enabled with COD_SPIKE_DRIVE=offscreen together with COD_SPIKE_URL.

use gpui::{Context, Entity, Window};
use workspace::Workspace;

use crate::offscreen::OffscreenView;

pub(crate) fn start(
    _page: Entity<OffscreenView>,
    _window: &mut Window,
    _cx: &mut Context<Workspace>,
) {
}
