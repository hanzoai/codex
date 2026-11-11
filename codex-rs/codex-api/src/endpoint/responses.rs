use crate::auth::SharedAuthProvider;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::Compression;
use crate::requests::headers::build_session_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use crate::sse::build_chat_completions_request;
use crate::sse::spawn_chat_completions_stream;
use crate::sse::spawn_response_stream;
use crate::telemetry::SseTelemetry;
use codex_client::EncodedJsonBody;
use codex_client::HttpTransport;
use codex_client::RequestCompression;
use codex_client::RequestTelemetry;
use codex_client::TransportError;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use http::HeaderValue;
use http::Method;
use http::StatusCode;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;
use tracing::instrument;

pub const HANZO_WIRE_API_HEADER: &str = "x-hanzo-wire-api";

static CHAT_COMPLETIONS_FALLBACK_BASES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn endpoint_prefers_chat_completions(base_url: &str, headers: &HeaderMap) -> bool {
    if headers
        .get(HANZO_WIRE_API_HEADER)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.eq_ignore_ascii_case("chat"))
    {
        return true;
    }
    let normalized = base_url.trim_end_matches('/');
    CHAT_COMPLETIONS_FALLBACK_BASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .contains(normalized)
}

fn mark_chat_completions_fallback(base_url: &str) {
    let normalized = base_url.trim_end_matches('/').to_string();
    CHAT_COMPLETIONS_FALLBACK_BASES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(normalized);
}

fn should_fallback_to_chat_completions(err: &ApiError) -> bool {
    match err {
        ApiError::Transport(TransportError::Http { status, body, .. }) => {
            if !matches!(
                *status,
                StatusCode::NOT_FOUND | StatusCode::METHOD_NOT_ALLOWED | StatusCode::NOT_IMPLEMENTED
            ) {
                return false;
            }
            let body_str = body.as_deref().unwrap_or_default();
            !body_str.contains("model_not_found")
        }
        _ => false,
    }
}

pub struct ResponsesClient<T: HttpTransport> {
    session: EndpointSession<T>,
    sse_telemetry: Option<Arc<dyn SseTelemetry>>,
}

#[derive(Default)]
pub struct ResponsesOptions {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub session_source: Option<SessionSource>,
    pub extra_headers: HeaderMap,
    pub compression: Compression,
    pub turn_state: Option<Arc<OnceLock<String>>>,
}

impl<T: HttpTransport> ResponsesClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
            sse_telemetry: None,
        }
    }

    pub fn with_telemetry(
        self,
        request: Option<Arc<dyn RequestTelemetry>>,
        sse: Option<Arc<dyn SseTelemetry>>,
    ) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
            sse_telemetry: sse,
        }
    }

    #[instrument(
        name = "responses.stream_request",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "/responses"
        )
    )]
    pub async fn stream_request(
        &self,
        request: ResponsesApiRequest,
        options: ResponsesOptions,
    ) -> Result<ResponseStream, ApiError> {
        let ResponsesOptions {
            session_id,
            thread_id,
            session_source,
            extra_headers,
            compression,
            turn_state,
        } = options;
        let mut headers = extra_headers;
        if let Some(ref thread_id) = thread_id {
            insert_header(&mut headers, "x-client-request-id", thread_id);
        }
        headers.extend(build_session_headers(session_id, thread_id));
        if let Some(subagent) = subagent_header(&session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        let base_url = &self.session.provider().base_url;
        let force_chat = endpoint_prefers_chat_completions(base_url, &headers)
            || endpoint_prefers_chat_completions(base_url, &self.session.provider().headers);
        headers.remove(HANZO_WIRE_API_HEADER);

        if force_chat {
            return self
                .stream_chat_completions(&request, headers, compression, turn_state)
                .await;
        }

        let body = EncodedJsonBody::encode(&request)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;

        match self
            .stream_encoded(body, headers.clone(), compression, turn_state.clone())
            .await
        {
            Ok(stream) => Ok(stream),
            Err(err) if should_fallback_to_chat_completions(&err) => {
                mark_chat_completions_fallback(&self.session.provider().base_url);
                self.stream_chat_completions(&request, headers, compression, turn_state)
                    .await
            }
            Err(err) => Err(err),
        }
    }

    async fn stream_chat_completions(
        &self,
        request: &ResponsesApiRequest,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let (chat_json, custom_tool_names) = build_chat_completions_request(request);
        let body = EncodedJsonBody::encode(&chat_json).map_err(|e| {
            ApiError::Stream(format!("failed to encode chat completions request: {e}"))
        })?;
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "/chat/completions",
                extra_headers,
                Some(body),
                |req| {
                    req.headers.remove(HANZO_WIRE_API_HEADER);
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_chat_completions_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
            custom_tool_names,
        ))
    }

    #[instrument(
        name = "responses.stream",
        level = "info",
        skip_all,
        fields(
            transport = "responses_http",
            http.method = "POST",
            api.path = "/responses",
            turn.has_state = turn_state.is_some()
        )
    )]
    pub async fn stream(
        &self,
        body: Value,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let body = EncodedJsonBody::encode(&body)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;
        self.stream_encoded(body, extra_headers, compression, turn_state)
            .await
    }

    async fn stream_encoded(
        &self,
        body: EncodedJsonBody,
        extra_headers: HeaderMap,
        compression: Compression,
        turn_state: Option<Arc<OnceLock<String>>>,
    ) -> Result<ResponseStream, ApiError> {
        let request_compression = match compression {
            Compression::None => RequestCompression::None,
            Compression::Zstd => RequestCompression::Zstd,
        };

        let stream_response = self
            .session
            .stream_encoded_json_with(
                Method::POST,
                "/responses",
                extra_headers,
                Some(body),
                |req| {
                    req.headers.insert(
                        http::header::ACCEPT,
                        HeaderValue::from_static("text/event-stream"),
                    );
                    req.compression = request_compression;
                },
            )
            .await?;

        Ok(spawn_response_stream(
            stream_response,
            self.session.provider().stream_idle_timeout,
            self.sse_telemetry.clone(),
            turn_state,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::NoopAuthProvider;
    use crate::common::ResponseEvent;
    use crate::provider::RetryConfig;
    use bytes::Bytes;
    use codex_client::Request;
    use codex_client::Response;
    use codex_client::StreamResponse;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ResponseItem;
    use futures::StreamExt;
    use std::time::Duration;

    struct FallbackMockTransport;

    impl HttpTransport for FallbackMockTransport {
        async fn execute(&self, _req: Request) -> Result<Response, TransportError> {
            Err(TransportError::Build("unexpected unary call".to_string()))
        }

        async fn stream(&self, req: Request) -> Result<StreamResponse, TransportError> {
            if req.url.ends_with("/responses") {
                return Err(TransportError::Http {
                    status: StatusCode::NOT_FOUND,
                    url: Some(req.url),
                    headers: None,
                    body: Some("404 page not found".to_string()),
                    retry_after: None,
                });
            }
            assert!(
                req.url.ends_with("/chat/completions"),
                "expected fallback to /chat/completions, got {}",
                req.url
            );
            let sse_payload = concat!(
                "data: {\"id\":\"chatcmpl-test\",\"model\":\"qwen-local\",\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"think...\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"CHAT_SSE_\"},\"finish_reason\":null}]}\n\n",
                "data: {\"id\":\"chatcmpl-test\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"OK\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":10,\"completion_tokens\":4,\"total_tokens\":14}}\n\n",
                "data: [DONE]\n\n"
            );
            let stream = futures::stream::iter(vec![Ok(Bytes::from_static(sse_payload.as_bytes()))]);
            Ok(StreamResponse {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                bytes: Box::pin(stream),
            })
        }
    }

    #[tokio::test]
    async fn falls_back_from_responses_404_to_chat_completions_sse() {
        let provider = Provider {
            name: "local-llama".to_string(),
            base_url: "http://127.0.0.1:19991/v1".to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: false,
                retry_transport: false,
            },
            stream_idle_timeout: Duration::from_secs(5),
        };
        let client = ResponsesClient::new(
            FallbackMockTransport,
            provider,
            Arc::new(NoopAuthProvider),
        );
        let request = ResponsesApiRequest {
            model: "qwen-local".to_string(),
            stream: true,
            service_tier: None,
            input: vec![ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "ping".to_string(),
                }],
                status: None,
                encrypted_content: None,
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            }],
            tools: None,
            tool_choice: "auto".to_string(),
            parallel_tool_calls: true,
            reasoning: None,
            store: false,
            stream_options: None,
            include: Vec::new(),
            prompt_cache_key: None,
            prompt_cache_options: None,
            text: None,
            client_metadata: None,
            access_programs: None,
        };
        let mut stream = client
            .stream_request(request, ResponsesOptions::default())
            .await
            .expect("chat completions fallback stream");
        let mut text = String::new();
        let mut completed = false;
        while let Some(evt) = stream.next().await {
            match evt.expect("sse event") {
                ResponseEvent::OutputTextDelta(delta) => text.push_str(&delta),
                ResponseEvent::Completed { token_usage, .. } => {
                    completed = true;
                    assert_eq!(token_usage.expect("usage").total_tokens, 14);
                }
                _ => {}
            }
        }
        assert_eq!(text, "CHAT_SSE_OK");
        assert!(completed);
    }
}
