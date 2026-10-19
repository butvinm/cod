//! Cod (#28): the state of an agent chat, as its chat tab dot, the workspace tab dot (#25) and the
//! overview row (#26) show it.
//!
//! Hook point: this is a local function until the status contract of PR #65
//! (`crates/workspace/src/multi_workspace/cod_status.rs`) is on master. Then the agent panel writes
//! these states into that contract for its workspace, from `AgentPanel::cod_chat_snapshot`.

use acp_thread::{AcpThread, AgentThreadEntry, ThreadStatus};

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
}

#[cfg(test)]
mod tests {
    use super::CodChatStatus;

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
