//! `codctl session` commands (story #39): new, list, send, wait and status, built on top of
//! `agent_ui::cod_sessions`, the small API the agent panel exposes for this.
//!
//! `new`, `list` and the `--workspace` of none of the others follow the pane/open/diff
//! convention of #38 (`target_workspace`): the named workspace, else `$COD_WORKSPACE`, else the
//! workspace shown in the active window. `send`, `wait` and `status` address a session only by its
//! own id, so they search every live workspace of every window for the chat with that id.

use std::{
    sync::{Arc, atomic::{AtomicBool, Ordering}},
    time::{Duration, Instant},
};

use agent_ui::cod_chat_status::CodChatStatus;
use anyhow::{Context as _, Result, bail};
use gpui::{App, AsyncApp, Entity, WindowHandle};
use serde_json::{Value, json};
use workspace::{MultiWorkspace, Workspace};

use crate::{members, target_workspace};

/// `codctl session new` (#39 AC1): starts the chat, then waits for the provider's own session id,
/// since the agent only assigns it once the connection to it is up (asynchronous even for the
/// fake agent of the tests).
pub async fn new_session(workspace_ref: Option<&str>, agent: String, cx: &mut AsyncApp) -> Result<Value> {
    let (window, workspace) = cx.update(|cx| target_workspace(workspace_ref, cx))?;
    // A workspace `codctl workspace new` made a moment ago attaches its agent panel
    // asynchronously; wait for it instead of failing a script that runs both commands in a row.
    // With AI disabled the panel never comes, and a workspace closed meanwhile must not get a
    // session nobody can address, so both are checked on every poll.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let ready = cx.update(|cx| -> Result<bool> {
            if agent_ui::cod_sessions::ai_disabled(cx) {
                bail!("AI is disabled in the settings, so the workspace has no agent panel");
            }
            let open = members(cx)
                .iter()
                .any(|member| member.window == window && member.live.as_ref() == Some(&workspace));
            if !open {
                bail!("the workspace was closed");
            }
            Ok(workspace.read(cx).panel::<agent_ui::AgentPanel>(cx).is_some())
        })?;
        if ready {
            break;
        }
        if Instant::now() >= deadline {
            bail!("the workspace has no agent panel");
        }
        cx.background_executor().timer(Duration::from_millis(20)).await;
    }
    let thread_id = window.update(cx, |_, window, cx| {
        agent_ui::cod_sessions::new_session(&workspace, &agent, window, cx)
    })??;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(id) = cx.update(|cx| agent_ui::cod_sessions::session_id_of(&workspace, thread_id, cx)) {
            return Ok(json!({ "id": id }));
        }
        if Instant::now() >= deadline {
            bail!("the agent never assigned a session id for the new chat");
        }
        cx.background_executor().timer(Duration::from_millis(20)).await;
    }
}

/// `codctl session list` (#39 AC2): one entry per session of the workspace, `--json` adding the
/// workspace id to each.
pub fn list_sessions(workspace_ref: Option<&str>, cx: &mut AsyncApp) -> Result<Value> {
    let (_, workspace) = cx.update(|cx| target_workspace(workspace_ref, cx))?;
    let workspace_id = cx.update(|cx| workspace.read(cx).database_id());
    let sessions = cx.update(|cx| agent_ui::cod_sessions::list_sessions(&workspace, cx));
    Ok(Value::Array(
        sessions
            .into_iter()
            .map(|info| {
                json!({
                    "id": info.id,
                    "agent": info.agent,
                    "state": info.state,
                    "name": info.name,
                    "workspace": workspace_id.map(i64::from),
                })
            })
            .collect(),
    ))
}

/// The window and workspace that holds the chat with this session id, searching every live
/// workspace of every window (#39: `send`, `wait` and `status` take a session id alone).
fn find_session_workspace(
    session_id: &str,
    cx: &App,
) -> Option<(WindowHandle<MultiWorkspace>, Entity<Workspace>)> {
    members(cx).into_iter().find_map(|member| {
        let workspace = member.live?;
        agent_ui::cod_sessions::has_session(&workspace, session_id, cx).then_some((member.window, workspace))
    })
}

/// `codctl session send` (#39 AC3): sends the text and returns at once.
pub async fn send_message(session_id: String, text: String, cx: &mut AsyncApp) -> Result<Value> {
    let Some((window, workspace)) = cx.update(|cx| find_session_workspace(&session_id, cx)) else {
        bail!("unknown session {session_id}");
    };
    window.update(cx, |_, window, cx| {
        agent_ui::cod_sessions::send_message(&workspace, &session_id, &text, window, cx)
    })??;
    Ok(json!({}))
}

/// Turns `--timeout` seconds into a `Duration`, never panicking (#A1): rejects negative,
/// non-finite (NaN, infinite) values and values too large for `Duration` to hold, instead of
/// letting `Duration::from_secs_f64` panic on them (`codctl session wait <id> --until idle
/// --timeout 1e30` used to crash Cod this way).
fn timeout_duration(secs: f64) -> Result<Duration> {
    if !secs.is_finite() || secs < 0.0 {
        bail!("--timeout must be a finite, non-negative number of seconds, not {secs}");
    }
    Duration::try_from_secs_f64(secs).map_err(|_| anyhow::anyhow!("--timeout is too large to represent"))
}

/// `codctl session wait` (#39 AC4): polls the session's state until it reaches `until`, the
/// timeout runs out, the session fails, or the client disconnects (RC review A6).
///
/// Without `--timeout` this can poll for as long as the session takes, which may be a very long
/// time; `cancel` is set by the server's client thread once it notices the client disconnected,
/// so this loop stops instead of running forever for a reply nobody will ever read.
pub async fn wait_session(
    session_id: String,
    until: String,
    timeout: Option<f64>,
    cancel: Arc<AtomicBool>,
    cx: &mut AsyncApp,
) -> Result<Value> {
    if !matches!(until.as_str(), "idle" | "needs-input") {
        bail!("--until must be idle or needs-input, not {until}");
    }
    let deadline = match timeout {
        Some(secs) => Some(
            Instant::now()
                .checked_add(timeout_duration(secs)?)
                .context("--timeout is too large to represent")?,
        ),
        None => None,
    };
    loop {
        if cancel.load(Ordering::Acquire) {
            bail!("the client disconnected while this command was still waiting");
        }
        let Some((window, workspace)) = cx.update(|cx| find_session_workspace(&session_id, cx)) else {
            bail!("unknown session {session_id}");
        };
        let Some((status, text)) = window.update(cx, |_, window, cx| {
            agent_ui::cod_sessions::session_snapshot(&workspace, &session_id, window, cx)
        })?
        else {
            bail!("unknown session {session_id}");
        };
        let reached = match until.as_str() {
            "idle" => matches!(status, CodChatStatus::Idle | CodChatStatus::Done),
            _ => status == CodChatStatus::NeedsInput,
        };
        if reached {
            return Ok(json!({ "state": agent_ui::cod_sessions::state_name(status), "text": text }));
        }
        if status == CodChatStatus::Error {
            bail!("the session failed");
        }
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            return Ok(json!({ "timeout": true }));
        }
        cx.background_executor().timer(Duration::from_millis(50)).await;
    }
}

/// `codctl session status` (#39 AC5): without a line, reports the state and the current line;
/// with one (an empty line clears it), sets the override and republishes the overview at once.
///
/// Reading the state reloads the session first if the idle-eviction cache had dropped it from
/// `retained_threads` (the same reload `wait` and `send` use): otherwise an evicted chat whose
/// turn actually ended with a reply (`done`) used to be reported `idle` instead.
pub async fn session_status(session_id: String, line: Option<String>, cx: &mut AsyncApp) -> Result<Value> {
    let Some((window, workspace)) = cx.update(|cx| find_session_workspace(&session_id, cx)) else {
        bail!("unknown session {session_id}");
    };
    if let Some(line) = line {
        cx.update(|cx| agent_ui::cod_sessions::set_line_override(&session_id, &line, cx));
        cx.update(|cx| agent_ui::cod_sessions::republish(&workspace, cx));
        return Ok(json!({}));
    }
    let (status, _text) = window
        .update(cx, |_, window, cx| {
            agent_ui::cod_sessions::session_snapshot(&workspace, &session_id, window, cx)
        })?
        .with_context(|| format!("unknown session {session_id}"))?;
    let line = cx
        .update(|cx| agent_ui::cod_sessions::line_override(&session_id, cx))
        .map(|line| line.to_string())
        .unwrap_or_else(|| status.cod_context().to_owned());
    Ok(json!({ "state": agent_ui::cod_sessions::state_name(status), "line": line }))
}

#[cfg(test)]
mod tests {
    use super::timeout_duration;
    use std::time::Duration;

    #[test]
    fn timeout_duration_rejects_what_duration_cannot_hold() {
        // #A1: `codctl session wait <id> --until idle --timeout 1e30` used to panic in
        // `Duration::from_secs_f64` here, on the app's foreground thread.
        assert!(timeout_duration(1e30).is_err());
        assert!(timeout_duration(f64::MAX).is_err());
        assert!(timeout_duration(f64::INFINITY).is_err());
        assert!(timeout_duration(f64::NAN).is_err());
        assert!(timeout_duration(-1.0).is_err());
    }

    #[test]
    fn timeout_duration_accepts_an_ordinary_timeout() {
        assert_eq!(timeout_duration(20.0).unwrap(), Duration::from_secs_f64(20.0));
        assert_eq!(timeout_duration(0.0).unwrap(), Duration::ZERO);
    }
}
