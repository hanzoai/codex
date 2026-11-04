use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Result;
use anyhow::anyhow;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::RouteAwareClientPool;
use codex_protocol::protocol::SessionSource;
use http::StatusCode;
use uuid::Uuid;

use crate::FeedbackAttachment;
use crate::FeedbackSnapshot;
use crate::MAX_DECODED_UPLOAD_BYTES;
use crate::MAX_EVENT_BYTES;
use crate::encode_attachment_payload;
use crate::upload;

// Background parts must tolerate slow links without inheriting the interactive
// upload's ten-second budget. The report API can cancel the request at any time.
const REPORT_PART_UPLOAD_TIMEOUT: Duration = Duration::from_secs(/*secs*/ 300);

/// The upstream HTTP result, not proof of durable storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackDelivery {
    Accepted {
        retry_after: Option<Duration>,
    },
    Rejected {
        status: u16,
        retry_after: Option<Duration>,
    },
    /// No response was received. The upstream may still have accepted the payload.
    Unconfirmed,
}

/// Sends persisted report parts without retries or request diagnostics.
pub struct FeedbackTransport {
    client_pool: RouteAwareClientPool,
    endpoint: &'static str,
    key: &'static str,
}

impl FeedbackTransport {
    pub fn new(http_client_factory: HttpClientFactory) -> Result<Self> {
        Ok(Self {
            client_pool: RouteAwareClientPool::new_without_redirects_or_request_logging(
                http_client_factory,
                ClientRouteClass::Other,
            ),
            endpoint: hanzo::FEEDBACK_ENDPOINT,
            key: hanzo::FEEDBACK_KEY,
        })
    }

    /// Make one attempt. Callers own retry decisions and must retain the same bytes.
    pub async fn send(&self, payload: Vec<u8>) -> FeedbackDelivery {
        let response = self
            .client_pool
            .post(self.endpoint)
            .header("Authorization", format!("Bearer {}", self.key))
            .header("Content-Type", "application/json")
            .body(payload)
            .timeout(REPORT_PART_UPLOAD_TIMEOUT)
            .send()
            .await;
        let Ok(response) = response else {
            return FeedbackDelivery::Unconfirmed;
        };
        let status = response.status();
        let headers = response.headers();
        let mut retry_after = headers
            .get_all("Retry-After")
            .iter()
            .map(|value| {
                upload::parse_retry_after(value.to_str().unwrap_or_default())
                    .unwrap_or(upload::DEFAULT_RATE_LIMIT)
            })
            .max();
        retry_after = retry_after.max(upload::rate_limit_delay(headers));
        if status == StatusCode::TOO_MANY_REQUESTS && retry_after.is_none() {
            retry_after = Some(upload::DEFAULT_RATE_LIMIT);
        }
        if status.is_success() {
            FeedbackDelivery::Accepted { retry_after }
        } else {
            FeedbackDelivery::Rejected {
                status: status.as_u16(),
                retry_after,
            }
        }
    }
}

impl FeedbackSnapshot {
    /// Prepare only the core report, using the caller's stable UUID as its event ID.
    pub fn prepare_report_event(
        &self,
        report_id: &str,
        classification: &str,
        reason: Option<&str>,
        tags: Option<&BTreeMap<String, String>>,
        session_source: Option<&SessionSource>,
    ) -> Result<Vec<u8>> {
        let mut event = self.feedback_event(classification, reason, tags, session_source);
        // Auth tags must come from this report's caller, not metadata retained
        // from an earlier account in the process-wide tracing layer.
        event.tags.retain(|key, _| {
            !matches!(key.as_str(), "account_id" | "chatgpt_user_id")
                || tags.is_some_and(|tags| tags.contains_key(key))
        });
        event.event_id =
            Uuid::parse_str(report_id).map_err(|_| anyhow!("invalid feedback report ID"))?;
        let bytes = event.to_hanzo_event_bytes(&self.thread_id, classification)?;
        anyhow::ensure!(
            bytes.len() <= MAX_EVENT_BYTES,
            "feedback event exceeds the size limit"
        );
        Ok(bytes)
    }
}

/// Prepare one whole attachment linked to an existing report, without replaying its event.
/// The caller must apply consent and the report's cumulative attachment budget first.
pub fn prepare_report_attachment(
    report_id: &str,
    attachment: FeedbackAttachment,
) -> Result<Vec<u8>> {
    let event_id = Uuid::parse_str(report_id).map_err(|_| anyhow!("invalid feedback report ID"))?;
    anyhow::ensure!(
        attachment.buffer.len() <= MAX_DECODED_UPLOAD_BYTES,
        "feedback attachment exceeds the size limit"
    );
    let event_id_str = event_id.to_string();
    encode_attachment_payload(
        &event_id_str,
        &event_id_str,
        &event_id_str,
        None,
        "bug",
        attachment,
    )
}
