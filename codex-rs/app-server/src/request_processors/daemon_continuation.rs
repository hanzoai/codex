//! Attempts one new continuation turn immediately after daemon thread restoration.
//! Completed or superseded work is discarded; recovery never waits for a later idle event.
//! Permissions and the saved local executor selection must match the resumed runtime.

use super::ThreadRequestProcessor;
use codex_app_server_protocol::ThreadEnvironment;
use codex_app_server_transport::daemon_recovery::InterruptedTurn;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_core::TurnStartOptions;
use codex_core::context::ContextualUserFragment;
use codex_core::context::InternalContextSource;
use codex_core::context::InternalModelContextFragment;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EnvironmentConfigState;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use codex_rollout::RolloutItem;

impl ThreadRequestProcessor {
    pub(crate) async fn continue_daemon_turn(&self, thread_id: &str, saved: InterruptedTurn) {
        let Ok(thread_id) = ThreadId::from_string(thread_id) else {
            return;
        };
        let Ok(thread) = self.thread_manager.get_thread(thread_id).await else {
            return;
        };
        let Some(path) = thread.rollout_path() else {
            return;
        };
        let history = match codex_rollout::RolloutRecorder::load_rollout_items(&path).await {
            Ok((history, _, _)) => history,
            Err(err) => {
                tracing::warn!(%thread_id, %err, "failed to read interrupted turn history");
                return;
            }
        };
        // The snapshot has complete recorded input, but the old process may have
        // completed or replaced its turn during the shutdown grace period.
        let Some(start) = history
            .iter()
            .rposition(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnStarted(_))))
        else {
            return;
        };
        if !matches!(&history[start], RolloutItem::EventMsg(EventMsg::TurnStarted(event))
            if event.turn_id == saved.turn_id)
            || history[start..].iter().any(|item| match item {
                RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => {
                    event.turn_id == saved.turn_id
                }
                RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => event
                    .turn_id
                    .as_ref()
                    .is_none_or(|turn_id| turn_id == &saved.turn_id),
                _ => false,
            })
        {
            return;
        }
        let previous_context = history.iter().rev().find_map(|item| match item {
            RolloutItem::TurnContext(context)
                if context.turn_id.as_ref() == Some(&saved.turn_id) =>
            {
                Some(context)
            }
            _ => None,
        });
        let Some(previous_context) = previous_context else {
            return;
        };
        let config = thread.config_snapshot().await;
        let [environment] = config.environment_selections() else {
            return;
        };
        if environment.environment_id != codex_exec_server::LOCAL_ENVIRONMENT_ID
            || environment.config != EnvironmentConfigState::FromThread
            || saved.local_environment.as_ref() != Some(&ThreadEnvironment::from(environment))
        {
            return;
        }
        // Recovery must not override a stricter saved or newly configured policy.
        if previous_context.permission_profile != config.permission_profile {
            return;
        }
        let continuation = ContextualUserFragment::into(InternalModelContextFragment::new(
            InternalContextSource::from_static("daemon_recovery"),
            "The server restarted and interrupted the previous turn. Continue the unfinished work from the saved conversation. Check the current state before repeating actions that may already have completed.",
        ));
        let request = TurnInputRequest::new(TurnInput::ResponseItem(continuation)).on_start(
            TurnStartOptions {
                turn_trigger: Some("daemon_recovery".to_string()),
                final_output_json_schema: saved.output_schema,
                service_tier: saved.service_tier,
                cyber_access_program: saved.cyber_access_program,
                root_turn_id: previous_context.root_turn_id.clone(),
                ..Default::default()
            },
        );
        // Close the old turn in persisted history before exposing a new running turn.
        if let Err(err) = thread
            .append_rollout_items(&[RolloutItem::EventMsg(EventMsg::TurnAborted(
                TurnAbortedEvent {
                    root_turn_id: None,
                    turn_id: Some(saved.turn_id.clone()),
                    reason: TurnAbortReason::Interrupted,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            ))])
            .await
        {
            tracing::warn!(%thread_id, %err, "failed to close interrupted turn history");
            return;
        }
        match thread.continue_turn_if_idle(request, saved.turn_id).await {
            Ok(TurnInputSubmission::Started { .. }) => {
                crate::extensions::send_thread_warning(
                    &self.outgoing,
                    &self.thread_state_manager,
                    thread_id,
                    "Resuming interrupted work".to_string(),
                )
                .await;
            }
            Ok(TurnInputSubmission::NotSubmitted { reason }) => {
                tracing::debug!(%thread_id, ?reason, "recovery continuation was not started");
            }
            Ok(TurnInputSubmission::Steered { .. } | TurnInputSubmission::Accepted) => {
                // continue_turn_if_idle uses start_if_idle, which cannot steer or
                // enter the agent-message path that returns Accepted.
                unreachable!("continuation must start or decline")
            }
            Err(err) => tracing::warn!(%thread_id, %err, "recovery continuation failed"),
        }
    }

    /// Recovers a thread whose last persisted `TurnStarted` has no terminal `TurnComplete`
    /// or `TurnAborted` (e.g. after `SIGKILL`, power loss, or OOM without a graceful daemon snapshot).
    pub(crate) async fn continue_unclosed_rollout_turn(&self, thread_id: ThreadId) -> bool {
        let Ok(thread) = self.thread_manager.get_thread(thread_id).await else {
            return false;
        };
        let Some(path) = thread.rollout_path() else {
            return false;
        };
        let history = match codex_rollout::RolloutRecorder::load_rollout_items(&path).await {
            Ok((history, _, _)) => history,
            Err(err) => {
                tracing::warn!(%thread_id, %err, "failed to read rollout for crash turn recovery");
                return false;
            }
        };
        let Some((crashed_turn_id, previous_context)) = find_unclosed_rollout_turn(&history) else {
            return false;
        };
        let config = thread.config_snapshot().await;
        let [environment] = config.environment_selections() else {
            return false;
        };
        if environment.environment_id != codex_exec_server::LOCAL_ENVIRONMENT_ID
            || environment.config != EnvironmentConfigState::FromThread
        {
            return false;
        }
        // Recovery must not override a stricter saved or newly configured policy.
        if previous_context.permission_profile != config.permission_profile {
            return false;
        }
        let continuation = ContextualUserFragment::into(InternalModelContextFragment::new(
            InternalContextSource::from_static("crash_recovery"),
            "The previous process terminated before finishing this turn. Continue the unfinished work from the saved conversation. Check the current workspace state before repeating actions that may already have completed.",
        ));
        let request = TurnInputRequest::new(TurnInput::ResponseItem(continuation)).on_start(
            TurnStartOptions {
                turn_trigger: Some("crash_recovery".to_string()),
                service_tier: config.service_tier.clone(),
                cyber_access_program: previous_context.cyber_access_program,
                root_turn_id: previous_context.root_turn_id.clone(),
                ..Default::default()
            },
        );
        if let Err(err) = thread
            .append_rollout_items(&[RolloutItem::EventMsg(EventMsg::TurnAborted(
                TurnAbortedEvent {
                    root_turn_id: previous_context.root_turn_id.clone(),
                    turn_id: Some(crashed_turn_id.clone()),
                    reason: TurnAbortReason::Interrupted,
                    error: None,
                    started_at: None,
                    completed_at: None,
                    duration_ms: None,
                },
            ))])
            .await
        {
            tracing::warn!(%thread_id, %err, "failed to close crashed turn history");
            return false;
        }
        match thread.continue_turn_if_idle(request, crashed_turn_id).await {
            Ok(TurnInputSubmission::Started { .. }) => {
                crate::extensions::send_thread_warning(
                    &self.outgoing,
                    &self.thread_state_manager,
                    thread_id,
                    "Resuming interrupted work after crash".to_string(),
                )
                .await;
                true
            }
            Ok(TurnInputSubmission::NotSubmitted { reason }) => {
                tracing::debug!(%thread_id, ?reason, "crash recovery continuation was not started");
                false
            }
            Ok(TurnInputSubmission::Steered { .. } | TurnInputSubmission::Accepted) => {
                unreachable!("continuation must start or decline")
            }
            Err(err) => {
                tracing::warn!(%thread_id, %err, "crash recovery continuation failed");
                false
            }
        }
    }
}

fn find_unclosed_rollout_turn(
    history: &[RolloutItem],
) -> Option<(String, &codex_protocol::protocol::TurnContextItem)> {
    let start = history
        .iter()
        .rposition(|item| matches!(item, RolloutItem::EventMsg(EventMsg::TurnStarted(_))))?;
    let RolloutItem::EventMsg(EventMsg::TurnStarted(started)) = &history[start] else {
        return None;
    };
    let crashed_turn_id = started.turn_id.clone();
    if history[start..].iter().any(|item| match item {
        RolloutItem::EventMsg(EventMsg::TurnComplete(event)) => event.turn_id == crashed_turn_id,
        RolloutItem::EventMsg(EventMsg::TurnAborted(event)) => event
            .turn_id
            .as_deref()
            .is_none_or(|turn_id| turn_id == crashed_turn_id.as_str()),
        _ => false,
    }) {
        return None;
    }
    let previous_context = history.iter().rev().find_map(|item| match item {
        RolloutItem::TurnContext(context)
            if context.turn_id.as_deref() == Some(crashed_turn_id.as_str()) =>
        {
            Some(context)
        }
        _ => None,
    })?;
    Some((crashed_turn_id, previous_context))
}

#[cfg(test)]
mod tests {
    use super::find_unclosed_rollout_turn;
    use codex_protocol::config_types::ModeKind;
    use codex_protocol::models::PermissionProfile;
    use codex_protocol::protocol::AskForApproval;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::TurnAbortReason;
    use codex_protocol::protocol::TurnAbortedEvent;
    use codex_protocol::protocol::TurnCompleteEvent;
    use codex_protocol::protocol::TurnContextItem;
    use codex_protocol::protocol::TurnStartedEvent;
    use codex_rollout::RolloutItem;
    use codex_utils_absolute_path::test_support::PathBufExt;
    use codex_utils_absolute_path::test_support::test_path_buf;

    fn turn_ctx(turn_id: &str) -> RolloutItem {
        RolloutItem::TurnContext(TurnContextItem {
            turn_id: Some(turn_id.to_string()),
            root_turn_id: Some(format!("root-{turn_id}")),
            direct_only_tool_namespaces: None,
            disabled_plugin_ids: None,
            cwd: test_path_buf("/tmp").abs(),
            approval_policy: AskForApproval::Never,
            approvals_reviewer: None,
            permission_profile: PermissionProfile::Disabled,
            active_permission_profile: None,
            model: "zen6-nano".to_string(),
            comp_hash: None,
            collaboration_mode: None,
            multi_agent_version: None,
            realtime_active: None,
            cyber_access_program: None,
            effort: None,
            summary: None,
        })
    }

    fn turn_started(turn_id: &str) -> RolloutItem {
        RolloutItem::EventMsg(EventMsg::TurnStarted(TurnStartedEvent {
            turn_attribution: None,
            turn_id: turn_id.to_string(),
            root_turn_id: Some(format!("root-{turn_id}")),
            trace_id: None,
            started_at: Some(1_700_000_000),
            model_context_window: None,
            collaboration_mode_kind: ModeKind::Default,
        }))
    }

    #[test]
    fn find_unclosed_rollout_turn_detects_crashed_turn_and_ignores_closed_turns() {
        let completed_turn_1 = vec![
            turn_ctx("turn-1"),
            turn_started("turn-1"),
            RolloutItem::EventMsg(EventMsg::TurnComplete(TurnCompleteEvent {
                root_turn_id: Some("root-turn-1".to_string()),
                turn_id: "turn-1".to_string(),
                last_agent_message: Some("done".to_string()),
                error: None,
                started_at: Some(10),
                completed_at: Some(20),
                duration_ms: Some(10_000),
                time_to_first_token_ms: None,
            })),
        ];
        assert!(find_unclosed_rollout_turn(&completed_turn_1).is_none());

        let mut crashed_turn_2 = completed_turn_1.clone();
        crashed_turn_2.push(turn_ctx("turn-2"));
        crashed_turn_2.push(turn_started("turn-2"));
        let (found_id, ctx) =
            find_unclosed_rollout_turn(&crashed_turn_2).expect("turn-2 should be unclosed");
        assert_eq!(found_id, "turn-2");
        assert_eq!(ctx.root_turn_id.as_deref(), Some("root-turn-2"));

        let mut aborted_turn_2 = crashed_turn_2;
        aborted_turn_2.push(RolloutItem::EventMsg(EventMsg::TurnAborted(
            TurnAbortedEvent {
                root_turn_id: Some("root-turn-2".to_string()),
                turn_id: Some("turn-2".to_string()),
                reason: TurnAbortReason::Interrupted,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            },
        )));
        assert!(find_unclosed_rollout_turn(&aborted_turn_2).is_none());
    }
}
