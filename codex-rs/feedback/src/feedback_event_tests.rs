//! Feedback event contracts for titles, report grouping, and lossless comment bodies.

use crate::CodexFeedback;
use crate::FeedbackEvent;
use crate::FeedbackException;
use crate::FeedbackLevel;
use crate::FeedbackUploadOptions;
use crate::UPLOAD_TIMEOUT;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use http::StatusCode;
use pretty_assertions::assert_eq;
use std::collections::BTreeMap;
use std::time::Instant;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::header;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test]
fn custom_titles_preserve_comments_and_keep_submissions_separate() {
    let snapshot = CodexFeedback::new().snapshot(/*session_id*/ None);
    let tags = BTreeMap::from([
        (
            "feedback_title".to_string(),
            " \t\u{2003}Workflow  feedback\nwith 🌍 context\r\n ".to_string(),
        ),
        ("entrypoint".to_string(), "task-picker".to_string()),
    ]);
    let title = "Workflow  feedback\nwith 🌍 context";
    for (reason, reason_tag) in [
        (
            Some("  The\n\nworkflow\twas\u{2003}useful 🌍.  "),
            Some("  The  workflow\twas\u{2003}useful 🌍.  "),
        ),
        (Some(""), Some("")),
        (None, None),
    ] {
        let event =
            snapshot.feedback_event("other", reason, Some(&tags), /*session_source*/ None);
        let mut expected_tags = tags.clone();
        expected_tags.extend([
            ("thread_id".to_string(), snapshot.thread_id.clone()),
            ("classification".to_string(), "other".to_string()),
            (
                "cli_version".to_string(),
                hanzo::version::get().to_string(),
            ),
        ]);
        if let Some(reason_tag) = reason_tag {
            expected_tags.insert("reason".to_string(), reason_tag.to_string());
        }
        assert_eq!(
            event,
            FeedbackEvent {
                event_id: event.event_id,
                level: FeedbackLevel::Info,
                message: Some(title.to_string()),
                exception: reason
                    .map(|reason| FeedbackException {
                        ty: title.to_string(),
                        value: Some(reason.to_string()),
                    })
                    .into_iter()
                    .collect(),
                tags: expected_tags,
                fingerprint: vec![event.event_id.to_string()],
            }
        );
        let next_event =
            snapshot.feedback_event("other", reason, Some(&tags), /*session_source*/ None);
        assert_ne!(event.fingerprint, next_event.fingerprint);
    }
}

#[test]
fn missing_or_blank_titles_preserve_session_titles_and_default_grouping() {
    let snapshot = CodexFeedback::new().snapshot(/*session_id*/ None);
    let reason = "  Feedback\nwith 🌍 context.  ";
    for custom_title in [None, Some(""), Some(" \t\n\u{2003} ")] {
        let tags = custom_title
            .map(|title| BTreeMap::from([("feedback_title".to_string(), title.to_string())]));
        let event = snapshot.feedback_event(
            "bug",
            Some(reason),
            tags.as_ref(),
            /*session_source*/ None,
        );
        let title = format!("[Bug]: Hanzo Dev session {}", snapshot.thread_id);
        let mut expected_tags = tags.unwrap_or_default();
        expected_tags.extend([
            ("thread_id".to_string(), snapshot.thread_id.clone()),
            ("classification".to_string(), "bug".to_string()),
            (
                "cli_version".to_string(),
                hanzo::version::get().to_string(),
            ),
            (
                "reason".to_string(),
                "  Feedback with 🌍 context.  ".to_string(),
            ),
        ]);
        assert_eq!(
            event,
            FeedbackEvent {
                event_id: event.event_id,
                level: FeedbackLevel::Error,
                message: Some(title.clone()),
                exception: vec![FeedbackException {
                    ty: title,
                    value: Some(reason.to_string()),
                }],
                tags: expected_tags,
                ..Default::default()
            }
        );
    }
}

#[test]
fn reason_tag_previews_preserve_full_comments_in_the_exception() {
    let snapshot = CodexFeedback::new().snapshot(/*session_id*/ None);
    let title = format!("[Bug]: Hanzo Dev session {}", snapshot.thread_id);
    for (reason, preview) in [
        ("A short comment".to_string(), "A short comment".to_string()),
        (
            "First\r\n\r\nSecond\rThird\n".to_string(),
            "First    Second Third ".to_string(),
        ),
        ("a".repeat(200), "a".repeat(200)),
        ("a".repeat(201), "a".repeat(200)),
        (format!("{}extra", "界🌍".repeat(100)), "界🌍".repeat(100)),
    ] {
        let event = snapshot.feedback_event(
            "bug",
            Some(&reason),
            /*tags*/ None,
            /*session_source*/ None,
        );
        assert_eq!(
            event,
            FeedbackEvent {
                event_id: event.event_id,
                level: FeedbackLevel::Error,
                message: Some(title.clone()),
                exception: vec![FeedbackException {
                    ty: title.clone(),
                    value: Some(reason),
                }],
                tags: BTreeMap::from([
                    ("thread_id".to_string(), snapshot.thread_id.clone()),
                    ("classification".to_string(), "bug".to_string()),
                    (
                        "cli_version".to_string(),
                        hanzo::version::get().to_string()
                    ),
                    ("reason".to_string(), preview),
                ]),
                ..Default::default()
            }
        );
    }
}

#[tokio::test]
async fn feedback_upload_sends_safe_reason_tag_and_full_comment() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/event"))
        .and(header("Content-Type", "application/json"))
        .respond_with(ResponseTemplate::new(StatusCode::OK))
        .expect(/*r*/ 1)
        .mount(&server)
        .await;

    let snapshot = CodexFeedback::new().snapshot(/*session_id*/ None);
    let reason = format!("first\r\nsecond\n{}tail", "界🌍".repeat(120));
    let tags = BTreeMap::from([("reason".to_string(), "caller override\n".to_string())]);
    snapshot
        .upload_feedback_to_hanzo(
            FeedbackUploadOptions {
                classification: "bug",
                reason: Some(&reason),
                tags: Some(&tags),
                include_logs: false,
                extra_attachments: Vec::new(),
                extra_attachment_paths: &[],
                session_source: None,
                logs_override: None,
            },
            &HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            &format!("{}/v1/event", server.uri()),
            hanzo::FEEDBACK_KEY,
            Instant::now() + UPLOAD_TIMEOUT,
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let event: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(
        event["properties"]["reason"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        200
    );
    let title = format!("[Bug]: Hanzo Dev session {}", snapshot.thread_id);
    let expected_tags = serde_json::to_value(BTreeMap::from([
        ("thread_id".to_string(), snapshot.thread_id),
        ("classification".to_string(), "bug".to_string()),
        (
            "cli_version".to_string(),
            hanzo::version::get().to_string(),
        ),
        (
            "reason".to_string(),
            format!("first  second {}", "界🌍".repeat(93)),
        ),
    ]))
    .unwrap();
    let expected_error = serde_json::json!({
        "type": title,
        "message": reason,
        "handled": true,
    });
    assert_eq!(
        (&event["properties"], &event["error"]),
        (&expected_tags, &expected_error)
    );
}

#[tokio::test]
async fn feedback_upload_to_hanzo_sends_error_event_and_log_attachments() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/event"))
        .and(header("Authorization", "Bearer pk-test-dev-key"))
        .and(header("Content-Type", "application/json"))
        .respond_with(
            ResponseTemplate::new(StatusCode::OK)
                .set_body_string("{\"accepted\":1,\"dropped\":0}"),
        )
        .expect(/*r*/ 3)
        .mount(&server)
        .await;

    let snapshot = CodexFeedback::new()
        .snapshot(/*session_id*/ None)
        .with_feedback_diagnostics(crate::FeedbackDiagnostics::default());
    let tags = BTreeMap::from([("account_id".to_string(), "user-42".to_string())]);
    snapshot
        .upload_feedback_to_hanzo(
            FeedbackUploadOptions {
                classification: "bad_result",
                reason: Some("missed repo status constraints"),
                tags: Some(&tags),
                include_logs: true,
                extra_attachments: vec![crate::FeedbackAttachment {
                    filename: crate::DOCTOR_REPORT_ATTACHMENT_FILENAME.to_string(),
                    content_type: Some("application/json".to_string()),
                    buffer: b"{\"overallStatus\":\"ok\"}".to_vec(),
                }],
                extra_attachment_paths: &[],
                session_source: None,
                logs_override: Some(b"ring buffer log line\n".to_vec()),
            },
            &HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
            &format!("{}/v1/event", server.uri()),
            "pk-test-dev-key",
            Instant::now() + UPLOAD_TIMEOUT,
        )
        .await
        .unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 3);

    let core: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let event_id = core["messageId"].as_str().unwrap();
    assert_eq!(core["type"], "error");
    assert_eq!(core["event"], "feedback");
    assert_eq!(core["product"], "dev");
    assert_eq!(core["distinctId"], "user-42");
    assert_eq!(core["sessionId"], snapshot.thread_id);
    assert_eq!(core["traceId"], event_id);
    assert_eq!(core["level"], "error");
    assert_eq!(
        core["error"]["type"],
        format!("[Bad result]: Hanzo Dev session {}", snapshot.thread_id)
    );
    assert_eq!(core["error"]["message"], "missed repo status constraints");
    assert_eq!(core["error"]["handled"], true);
    assert_eq!(core["properties"]["classification"], "bad_result");
    assert_eq!(core["properties"]["thread_id"], snapshot.thread_id);

    let log_event: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
    assert_eq!(log_event["type"], "log");
    assert_eq!(log_event["event"], "feedback.attachment");
    assert_eq!(log_event["kind"], "codex-logs.log");
    assert_eq!(log_event["sessionId"], snapshot.thread_id);
    assert_eq!(log_event["traceId"], event_id);
    assert_eq!(log_event["log"]["body"], "ring buffer log line\n");

    let doctor_event: serde_json::Value = serde_json::from_slice(&requests[2].body).unwrap();
    assert_eq!(doctor_event["type"], "log");
    assert_eq!(
        doctor_event["kind"],
        crate::DOCTOR_REPORT_ATTACHMENT_FILENAME
    );
    assert_eq!(doctor_event["log"]["body"], "{\"overallStatus\":\"ok\"}");
}
