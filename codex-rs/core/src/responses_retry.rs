//! Shared retry and transport fallback decisions for Responses requests.
//! Content-filter guidance is recorded for sampling requests before retry decisions.
//! Server advice controls timing without extending configured retry limits.

use std::time::Duration;

use crate::client::ModelClientSession;
use crate::context::ContentFilterGuidance;
use crate::context::ContextualUserFragment;
use crate::session::session::Session;
use crate::session::step_context::StepContext;
use crate::session::turn_context::TurnContext;
use codex_client::RetryOperation;
use codex_features::Feature;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::WarningEvent;
use tokio::time::Instant;
use tracing::warn;

const INITIAL_CONNECTION_RETRY_DELAY: Duration = Duration::from_secs(5);
const MAX_CONNECTION_RETRY_DELAY: Duration = Duration::from_secs(60);
/// Hanzo: the first wait for a rate limit or an overload without `Retry-After`, doubled
/// per wait up to [`MAX_SLOW_DOWN_DELAY`], each one drawn at random below that (full jitter).
const SLOW_DOWN_BASE_DELAY: Duration = Duration::from_secs(1);
const MAX_SLOW_DOWN_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy)]
pub(crate) enum ResponsesStreamRequest {
    Sampling,
    RemoteCompactionV2,
}

pub(crate) struct ResponsesStreamRetryState {
    retries: u64,
    connection_retries: u64,
    connection_retry_delay: Duration,
    /// Hanzo: waits for a rate limit or an overload, which no retry budget caps.
    waits: u64,
}

impl Default for ResponsesStreamRetryState {
    fn default() -> Self {
        Self {
            retries: 0,
            connection_retries: 0,
            connection_retry_delay: INITIAL_CONNECTION_RETRY_DELAY,
            waits: 0,
        }
    }
}

/// Hanzo: whether an active sampling turn is talking to the Hanzo gateway, where transient
/// upstream/stream blips must never abort the developer's flow.
fn is_hanzo_sampling(turn_context: &TurnContext, request: ResponsesStreamRequest) -> bool {
    matches!(request, ResponsesStreamRequest::Sampling)
        && !turn_context.session_source.is_internal()
        && matches!(
            turn_context.config.model_provider_id.as_str(),
            "hanzo" | "hanzocode"
        )
}

/// Hanzo: what a provider says when it is asking to be called less, not refusing: a 429 that
/// is not a spent plan or quota, `slow_down`, and an overloaded or unavailable server. Each is
/// waited out for as long as the turn lives, and named as a wait, never as an error.
fn slow_down(err: &CodexErr, hanzo_sampling: bool) -> Option<&'static str> {
    match err.details() {
        CodexErrorDetails::RateLimitExceeded(_) => Some("waiting for rate limit"),
        CodexErrorDetails::RetryLimit(limit) => match limit.status.as_u16() {
            429 => Some("waiting for rate limit"),
            502 | 503 | 520..=529 => Some("waiting for server capacity"),
            408 | 500 | 504 if hanzo_sampling => Some("waiting for server recovery"),
            _ => None,
        },
        CodexErrorDetails::UnexpectedStatus(response) => match response.status.as_u16() {
            429 => Some("waiting for rate limit"),
            502 | 503 | 520..=529 => Some("waiting for server capacity"),
            _ => None,
        },
        CodexErrorDetails::ServerOverloaded | CodexErrorDetails::PolicyCapacityUnavailable => {
            Some("waiting for server capacity")
        }
        _ => None,
    }
}

async fn wait_for_recovery(
    retry_state: &mut ResponsesStreamRetryState,
    err: &CodexErr,
    reason: &'static str,
    sess: &Session,
    turn_context: &TurnContext,
    operation: RetryOperation,
) {
    retry_state.waits = retry_state.waits.saturating_add(1);
    let now = Instant::now();
    let retry_at = err
        .retry_after()
        .map(RetryAfter::deadline)
        .unwrap_or_else(|| now + slow_down_delay(retry_state.waits));
    let delay = retry_at.saturating_duration_since(now);
    warn!(
        turn_id = %turn_context.sub_id,
        waits = retry_state.waits,
        ?delay,
        error = %err,
        "{reason}"
    );
    let seconds = delay.as_secs() + u64::from(delay.subsec_nanos() > 0);
    sess.notify_retry_wait(turn_context, format!("{reason}, retrying in {seconds}s"))
        .await;
    codex_client::record_retry!(retry_state.waits, delay, operation);
    tokio::time::sleep_until(retry_at).await;
}

/// Hanzo: the `wait`th wait without server advice: uniform in `[0, min(cap, base * 2^(wait-1))]`.
fn slow_down_delay(wait: u64) -> Duration {
    let doublings = u32::try_from(wait.saturating_sub(1))
        .unwrap_or(u32::MAX)
        .min(16);
    let ceiling = SLOW_DOWN_BASE_DELAY
        .saturating_mul(1 << doublings)
        .min(MAX_SLOW_DOWN_DELAY);
    ceiling.mul_f64(rand::random::<f64>())
}

/// Server retry advice retained after stream retries are exhausted. The turn ID
/// prevents a reused Guardian session from applying advice from an earlier review.
pub(crate) struct ExhaustedResponseRetry {
    pub(crate) turn_id: String,
    pub(crate) retry_at: Option<tokio::time::Instant>,
}

/// Returns `Ok(())` when the caller should retry the request loop, or the original error when
/// it is terminal or the retry budget is exhausted.
#[tracing::instrument(level = "trace", skip_all, fields(request = ?request))]
pub(crate) async fn handle_response_stream_error(
    retry_state: &mut ResponsesStreamRetryState,
    max_retries: u64,
    err: CodexErr,
    client_session: &mut ModelClientSession,
    sess: &Session,
    step_context: &StepContext,
    request: ResponsesStreamRequest,
) -> Result<(), CodexErr> {
    let turn_context = &step_context.turn;
    if matches!(request, ResponsesStreamRequest::Sampling)
        && matches!(err.details(), CodexErrorDetails::ContentFilter)
    {
        let model_info = &step_context.settings.model_info;
        let guidance = ContentFilterGuidance {
            text: codex_prompts::ResolvedModelMessages::from_model(model_info)
                .content_filter_guidance()
                .to_string(),
        };
        sess.record_conversation_items(
            turn_context,
            model_info,
            &[ContextualUserFragment::into(guidance)],
        )
        .await;
    }
    let operation = match request {
        ResponsesStreamRequest::Sampling => RetryOperation::Sampling,
        ResponsesStreamRequest::RemoteCompactionV2 => RetryOperation::RemoteCompactionV2,
    };
    let hanzo_sampling = is_hanzo_sampling(turn_context, request);
    if let Some(reason) = slow_down(&err, hanzo_sampling) {
        wait_for_recovery(retry_state, &err, reason, sess, turn_context, operation).await;
        return Ok(());
    }

    let retry_count = retry_state.retries.saturating_add(1);
    let Some(delay) = err.retry_delay(retry_count) else {
        return Err(err);
    };
    let retry_after = err.retry_after();

    if turn_context
        .config
        .features
        .enabled(Feature::UnboundedConnectionRetries)
        && matches!(request, ResponsesStreamRequest::Sampling)
        && matches!(err.details(), CodexErrorDetails::ConnectionFailed(_))
        && !turn_context.session_source.is_internal()
        && !turn_context.provider.info().is_amazon_bedrock()
    {
        let retry_delay = retry_state.connection_retry_delay;
        warn!(
            turn_id = %turn_context.sub_id,
            error = %err,
            ?retry_delay,
            "stream connection failed; waiting to retry"
        );
        sess.notify_stream_error(turn_context, "Reconnecting... waiting for network", err)
            .await;
        retry_state.connection_retries = retry_state.connection_retries.saturating_add(1);
        codex_client::record_retry!(retry_state.connection_retries, retry_delay, operation);
        tokio::time::sleep(retry_delay).await;
        retry_state.connection_retry_delay = retry_delay
            .saturating_mul(2)
            .min(MAX_CONNECTION_RETRY_DELAY);
        return Ok(());
    }

    let requires_http_fallback = err.requires_http_fallback();
    let switched = (requires_http_fallback || retry_state.retries >= max_retries)
        && client_session.try_switch_fallback_transport(
            &turn_context.session_telemetry,
            turn_context.model_info(),
        );
    if switched {
        // Changing transport must not bypass the server's retry deadline.
        if let Some(retry_after) = retry_after {
            tokio::time::sleep_until(retry_after.deadline()).await;
        }
        sess.send_event(
            turn_context,
            EventMsg::Warning(WarningEvent {
                message: format!("Falling back from WebSockets to HTTPS transport. {err:#}"),
            }),
        )
        .await;
    }
    // Another request can already have switched this session to HTTP.
    if switched || requires_http_fallback {
        retry_state.retries = 0;
        return Ok(());
    }

    if retry_state.retries < max_retries {
        retry_state.retries = retry_count;
        log_retry(request, turn_context, &err, retry_count, max_retries, delay);

        // In release builds, hide the first websocket retry notification to reduce noisy
        // transient reconnect messages. In debug builds, keep full visibility for diagnosis.
        let report_error = retry_count > 1
            || cfg!(debug_assertions)
            || !sess.services.model_client.responses_websocket_enabled();
        if report_error {
            // Surface retry information to any UI/front-end so the user understands what is
            // happening instead of staring at a seemingly frozen screen.
            sess.notify_stream_error(
                turn_context,
                format!("Reconnecting... {retry_count}/{max_retries}"),
                err,
            )
            .await;
        }
        // Use one clock sample so local backoff telemetry retains the selected delay.
        let now = Instant::now();
        let retry_at = retry_after.map(RetryAfter::deadline).unwrap_or(now + delay);
        let delay = retry_at.saturating_duration_since(now);
        codex_client::record_retry!(retry_count, delay, operation);
        tokio::time::sleep_until(retry_at).await;
        return Ok(());
    }

    if hanzo_sampling {
        wait_for_recovery(
            retry_state,
            &err,
            "waiting for server recovery",
            sess,
            turn_context,
            operation,
        )
        .await;
        return Ok(());
    }

    sess.services
        .thread_extension_data
        .insert(ExhaustedResponseRetry {
            turn_id: turn_context.sub_id.clone(),
            retry_at: retry_after.map(RetryAfter::deadline),
        });
    Err(err)
}

fn log_retry(
    request: ResponsesStreamRequest,
    turn_context: &TurnContext,
    err: &CodexErr,
    retries: u64,
    max_retries: u64,
    delay: Duration,
) {
    match request {
        ResponsesStreamRequest::Sampling => {
            warn!(
                turn_id = %turn_context.sub_id,
                retries,
                max_retries,
                sampling_error = %err,
                "stream disconnected - retrying sampling request ({retries}/{max_retries} in {delay:?})...",
            );
        }
        ResponsesStreamRequest::RemoteCompactionV2 => {
            warn!(
                turn_id = %turn_context.sub_id,
                retries,
                max_retries,
                compact_error = %err,
                "remote compaction v2 stream failed; retrying request after delay"
            );
        }
    }
}

#[cfg(test)]
#[path = "responses_retry_tests.rs"]
mod tests;
