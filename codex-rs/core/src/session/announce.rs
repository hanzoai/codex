//! Hanzo: a turn whose model ends on an announcement of its next step, with no tool
//! call, is asked once to make the call it announced (hanzo-inc/cloud#759).
//!
//! A sampling request that makes no tool call ends the turn. enso-auto often answers a
//! tool error with a message that only says what comes next ("Now I have the base hash.
//! Applying all 5 config.toml edits."), so `dev exec` reported `task_complete` with
//! nothing applied. [`hanzo::announce::announces`] reads the message; the turn is then
//! continued with [`hanzo::announce::FINISH`], recorded the way a Stop hook's
//! continuation is.
//!
//! One ask in a row: a step that calls a tool lets the next announcement be asked about
//! again, and an announcement that answers the ask ends the turn. Only turns on Hanzo
//! that a person or `dev exec` started are asked; plan mode ends on a plan by design.

use codex_protocol::config_types::ModeKind;
use codex_protocol::ItemIdGenerator;
use codex_protocol::items::HookPromptFragment;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;

/// The run id the ask is recorded under, in place of a hook's.
pub(crate) const ASK_RUN_ID: &str = "hanzo-finish-turn";

/// The turn's view of its asks.
#[derive(Debug, Default)]
pub(crate) struct Announce {
    asked: bool,
}

impl Announce {
    /// A step called a tool: the next announcement may be asked about.
    pub(crate) fn acted(&mut self) {
        self.asked = false;
    }

    /// The message to continue the turn with when it ended on `last_agent_message` and
    /// that only announced a step. `None` ends the turn.
    pub(crate) fn ask(
        &mut self,
        applies: bool,
        last_agent_message: Option<&str>,
        item_ids: &ItemIdGenerator,
    ) -> Option<ResponseItem> {
        if !applies || self.asked || !last_agent_message.is_some_and(hanzo::announce::announces)
        {
            return None;
        }
        let ask = build_hook_prompt_message(
            &[HookPromptFragment::from_single_hook(
                hanzo::announce::FINISH,
                ASK_RUN_ID,
            )],
            item_ids,
        )?;
        self.asked = true;
        Some(ask)
    }
}

/// Whether a turn is asked: it runs on Hanzo (the provider, its base URL, or a Hanzo
/// model), outside plan mode, in a session a person, `dev exec` or a spawned agent
/// started. A review, a compaction or memory work ends on its report.
pub(crate) fn applies(
    provider_id: &str,
    base_url: Option<&str>,
    model: &str,
    source: &SessionSource,
    mode: ModeKind,
) -> bool {
    let works = match source {
        SessionSource::Internal(_) => false,
        SessionSource::SubAgent(agent) => matches!(agent, SubAgentSource::ThreadSpawn { .. }),
        _ => true,
    };
    works
        && mode != ModeKind::Plan
        && (matches!(provider_id, "hanzo" | "hanzocode" | "local")
            || hanzo::is_hanzo(base_url)
            || hanzo::catalog::find(model).is_some())
}

#[cfg(test)]
#[path = "announce_tests.rs"]
mod tests;
