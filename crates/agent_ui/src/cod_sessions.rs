//! codctl sessions (#39): the small API `codctl_server` calls into to start, list, message and
//! wait on the chats of a workspace's agent panel from outside the window, and to set a session's
//! status line override in the overview (#26).
//!
//! Kept separate from `cod_chat_status.rs` (#28), which owns a chat's own status words from its
//! thread state; this file only adds what #39 needs on top of that: starting a chat with a named
//! agent id (not by clicking through the new-thread menu), finding a chat by its provider session
//! id among the chats of one workspace, sending a message into it without the keyboard, reading
//! its last reply or pending prompt for `codctl session wait`, and an operator's status line that
//! overrides the chat's own status words until it is changed or cleared.
//!
//! Every function takes the workspace as an `Entity` handle and does its own `read`/`update` on
//! it, so a caller never has to pre-borrow the workspace itself: that would conflict with also
//! passing `cx` by the mutable reference these functions need for the window and the entities.

use std::collections::{HashMap, HashSet};

use acp_thread::{AcpThread, AgentThreadEntry, ThreadStatus, ToolCallStatus};
use agent_client_protocol::schema::v1 as acp_v1;
use anyhow::{Context as _, Result, bail};
use gpui::{App, AppContext as _, Entity, Global, SharedString, Task, Window};
use project::AgentId;
use workspace::Workspace;

use crate::{Agent, AgentInitialContent, AgentPanel, ThreadId, cod_chat_status::CodChatStatus};

/// One session of a workspace, as `codctl session list` reports it (#39 AC2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The provider's own session id.
    pub id: String,
    /// The chat's raw agent id, for example `claude`.
    pub agent: String,
    /// `working`, `needs-input`, `done`, `error` or `idle` (#39 AC2; hyphenated, unlike
    /// `CodChatStatus::as_str`'s `needs_input`, which is the end-to-end driver's own name).
    pub state: &'static str,
    pub name: String,
    /// The session's current status line: the operator's override, else the chat's own status
    /// words (#39 AC5).
    pub line: String,
}

/// The state name `codctl` reports for this chat status (#39 AC2, AC4).
pub fn state_name(status: CodChatStatus) -> &'static str {
    match status {
        CodChatStatus::Idle => "idle",
        CodChatStatus::Working => "working",
        CodChatStatus::NeedsInput => "needs-input",
        CodChatStatus::Done => "done",
        CodChatStatus::Error => "error",
    }
}

/// Starts a fresh chat with this agent id in the workspace's agent panel (#39 AC1): the same
/// action the new-thread menu's external agent entries run, so the fake agent registered under
/// `claude` or `codex` in the test settings starts exactly as a real external agent would.
///
/// Zed 1.23.2's agent panel has no chat-as-pane-tab UI, so `--pane` has no effect yet: every
/// session opens in the agent panel (an open question for the product owner, not decided here).
pub fn new_session(
    workspace: &Entity<Workspace>,
    agent_id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Result<ThreadId> {
    let panel = workspace
        .read(cx)
        .panel::<AgentPanel>(cx)
        .context("the workspace has no agent panel")?;
    let agent = Agent::from(AgentId(SharedString::from(agent_id.to_owned())));
    panel.update(cx, |panel, cx| {
        panel.force_new_external_agent_thread(agent, window, cx);
    });
    let thread_id = panel
        .read(cx)
        .active_thread_id(cx)
        .context("no chat opened after starting the session")?;
    cx.default_global::<RequestedSessions>().0.insert(thread_id);
    Ok(thread_id)
}

/// The chats `codctl session new` started (#39 AC1). The caller asked for each of them, so an
/// empty one stays a chat tab instead of being dropped as an unused draft (#28 AC1) when the next
/// session opens.
#[derive(Default)]
struct RequestedSessions(HashSet<ThreadId>);

impl Global for RequestedSessions {}

/// Whether `codctl session new` started this chat.
pub fn is_requested_session(thread_id: ThreadId, cx: &App) -> bool {
    cx.try_global::<RequestedSessions>()
        .is_some_and(|sessions| sessions.0.contains(&thread_id))
}

/// The provider session id of this chat, once the agent has assigned one. `codctl session new`
/// polls this after `new_session` above, since the session negotiation finishes asynchronously
/// (#39 AC1).
pub fn session_id_of(workspace: &Entity<Workspace>, thread_id: ThreadId, cx: &App) -> Option<String> {
    let panel = workspace.read(cx).panel::<AgentPanel>(cx)?;
    panel
        .read(cx)
        .cod_chat_tabs(cx)
        .into_iter()
        .find(|tab| tab.thread_id == thread_id)
        .map(|tab| tab.session_id)
}

/// Every session of the workspace (#39 AC2).
pub fn list_sessions(workspace: &Entity<Workspace>, cx: &App) -> Vec<SessionInfo> {
    let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
        return Vec::new();
    };
    panel
        .read(cx)
        .cod_chat_tabs(cx)
        .into_iter()
        .map(|tab| {
            let line = line_override(&tab.session_id, cx)
                .map(|line| line.to_string())
                .unwrap_or_else(|| tab.status.cod_context().to_owned());
            SessionInfo {
                id: tab.session_id,
                agent: tab.agent_id,
                state: state_name(tab.status),
                name: tab.title,
                line,
            }
        })
        .collect()
}

/// The chat of the workspace with this provider session id, if any.
fn find_chat(
    workspace: &Entity<Workspace>,
    session_id: &str,
    cx: &App,
) -> Option<Entity<crate::ConversationView>> {
    let panel = workspace.read(cx).panel::<AgentPanel>(cx)?;
    panel.read(cx).conversation_views().into_iter().find(|view| {
        view.read(cx)
            .root_thread(cx)
            .is_some_and(|thread| thread.read(cx).session_id().0.to_string() == session_id)
    })
}

/// Brings this session back into `retained_threads` if the idle-eviction cache had dropped it,
/// so a caller that needs its real transcript or status (not just to send into it) sees the chat
/// itself instead of a placeholder. Does nothing when the chat is already resident, or when the
/// workspace never owned this session id.
fn ensure_resident(workspace: &Entity<Workspace>, session_id: &str, window: &mut Window, cx: &mut App) {
    if find_chat(workspace, session_id, cx).is_some() {
        return;
    }
    if let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) {
        panel.update(cx, |panel, cx| {
            panel.cod_ensure_session_resident(session_id, window, cx)
        });
    }
}

/// Sends the text as the user's message in this session and starts the agent (#39 AC3), the same
/// way the message box's own submit does, but without ever touching the box's live contents.
///
/// Setting the box's text through `insert_text` (at the user's cursor) and sending used to
/// submit the draft the user had not sent yet together with the requested text, then clear the
/// box: an unsent draft was silently sent along with the command and lost (#C2). Clearing the
/// box, inserting the requested text and restoring the draft afterwards does not fix this: the
/// box's contents are read by a task spawned when sending starts, not read synchronously by it,
/// so a draft restored right after that call returns can still be swept up by that task once it
/// runs. `ThreadView::send_content` takes finished content blocks directly and never reads the
/// box at all, so the draft is never at risk.
///
/// `send_content` always starts a turn right away; calling it unconditionally used to interrupt
/// an active turn on the legacy transport and overtake whatever the user had already queued
/// behind it (RC review C2 new defect #2). While the thread is generating, or already has
/// messages waiting, this instead queues the codctl message the same way the message box's own
/// non-empty send does once busy (`ThreadView::add_to_queue`): with its own content only, never
/// reading or clearing the user's draft, and in the same FIFO order as everything else queued.
pub fn send_message(
    workspace: &Entity<Workspace>,
    session_id: &str,
    text: &str,
    window: &mut Window,
    cx: &mut App,
) -> Result<()> {
    let contents: Vec<acp_v1::ContentBlock> =
        vec![acp_v1::ContentBlock::Text(acp_v1::TextContent::new(text.to_owned()))];
    if let Some(view) = find_chat(workspace, session_id, cx) {
        let thread_view = view
            .read(cx)
            .root_thread_view()
            .context("the chat has no message box yet")?;
        thread_view.update(cx, |view, cx| {
            let is_generating = view.thread.read(cx).status() != ThreadStatus::Idle;
            if is_generating || !view.message_queue.is_empty() {
                view.add_to_queue(contents, Vec::new(), window, cx);
            } else {
                view.send_content(Task::ready(Ok(Some((contents, Vec::new())))), false, window, cx);
            }
        });
        return Ok(());
    }
    // RC review C8 new defect #2: a session the idle-eviction cache dropped from
    // `retained_threads` is still owned by the workspace, but connecting to its agent and
    // loading its history both resolve asynchronously, never within this same call; looking the
    // chat up again right after starting that reload (as this used to) raced it and reported
    // "unknown session" before it ever had a chance to finish. Handing the reload this text as
    // its `AgentInitialContent` instead sends it the moment the chat actually becomes ready,
    // exactly as `ThreadView::send` sends an ordinary message, with no race to lose.
    let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
        bail!("unknown session {session_id}");
    };
    // A reload already under way takes no second initial content, so this text would be dropped
    // while the command reported success: refuse it and let the caller send again.
    if panel.read(cx).cod_session_reload_in_flight(session_id, cx) {
        bail!("session {session_id} is still loading; send the message again when it is ready");
    }
    let initial_content = AgentInitialContent::ContentBlock {
        blocks: contents,
        auto_submit: true,
    };
    let started = panel.update(cx, |panel, cx| {
        panel.cod_ensure_session_resident_with_content(session_id, Some(initial_content), window, cx)
    });
    if !started {
        bail!("unknown session {session_id}");
    }
    Ok(())
}

/// The agent's last reply text, the most recent assistant message of the thread.
fn last_reply(thread: &AcpThread, cx: &App) -> Option<String> {
    thread.entries().iter().rev().find_map(|entry| match entry {
        AgentThreadEntry::AssistantMessage(message) => Some(message.to_markdown(cx)),
        _ => None,
    })
}

/// The prompt text of the thread's pending permission request, if it has one.
fn pending_prompt(thread: &AcpThread, cx: &App) -> Option<String> {
    thread.entries().iter().rev().find_map(|entry| match entry {
        AgentThreadEntry::ToolCall(tool_call)
            if matches!(tool_call.status(), ToolCallStatus::WaitingForConfirmation) =>
        {
            Some(tool_call.label.read(cx).source().to_string())
        }
        _ => None,
    })
}

/// The session's current state and the text that goes with it: the pending prompt while it waits
/// for input, else the last reply (#39 AC4). None when the session is unknown to this workspace.
///
/// A session the idle-eviction cache dropped from `retained_threads` is brought back resident
/// first (the same reload `send_message` already does), so this reports its real status and
/// transcript, not a placeholder. `cleanup_retained_threads` evicts any chat whose turn ended,
/// including one with a real reply (`CodChatStatus::Done`), not only a chat that never had one
/// (`CodChatStatus::Idle`); reporting a reloaded `Done` chat as `Idle` with an empty transcript
/// used to hide that reply from `codctl session wait` and `codctl session status` alike.
pub fn session_snapshot(
    workspace: &Entity<Workspace>,
    session_id: &str,
    window: &mut Window,
    cx: &mut App,
) -> Option<(CodChatStatus, String)> {
    ensure_resident(workspace, session_id, window, cx);
    if let Some(view) = find_chat(workspace, session_id, cx)
        && let Some(thread_entity) = view.read(cx).root_thread(cx)
    {
        let thread = thread_entity.read(cx);
        let status = CodChatStatus::of_thread(thread);
        let text = if status == CodChatStatus::NeedsInput {
            pending_prompt(thread, cx).unwrap_or_default()
        } else {
            last_reply(thread, cx).unwrap_or_default()
        };
        return Some((status, text));
    }
    // The session is owned by the workspace but its chat is not fully resident yet: either
    // `ensure_resident` just started reloading it (connecting to its agent and loading its
    // session history both resolve asynchronously, never within this same call) or it has no
    // chat entity at all.
    let panel = workspace.read(cx).panel::<AgentPanel>(cx)?;
    let panel = panel.read(cx);
    if !panel.cod_owns_session(session_id, cx) {
        return None;
    }
    // RC review new defect #3: a reload that already ended in `LoadError` (no agent executable,
    // an unsupported agent, and so on) has no root thread coming; reporting it `Working` left
    // `codctl session wait` polling forever for a status that would never arrive. Report a
    // terminal `Error` instead, so `wait` stops and exits non-zero, and `status` says so too.
    if panel.cod_session_load_failed(session_id, cx) {
        return Some((CodChatStatus::Error, String::new()));
    }
    // Still connecting or loading: report `Working`, not a terminal `Idle`, so `wait` keeps
    // polling and `status` shows the chat as busy becoming available rather than falsely idle.
    Some((CodChatStatus::Working, String::new()))
}

/// Whether the workspace has a chat with this session id, for `codctl session status` (#39 AC5,
/// AC6). Also true for a session the idle-eviction cache dropped from `retained_threads` (RC
/// review C8): workspace membership (`cod_owns_session`) does not depend on that cache.
pub fn has_session(workspace: &Entity<Workspace>, session_id: &str, cx: &App) -> bool {
    let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) else {
        return false;
    };
    panel.read(cx).cod_owns_session(session_id, cx)
}

/// Re-publishes the workspace's chat statuses right away (#39 AC5): called after a status line
/// override changes, so the overview shows it without waiting for the chat's own state to change.
pub fn republish(workspace: &Entity<Workspace>, cx: &mut App) {
    if let Some(panel) = workspace.read(cx).panel::<AgentPanel>(cx) {
        panel.update(cx, |panel, cx| panel.cod_republish_chat_statuses(cx));
    }
}

/// The status line overrides `codctl session status --line` sets (#39 AC5), keyed by the
/// provider's own session id. Not persisted across a restart (#39 open question: the default is
/// that a restart clears it, the same as every other chat status).
#[derive(Default)]
struct LineOverrides(HashMap<String, SharedString>);

struct GlobalLineOverrides(Entity<LineOverrides>);

impl Global for GlobalLineOverrides {}

fn overrides(cx: &mut App) -> Entity<LineOverrides> {
    if let Some(global) = cx.try_global::<GlobalLineOverrides>() {
        return global.0.clone();
    }
    let entity = cx.new(|_| LineOverrides::default());
    cx.set_global(GlobalLineOverrides(entity.clone()));
    entity
}

/// The status line override of this session, if one is set.
pub fn line_override(session_id: &str, cx: &App) -> Option<SharedString> {
    cx.try_global::<GlobalLineOverrides>()
        .and_then(|global| global.0.read(cx).0.get(session_id).cloned())
}

/// Sets the session's status line override, or clears it when `line` is empty (#39 AC5).
pub fn set_line_override(session_id: &str, line: &str, cx: &mut App) {
    let entity = overrides(cx);
    entity.update(cx, |store, _cx| {
        if line.is_empty() {
            store.0.remove(session_id);
        } else {
            store
                .0
                .insert(session_id.to_owned(), SharedString::from(line.to_owned()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::state_name;
    use crate::cod_chat_status::CodChatStatus;

    #[test]
    fn state_names_are_hyphenated_for_codctl() {
        assert_eq!(state_name(CodChatStatus::NeedsInput), "needs-input");
        assert_eq!(state_name(CodChatStatus::Idle), "idle");
        assert_eq!(state_name(CodChatStatus::Working), "working");
        assert_eq!(state_name(CodChatStatus::Done), "done");
        assert_eq!(state_name(CodChatStatus::Error), "error");
    }
}
