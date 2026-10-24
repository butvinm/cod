//! Cod (#28): the state of an agent chat, as its chat tab dot, the workspace tab dot (#25) and the
//! overview row (#26) show it.
//!
//! The agent panel writes these states into the status contract
//! (`crates/workspace/src/multi_workspace/cod_status.rs`) for its workspace, from
//! `AgentPanel::cod_publish_chat_statuses`, as a `workspace::CodEntry` of kind `Session`
//! (`cod_status`, `cod_context`).

use acp_thread::{AcpThread, AgentThreadEntry, ThreadStatus};
use workspace::CodStatus;

/// What a chat is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodChatStatus {
    /// Nothing was asked yet.
    Idle,
    /// The agent streams a reply or runs tools.
    Working,
    /// The agent waits for the user, for example for a permission.
    NeedsInput,
    /// The last turn ended.
    Done,
    /// The last turn failed.
    Error,
}

impl CodChatStatus {
    /// The state from what the thread reports. Waiting for the user comes first, since the agent
    /// is still generating while it asks; a failed turn is an error until the next turn starts.
    pub fn from_parts(
        generating: bool,
        waiting_for_user: bool,
        failed: bool,
        has_reply: bool,
    ) -> Self {
        if waiting_for_user {
            Self::NeedsInput
        } else if generating {
            Self::Working
        } else if failed {
            Self::Error
        } else if has_reply {
            Self::Done
        } else {
            Self::Idle
        }
    }

    /// The state of a thread.
    pub fn of_thread(thread: &AcpThread) -> Self {
        let has_reply = thread
            .entries()
            .iter()
            .any(|entry| matches!(entry, AgentThreadEntry::AssistantMessage(_)));
        Self::from_parts(
            thread.status() == ThreadStatus::Generating,
            thread.is_waiting_for_confirmation(),
            thread.had_error(),
            has_reply,
        )
    }

    /// The name the end-to-end driver reports.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::NeedsInput => "needs_input",
            Self::Done => "done",
            Self::Error => "error",
        }
    }

    /// The status of the workspace status contract (#25, #26) this chat state means.
    pub fn cod_status(self) -> CodStatus {
        match self {
            Self::Idle => CodStatus::Idle,
            Self::Working => CodStatus::Working,
            Self::NeedsInput => CodStatus::NeedsInput,
            Self::Done => CodStatus::Done,
            Self::Error => CodStatus::Error,
        }
    }

    /// The short status line a `CodEntry` of this chat carries as its context (its own words,
    /// not an agent-set context line: that is #39, out of scope here).
    pub fn cod_context(self) -> &'static str {
        match self {
            Self::Idle => "",
            Self::Working => "working",
            Self::NeedsInput => "waiting for you",
            Self::Done => "done",
            Self::Error => "failed",
        }
    }
}

/// The chat's name shown in its tab, the workspace tab's tooltip and the overview row: the
/// agent's label, then the thread's own title once it has one (#28 open question: default is
/// the agent name plus the thread title Zed already makes, for example "Claude: fix invoice
/// rounding").
pub fn cod_chat_title(agent_label: &str, thread_title: Option<&str>) -> String {
    match thread_title {
        Some(title) if !title.is_empty() => format!("{agent_label}: {title}"),
        _ => agent_label.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{CodChatStatus, cod_chat_title};
    use workspace::CodStatus;

    #[test]
    fn cod_status_follows_the_chat_status_urgency() {
        assert_eq!(CodChatStatus::NeedsInput.cod_status(), CodStatus::NeedsInput);
        assert_eq!(CodChatStatus::Error.cod_status(), CodStatus::Error);
        assert_eq!(CodChatStatus::Working.cod_status(), CodStatus::Working);
        assert_eq!(CodChatStatus::Done.cod_status(), CodStatus::Done);
        assert_eq!(CodChatStatus::Idle.cod_status(), CodStatus::Idle);
    }

    #[test]
    fn cod_context_is_a_short_status_line() {
        assert_eq!(CodChatStatus::NeedsInput.cod_context(), "waiting for you");
        assert_eq!(CodChatStatus::Working.cod_context(), "working");
        assert_eq!(CodChatStatus::Done.cod_context(), "done");
        assert_eq!(CodChatStatus::Error.cod_context(), "failed");
        assert_eq!(CodChatStatus::Idle.cod_context(), "");
    }

    #[test]
    fn the_title_is_the_agent_label_then_the_thread_title() {
        assert_eq!(
            cod_chat_title("Claude", Some("fix invoice rounding")),
            "Claude: fix invoice rounding"
        );
        assert_eq!(cod_chat_title("Claude", None), "Claude");
        assert_eq!(cod_chat_title("Claude", Some("")), "Claude");
    }

    #[test]
    fn a_chat_without_a_turn_is_idle() {
        assert_eq!(
            CodChatStatus::from_parts(false, false, false, false),
            CodChatStatus::Idle
        );
    }

    #[test]
    fn a_streaming_reply_is_working() {
        assert_eq!(
            CodChatStatus::from_parts(true, false, false, true),
            CodChatStatus::Working
        );
    }

    #[test]
    fn a_permission_question_needs_input_while_the_turn_runs() {
        assert_eq!(
            CodChatStatus::from_parts(true, true, false, true),
            CodChatStatus::NeedsInput
        );
    }

    #[test]
    fn an_ended_turn_is_done() {
        assert_eq!(
            CodChatStatus::from_parts(false, false, false, true),
            CodChatStatus::Done
        );
    }

    #[test]
    fn a_failed_turn_is_an_error_until_the_next_turn_runs() {
        assert_eq!(
            CodChatStatus::from_parts(false, false, true, true),
            CodChatStatus::Error
        );
        assert_eq!(
            CodChatStatus::from_parts(true, false, true, true),
            CodChatStatus::Working
        );
    }
}
