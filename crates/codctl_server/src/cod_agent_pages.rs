//! `codctl open --html` (story #43): opens local HTML as an agent-made page tab, and `--wait`
//! waits for it to be submitted or its tab closed.
//!
//! The heavy lifting (reading the file, stripping inline scripts unless `--js`, injecting Cod's
//! bootstrap script and theme variables, and the `data:` URL the page loads) lives in
//! `web_view::page_control` and `web_view`'s own `agent_bridge` module; this file only resolves
//! the target workspace and shapes the reply the way `codctl`'s `render` expects it.

use std::path::PathBuf;

use anyhow::Result;
use gpui::AsyncApp;
use serde_json::{Value, json};

use crate::target_workspace;

/// Opens the HTML file as an agent-made page tab (#43 AC1) and, with `wait`, waits for it to be
/// submitted or closed (#43 AC2) before answering.
pub async fn open_agent_page(
    reference: Option<&str>,
    html: PathBuf,
    wait: bool,
    js: bool,
    cx: &mut AsyncApp,
) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(reference, cx))?;
    let id = window.update(cx, |_, window, cx| {
        workspace.update(cx, |workspace, cx| {
            web_view::page_control::open_html(workspace, &html, js, window, cx)
        })
    })??;
    if !wait {
        return Ok(json!({ "id": id }));
    }
    match web_view::page_control::wait_for_submit(&id, cx).await? {
        web_view::page_control::SubmitOutcome::Submitted(value) => {
            Ok(json!({ "id": id, "value": value }))
        }
        web_view::page_control::SubmitOutcome::Closed => Ok(json!({ "id": id, "closed": true })),
    }
}
