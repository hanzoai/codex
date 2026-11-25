//! Per-session Hanzo safety guards attached to `Session::services::thread_extension_data`:
//! - [`ReadOnlyTurnGuard`]: enforces explicit user read-only / wait-for-confirmation prompts.
//! - [`FileTruncationGuard`]: snapshots non-empty source files, restores accidental `0`-byte
//!   truncations, and trips a circuit breaker on empty or looping `.bak` restores.

use std::sync::Arc;

use codex_protocol::models::ResponseItem;
use codex_protocol::user_input::UserInput;
use codex_tools::ToolName;
use hanzo::file_guard::FileTruncationGuard;
use hanzo::readonly_guard::READ_ONLY_DEVELOPER_REMINDER;
use hanzo::readonly_guard::ReadOnlyTurnGuard;
use hanzo::readonly_guard::blocked_tool_message;
use hanzo::readonly_guard::is_mutating_tool_call;

use crate::context::ContextualUserFragment;
use crate::context::InternalContextSource;
use crate::context::InternalModelContextFragment;
use crate::function_tool::FunctionCallError;
use crate::session::session::Session;
use crate::tools::context::ToolPayload;

pub(crate) fn readonly_guard(session: &Session) -> Arc<ReadOnlyTurnGuard> {
    session
        .services
        .thread_extension_data
        .get_or_init(ReadOnlyTurnGuard::new)
}

pub(crate) fn file_guard(session: &Session) -> Arc<FileTruncationGuard> {
    session
        .services
        .thread_extension_data
        .get_or_init(FileTruncationGuard::new)
}

pub(crate) fn is_readonly_guard_active(session: &Session) -> bool {
    session
        .services
        .thread_extension_data
        .get::<ReadOnlyTurnGuard>()
        .is_some_and(|guard| guard.is_active())
}

/// Updates the session's [`ReadOnlyTurnGuard`] from genuine `UserInput::Text` items
/// (automatic hook/controller continuations carry no `UserInput` and preserve the lock).
pub(crate) fn update_readonly_guard_from_user_input(
    session: &Session,
    user_input: &[UserInput],
) -> bool {
    let text = user_input
        .iter()
        .filter_map(|item| match item {
            UserInput::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return is_readonly_guard_active(session);
    }
    readonly_guard(session).on_user_turn(&text)
}

/// Developer reminder injected into turn context while [`ReadOnlyTurnGuard`] is active.
pub(crate) fn readonly_reminder_item() -> ResponseItem {
    ContextualUserFragment::into(InternalModelContextFragment::new(
        InternalContextSource::from_static("hanzo_readonly_guard"),
        READ_ONLY_DEVELOPER_REMINDER.to_string(),
    ))
}

/// Deterministically gates mutating tool calls while [`ReadOnlyTurnGuard`] is active.
pub(crate) fn check_tool_allowed_in_turn(
    session: &Session,
    tool_name: &ToolName,
    payload: &ToolPayload,
) -> Result<(), FunctionCallError> {
    if !is_readonly_guard_active(session) {
        return Ok(());
    }
    let payload_text = match payload {
        ToolPayload::Function { arguments } => arguments.as_str(),
        ToolPayload::Custom { input } => input.as_str(),
        ToolPayload::ToolSearch { .. } => return Ok(()),
    };
    if let Some(reason) =
        is_mutating_tool_call(tool_name.namespace.as_deref(), &tool_name.name, payload_text)
    {
        return Err(FunctionCallError::RespondToModel(blocked_tool_message(
            &reason,
        )));
    }
    Ok(())
}

#[cfg(test)]
#[path = "hanzo_guards_tests.rs"]
mod tests;
