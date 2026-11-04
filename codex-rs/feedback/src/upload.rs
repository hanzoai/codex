use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;

use anyhow::Context;
use anyhow::Result;
use anyhow::anyhow;
use bytes::Bytes;
use codex_http_client::RequestBuilder;
use codex_http_client::RouteAwareClientPool;
use http::HeaderMap;
use http::StatusCode;

pub(super) const DEFAULT_RATE_LIMIT: Duration = Duration::from_secs(/*secs*/ 60);

pub(super) enum PayloadKind {
    Event,
    Attachment,
}

pub(super) fn event_request(
    client_pool: &RouteAwareClientPool,
    endpoint: &str,
    key: &str,
    body: Bytes,
    timeout: Duration,
) -> RequestBuilder {
    client_pool
        .post(endpoint)
        .header("Authorization", format!("Bearer {key}"))
        .header("Content-Type", "application/json")
        .body(body)
        .timeout(timeout)
}

/// Send an already-serialized `/v1/event` JSON payload within the report's shared deadline.
pub(super) async fn send_hanzo_event(
    client_pool: &RouteAwareClientPool,
    endpoint: &str,
    key: &str,
    body: Vec<u8>,
    kind: PayloadKind,
    deadline: Instant,
    rate_limited: &mut bool,
) -> Result<StatusCode> {
    let body = Bytes::from(body);
    // Retain the exact request bytes for at most two diagnostic retries. Never replay the core.
    let mut retry_delays = [250, 500].map(Duration::from_millis).into_iter();
    let response = loop {
        let timeout = deadline.saturating_duration_since(Instant::now());
        anyhow::ensure!(!timeout.is_zero(), "feedback upload deadline exceeded");
        let response = event_request(client_pool, endpoint, key, body.clone(), timeout)
            .send()
            .await;
        if response.as_ref().is_ok_and(|response| {
            rate_limit_delay(response.headers()).is_some_and(|delay| !delay.is_zero())
                || (response.status().is_success()
                    && response.headers().get("Retry-After").is_some_and(|value| {
                        parse_retry_after(value.to_str().unwrap_or_default())
                            .is_none_or(|delay| !delay.is_zero())
                    }))
        }) {
            *rate_limited = true;
            break response;
        }
        let retryable = match &response {
            Ok(response) => matches!(response.status().as_u16(), 408 | 429 | 500..=599),
            Err(error) => {
                error.is_timeout()
                    || (error.failure_class().is_none()
                        && (error.is_connect() || error.is_request() || error.is_body()))
            }
        };
        if matches!(kind, PayloadKind::Event) || !retryable {
            break response;
        }
        let retry_delay = retry_delays.next();
        if response.as_ref().is_ok_and(|response| {
            response.status() == StatusCode::TOO_MANY_REQUESTS
                && (retry_delay.is_none() || !response.headers().contains_key("Retry-After"))
        }) {
            // Never let later files bypass an exhausted rate limit.
            *rate_limited = true;
            break response;
        }
        let Some(retry_delay) = retry_delay else {
            break response;
        };
        let retry_after = response
            .as_ref()
            .ok()
            .and_then(|response| response.headers().get("Retry-After"))
            .map(|value| {
                // An invalid cooldown is not permission to send immediately.
                parse_retry_after(value.to_str().unwrap_or_default())
                    .unwrap_or_else(|| deadline.saturating_duration_since(Instant::now()))
            });
        let delay = retry_delay.max(retry_after.unwrap_or_default());
        if delay >= deadline.saturating_duration_since(Instant::now()) {
            // Leave long cooldowns for a later submission, including its remaining files.
            *rate_limited = true;
            break response;
        }
        tokio::time::sleep(delay).await;
    };
    response
        .map(|response| response.status())
        .context("failed to upload feedback to Hanzo")
}

pub(super) fn rate_limit_delay(headers: &HeaderMap) -> Option<Duration> {
    let mut retry_after = None;
    for value in headers.get_all("X-Sentry-Rate-Limits") {
        for quota in value.to_str().unwrap_or_default().split(',') {
            let mut fields = quota.trim().split(':');
            let seconds = fields.next().unwrap_or_default();
            let categories = fields.next().unwrap_or_default();
            if !categories.is_empty()
                && !categories
                    .split(';')
                    .any(|category| matches!(category, "error" | "attachment"))
            {
                continue;
            }
            let delay = seconds
                .parse::<f64>()
                .ok()
                .and_then(|seconds| Duration::try_from_secs_f64(seconds.ceil()).ok())
                .unwrap_or(DEFAULT_RATE_LIMIT);
            retry_after = retry_after.max(Some(delay));
        }
    }
    retry_after
}

pub(super) fn parse_retry_after(value: &str) -> Option<Duration> {
    value
        .parse::<u64>()
        .map(Duration::from_secs)
        .or_else(|_| {
            httpdate::parse_http_date(value)
                .map(|date| date.duration_since(SystemTime::now()).unwrap_or_default())
        })
        .ok()
}

pub(super) fn sanitize_transport_error(error: anyhow::Error) -> anyhow::Error {
    if let Some(cause) = error.downcast_ref::<codex_http_client::RouteAwareRequestError>() {
        anyhow!(
            "feedback transport failed: class={:?}, timeout={}, connect={}, request={}, body={}",
            cause.failure_class(),
            cause.is_timeout(),
            cause.is_connect(),
            cause.is_request(),
            cause.is_body()
        )
    } else {
        error
    }
}
