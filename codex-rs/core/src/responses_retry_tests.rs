use super::ResponsesStreamRequest;
use super::ResponsesStreamRetryState;
use super::handle_response_stream_error;
use super::log_retry;
use super::slow_down;
use crate::realtime_history::RealtimeHistoryState;
use crate::session::step_context::StepContext;
use crate::session::tests::make_session_and_context;
use codex_http_client::RetryAfter;
use codex_protocol::error::CodexErr;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::RetryLimitReachedError;
use codex_protocol::error::UnexpectedResponseError;
use http::StatusCode;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;
use tokio::time::Instant;
use tracing_test::internal::MockWriter;

#[tokio::test]
async fn sampling_retry_logs_stream_error_context() {
    let (_session, turn_context) = make_session_and_context().await;
    let buffer: &'static std::sync::Mutex<Vec<u8>> =
        Box::leak(Box::new(std::sync::Mutex::new(Vec::new())));
    let subscriber = tracing_subscriber::fmt()
        .with_ansi(false)
        .with_max_level(tracing::Level::WARN)
        .with_writer(MockWriter::new(buffer))
        .finish();
    let _subscriber_guard = tracing::subscriber::set_default(subscriber);

    log_retry(
        ResponsesStreamRequest::Sampling,
        &turn_context,
        &CodexErr::Stream("websocket closed by server before response.completed".to_string()),
        /*retries*/ 2,
        /*max_retries*/ 5,
        Duration::from_secs(1),
    );

    let logs = String::from_utf8(
        buffer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone(),
    )
    .expect("retry log should be valid utf-8");
    assert!(logs.contains("stream disconnected - retrying sampling request"));
    assert!(logs.contains(&format!("turn_id={}", turn_context.sub_id)));
    assert!(logs.contains("retries=2"));
    assert!(logs.contains("max_retries=5"));
    assert!(logs.contains(
        "sampling_error=stream disconnected before completion: websocket closed by server before response.completed"
    ));
}

/// Time spent reporting a retry must count toward the server's original deadline.
#[tokio::test]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "test holds the event-delivery lock to delay notification while virtual time advances"
)]
async fn stream_retry_preserves_deadline_across_delayed_notification() {
    let (mut session, turn_context) = make_session_and_context().await;
    let step_context = StepContext::for_test(Arc::new(turn_context));
    session.realtime_history = Some(Mutex::new(RealtimeHistoryState::default()));
    let mut client_session = session.services.model_client.new_session();
    // The second retry emits a notification even when release builds hide the first.
    let mut retry_state = ResponsesStreamRetryState {
        retries: 1,
        ..Default::default()
    };

    tokio::time::pause();
    let advice = RetryAfter::from_delay(Duration::from_secs(10)).expect("retry deadline");
    tokio::time::advance(Duration::from_secs(4)).await;

    // Hold the event-delivery lock so reporting the error consumes part of the deadline.
    let history_guard = session.realtime_history.as_ref().unwrap().lock().await;
    let retry = handle_response_stream_error(
        &mut retry_state,
        /*max_retries*/ 2,
        CodexErr::InternalServerError.with_retry_after(advice),
        &mut client_session,
        &session,
        &step_context,
        ResponsesStreamRequest::Sampling,
    );
    tokio::pin!(retry);
    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(4)).await;
    drop(history_guard);

    assert!(futures::poll!(&mut retry).is_pending());
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(futures::poll!(&mut retry).is_pending());
    retry.await.expect("retry should be allowed");
    // Tokio rounds timer deadlines up to the next millisecond.
    let resumed_at = Instant::now();
    assert!(
        (advice.deadline()..=advice.deadline() + Duration::from_millis(1)).contains(&resumed_at),
        "retry resumed at {resumed_at:?}, expected {advice:?}"
    );
}

#[test]
fn slow_down_waits_out_rate_limits_and_capacity_exhaustion_including_retry_limit() {
    let retry_limit = |code: u16| -> CodexErr {
        CodexErrorDetails::RetryLimit(RetryLimitReachedError {
            status: StatusCode::from_u16(code).unwrap(),
            request_id: None,
        })
        .into()
    };
    let unexpected = |code: u16| -> CodexErr {
        CodexErrorDetails::UnexpectedStatus(UnexpectedResponseError {
            status: StatusCode::from_u16(code).unwrap(),
            body: String::new(),
            user_message: None,
            url: None,
            cf_ray: None,
            request_id: None,
            identity_authorization_error: None,
            identity_error_code: None,
        })
        .into()
    };

    assert_eq!(
        slow_down(&retry_limit(429), false),
        Some("waiting for rate limit")
    );
    assert_eq!(
        slow_down(&retry_limit(502), false),
        Some("waiting for server capacity")
    );
    assert_eq!(
        slow_down(&retry_limit(503), false),
        Some("waiting for server capacity")
    );
    assert_eq!(
        slow_down(&retry_limit(529), false),
        Some("waiting for server capacity")
    );
    assert_eq!(
        slow_down(&unexpected(502), false),
        Some("waiting for server capacity")
    );
    assert_eq!(
        slow_down(&unexpected(529), false),
        Some("waiting for server capacity")
    );

    // Hanzo sampling also waits out exhausted HTTP 408/500/504 bursts without aborting the turn.
    assert_eq!(slow_down(&retry_limit(500), false), None);
    assert_eq!(
        slow_down(&retry_limit(500), true),
        Some("waiting for server recovery")
    );
    assert_eq!(
        slow_down(&retry_limit(504), true),
        Some("waiting for server recovery")
    );

    // Auth, payment, and client errors never enter unbounded wait.
    for terminal in [400, 401, 402, 403, 404] {
        assert_eq!(slow_down(&unexpected(terminal), true), None);
    }
}
