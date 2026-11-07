use crate::error::ApiError;
use codex_client::Request;
use codex_client::RequestTelemetry;
use codex_client::Response;
use codex_client::RetryPolicy;
use codex_client::StreamResponse;
use codex_client::TransportError;
use codex_client::run_with_retry;
use http::StatusCode;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::Instant;

/// Generic telemetry.
pub trait SseTelemetry: Send + Sync {
    fn on_sse_poll(
        &self,
        result: &Result<
            Option<
                Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<TransportError>,
                >,
            >,
            tokio::time::error::Elapsed,
        >,
        duration: Duration,
    );
}

/// Telemetry for Responses WebSocket transport.
pub trait WebsocketTelemetry: Send + Sync {
    /// Reports how long it took to send the request and whether the send failed.
    fn on_ws_request(&self, duration: Duration, error: Option<&ApiError>, connection_reused: bool);

    /// Reports the poll duration for timeouts, stream errors, end of stream,
    /// and binary, close, or raw frames.
    fn on_ws_receive_failure(&self, duration: Duration);

    /// Reports each text frame and its poll duration. `kind` is `None` if decoding the stream event failed.
    fn on_ws_text_event(&self, text: &str, kind: Option<&str>, duration: Duration);
}

pub(crate) trait WithStatus {
    fn status(&self) -> StatusCode;
    fn headers(&self) -> &http::HeaderMap;
}

fn http_status(err: &TransportError) -> Option<StatusCode> {
    match err {
        TransportError::Http { status, .. } => Some(*status),
        _ => None,
    }
}

impl WithStatus for Response {
    fn status(&self) -> StatusCode {
        self.status
    }
    fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }
}

impl WithStatus for StreamResponse {
    fn status(&self) -> StatusCode {
        self.status
    }
    fn headers(&self) -> &http::HeaderMap {
        &self.headers
    }
}

/// Hanzo: a provider's rate-limit headers pace the requests that follow (`hanzo::pace`).
fn observe(url: &str, status: StatusCode, headers: &http::HeaderMap) {
    hanzo::pace::observe(url, status.as_u16(), |name| {
        headers.get(name).and_then(|value| value.to_str().ok())
    });
}

pub(crate) async fn run_with_request_telemetry<T, F, Fut>(
    policy: RetryPolicy,
    telemetry: Option<Arc<dyn RequestTelemetry>>,
    make_request: impl FnMut() -> Request,
    send: F,
) -> Result<T, TransportError>
where
    T: WithStatus,
    F: Clone + Fn(Request) -> Fut,
    Fut: Future<Output = Result<T, TransportError>>,
{
    // Wraps `run_with_retry` to attach per-attempt request telemetry for both
    // unary and streaming HTTP calls.
    run_with_retry(policy, make_request, move |req, attempt| {
        let telemetry = telemetry.clone();
        let send = send.clone();
        async move {
            // Hanzo: wait out what the provider said is spent, then learn from its answer.
            let url = req.url.clone();
            let wait = hanzo::pace::delay(&url);
            if !wait.is_zero() {
                tokio::time::sleep(wait).await;
            }
            let start = Instant::now();
            let result = send(req).await;
            match &result {
                Ok(resp) => observe(&url, resp.status(), resp.headers()),
                Err(TransportError::Http {
                    status,
                    headers: Some(headers),
                    ..
                }) => observe(&url, *status, headers),
                Err(_) => {}
            }
            if let Some(t) = telemetry.as_ref() {
                let (status, err) = match &result {
                    Ok(resp) => (Some(resp.status()), None),
                    Err(err) => (http_status(err), Some(err)),
                };
                t.on_request(attempt, status, err, start.elapsed());
            }
            result
        }
    })
    .await
}
