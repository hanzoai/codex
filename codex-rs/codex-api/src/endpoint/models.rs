use crate::auth::SharedAuthProvider;
use crate::endpoint::session::EndpointSession;
use crate::error::ApiError;
use crate::provider::Provider;
use bytes::Bytes;
use codex_client::HttpTransport;
use codex_client::RequestTelemetry;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelsResponse;
use http::HeaderMap;
use http::Method;
use http::header::ETAG;
use std::sync::Arc;
use url::Url;

pub struct ModelsClient<T: HttpTransport> {
    session: EndpointSession<T>,
}

impl<T: HttpTransport> ModelsClient<T> {
    pub fn new(transport: T, provider: Provider, auth: SharedAuthProvider) -> Self {
        Self {
            session: EndpointSession::new(transport, provider, auth),
        }
    }

    pub fn with_telemetry(self, request: Option<Arc<dyn RequestTelemetry>>) -> Self {
        Self {
            session: self.session.with_request_telemetry(request),
        }
    }

    fn path() -> &'static str {
        "models"
    }

    fn append_client_version_query(req: &mut codex_client::Request, client_version: &str) {
        let separator = if req.url.contains('?') { '&' } else { '?' };
        req.url = format!("{}{}client_version={client_version}", req.url, separator);
    }

    pub fn request_url(provider: &Provider, client_version: &str) -> String {
        let mut request = provider.build_request(Method::GET, Self::path());
        Self::append_client_version_query(&mut request, client_version);
        request.url
    }

    /// Builds a full catalog URL, preserving provider routing parameters and client version.
    pub fn catalog_request_url(
        provider: &Provider,
        catalog_url: &str,
        client_version: &str,
    ) -> Result<String, ApiError> {
        let invalid = |message: &str| ApiError::InvalidRequest {
            message: message.to_string(),
        };
        let mut url = Url::parse(catalog_url)
            .map_err(|_| invalid("model_catalog_url must be an absolute URL"))?;
        {
            let mut query = url.query_pairs_mut();
            if let Some(params) = &provider.query_params {
                query.extend_pairs(params);
            }
            query.append_pair("client_version", client_version);
        }
        Ok(url.into())
    }

    /// Fetches and decodes a catalog, optionally bounding response bytes before decoding.
    pub async fn list_models(
        &self,
        request_url: String,
        extra_headers: HeaderMap,
        response_body_limit_bytes: Option<usize>,
    ) -> Result<(Vec<ModelInfo>, Option<String>), ApiError> {
        let is_hanzo = request_url
            .split('?')
            .next()
            .and_then(|url| url.strip_suffix("/models"))
            .is_some_and(|base| hanzo::is_hanzo(Some(base)));
        let (body, header_etag) = self
            .list_models_raw(request_url, extra_headers, response_body_limit_bytes)
            .await?;
        let models = match serde_json::from_slice::<ModelsResponse>(&body) {
            Ok(ModelsResponse { models }) if !models.is_empty() => models,
            Ok(ModelsResponse { models }) => {
                decode_dynamic_models_response(&body, is_hanzo).unwrap_or(models)
            }
            Err(primary_err) => decode_dynamic_models_response(&body, is_hanzo).ok_or_else(|| {
                ApiError::Stream(format!(
                    "failed to decode models response: {:?} at line {} column {} (body: {} bytes)",
                    primary_err.classify(),
                    primary_err.line(),
                    primary_err.column(),
                    body.len()
                ))
            })?,
        };

        Ok((models, header_etag))
    }

    /// Fetches a catalog using the provider's auth and retry policy without decoding it.
    ///
    /// Callers accepting provider-controlled catalogs must validate the native response
    /// without including raw values in diagnostics.
    pub async fn list_models_raw(
        &self,
        request_url: String,
        extra_headers: HeaderMap,
        response_body_limit_bytes: Option<usize>,
    ) -> Result<(Bytes, Option<String>), ApiError> {
        let resp = self
            .session
            .execute_with(
                Method::GET,
                Self::path(),
                extra_headers,
                /*body*/ None,
                move |req| {
                    req.url.clone_from(&request_url);
                    req.response_body_limit_bytes = response_body_limit_bytes;
                },
            )
            .await?;

        let header_etag = resp
            .headers
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);

        Ok((resp.body, header_etag))
    }
}

fn decode_dynamic_models_response(body: &[u8], is_hanzo: bool) -> Option<Vec<ModelInfo>> {
    use codex_protocol::config_types::ReasoningSummary;
    use codex_protocol::openai_models::ConfigShellToolType;
    use codex_protocol::openai_models::ModelMessages;
    use codex_protocol::openai_models::ModelVisibility;
    use codex_protocol::openai_models::ReasoningEffortPreset;
    use codex_protocol::openai_models::TruncationPolicyConfig;
    use codex_protocol::openai_models::WebSearchToolType;
    use codex_protocol::openai_models::default_input_modalities;
    use serde_json::Value;

    const DYNAMIC_BASE_INSTRUCTIONS: &str = include_str!("../../../models-manager/prompt.md");

    let root: Value = serde_json::from_slice(body).ok()?;
    let entries = if let Some(arr) = root.get("data").and_then(Value::as_array) {
        arr
    } else if let Some(arr) = root.get("models").and_then(Value::as_array) {
        arr
    } else if let Some(arr) = root.as_array() {
        arr
    } else {
        return None;
    };

    let efforts: Vec<ReasoningEffortPreset> = hanzo::effort::EFFORTS
        .iter()
        .filter_map(|(name, description)| {
            Some(ReasoningEffortPreset {
                effort: name.parse().ok()?,
                description: (*description).to_string(),
            })
        })
        .collect();
    let default_effort = hanzo::effort::DEFAULT.parse().ok();

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for (idx, item) in entries.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(raw_id) = obj
            .get("id")
            .or_else(|| obj.get("slug"))
            .or_else(|| obj.get("name"))
            .or_else(|| obj.get("model"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        let slug = raw_id.trim();
        if slug.is_empty()
            || (is_hanzo && !hanzo::catalog::is_chat_model(slug))
            || !seen.insert(slug.to_string())
        {
            continue;
        }
        let known = hanzo::catalog::find(slug);
        let display_name = obj
            .get("display_name")
            .or_else(|| obj.get("name"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| known.map_or_else(|| slug.to_string(), |m| m.name.to_string()));
        let description = obj
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .or_else(|| known.map(|m| m.name.to_string()));
        let window = obj
            .get("context_window")
            .or_else(|| obj.get("context_length"))
            .or_else(|| obj.get("max_model_len"))
            .and_then(Value::as_i64)
            .filter(|&w| w > 0)
            .unwrap_or_else(|| known.map_or(272_000, |m| m.window));
        let (visibility, priority) = if is_hanzo && hanzo::catalog::is_first_party(slug) {
            let listed = known.is_none_or(|m| m.listed);
            (
                if listed {
                    ModelVisibility::List
                } else {
                    ModelVisibility::Hide
                },
                hanzo::catalog::picker_priority(slug),
            )
        } else if is_hanzo {
            (ModelVisibility::List, 200 + idx as i32)
        } else {
            (ModelVisibility::List, 100 + idx as i32)
        };

        out.push(ModelInfo {
            guardian: None,
            slug: slug.to_string(),
            display_name,
            description,
            default_reasoning_level: default_effort.clone(),
            supported_reasoning_levels: efforts.clone(),
            shell_type: ConfigShellToolType::UnifiedExec,
            visibility,
            supported_in_api: true,
            priority,
            additional_speed_tiers: Vec::new(),
            service_tiers: Vec::new(),
            default_service_tier: None,
            available_access_programs: None,
            availability_nux: None,
            upgrade: None,
            model_messages: Some(ModelMessages {
                instructions_template: Some(DYNAMIC_BASE_INSTRUCTIONS.to_string()),
                ..Default::default()
            }),
            include_skills_usage_instructions: true,
            include_plugin_usage_instructions: false,
            include_apps_usage_instructions: false,
            supports_reasoning_summary_parameter: true,
            default_reasoning_summary: ReasoningSummary::Auto,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            web_search_tool_type: WebSearchToolType::Text,
            web_search_provider: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_image_detail_original: false,
            context_window: Some(window),
            max_context_window: Some(window),
            auto_compact_token_limit: None,
            comp_hash: None,
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
            input_modalities: match known {
                Some(m) if !m.vision => vec![codex_protocol::openai_models::InputModality::Text],
                _ => default_input_modalities(),
            },
            supports_search_tool: false,
            supports_experimental_context: false,
            use_responses_lite: false,
            supports_reasoning_effort_updates: false,
            node_repl_auto_review_required: false,
            node_repl_disabled: false,
            auto_review_model_override: None,
            model_specialty: None,
            tool_mode: None,
            multi_agent_version: None,
            multi_agent_reasoning_effort: None,
        });
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthError;
    use crate::auth::AuthProvider;
    use crate::auth::AuthProviderFuture;
    use crate::provider::RetryConfig;
    use codex_client::Request;
    use codex_client::Response;
    use codex_client::StreamResponse;
    use codex_client::TransportError;
    use http::HeaderMap;
    use http::StatusCode;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::sync::Arc;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Clone)]
    struct CapturingTransport {
        last_request: Arc<Mutex<Option<Request>>>,
        body: Arc<ModelsResponse>,
        etag: Option<String>,
    }

    impl Default for CapturingTransport {
        fn default() -> Self {
            Self {
                last_request: Arc::new(Mutex::new(None)),
                body: Arc::new(ModelsResponse { models: Vec::new() }),
                etag: None,
            }
        }
    }

    impl HttpTransport for CapturingTransport {
        async fn execute(&self, req: Request) -> Result<Response, TransportError> {
            *self.last_request.lock().unwrap() = Some(req);
            let body = serde_json::to_vec(&*self.body).unwrap();
            let mut headers = HeaderMap::new();
            if let Some(etag) = &self.etag {
                headers.insert(ETAG, etag.parse().unwrap());
            }
            Ok(Response {
                status: StatusCode::OK,
                headers,
                body: body.into(),
            })
        }

        async fn stream(&self, _req: Request) -> Result<StreamResponse, TransportError> {
            Err(TransportError::Build("stream should not run".to_string()))
        }
    }

    #[derive(Clone, Default)]
    struct DummyAuth;

    impl AuthProvider for DummyAuth {
        fn add_auth_headers(&self, _headers: &mut HeaderMap) -> Result<(), crate::AuthError> {
            Ok(())
        }
    }

    #[derive(Default)]
    struct RetryAuth {
        request_limits: Mutex<Vec<Option<usize>>>,
    }

    impl AuthProvider for RetryAuth {
        fn add_auth_headers(&self, headers: &mut HeaderMap) -> Result<(), AuthError> {
            headers.insert(http::header::AUTHORIZATION, "Bearer test".parse().unwrap());
            Ok(())
        }

        fn apply_auth(&self, mut request: Request) -> AuthProviderFuture<'_> {
            Box::pin(async move {
                let mut limits = self.request_limits.lock().unwrap();
                limits.push(request.response_body_limit_bytes);
                if limits.len() == 1 {
                    return Err(AuthError::Transient("retry authentication".to_string()));
                }
                self.add_auth_headers(&mut request.headers)?;
                Ok(request)
            })
        }
    }

    fn provider(base_url: &str) -> Provider {
        Provider {
            name: "test".to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(1),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn catalog_request_url_preserves_routing_and_encodes_queries() {
        let mut provider = provider("https://gateway.example/v1");
        provider.query_params = Some(std::collections::HashMap::from([(
            "api-version".to_string(),
            "2026 09".to_string(),
        )]));
        let url = ModelsClient::<CapturingTransport>::catalog_request_url(
            &provider,
            "https://catalog.example/codex/models?deployment=one",
            "1.2.3",
        )
        .unwrap();
        assert_eq!(
            url,
            "https://catalog.example/codex/models?deployment=one&api-version=2026+09&client_version=1.2.3"
        );
    }

    #[test]
    fn catalog_request_url_rejects_relative_urls() {
        let provider = provider("https://gateway.example/v1");
        for catalog_url in ["/codex/models", "codex/models"] {
            let error = ModelsClient::<CapturingTransport>::catalog_request_url(
                &provider,
                catalog_url,
                "1.2.3",
            )
            .unwrap_err();
            assert!(matches!(error, ApiError::InvalidRequest { .. }));
        }
    }

    #[tokio::test]
    async fn response_body_limit_survives_auth_retry_without_limiting_other_models_requests() {
        let transport = CapturingTransport::default();
        let auth = Arc::new(RetryAuth::default());
        let mut provider = provider("https://example.com/api/codex");
        provider.retry.max_attempts = 2;
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.99.0");
        let client = ModelsClient::new(transport.clone(), provider, auth.clone());

        client
            .list_models(
                request_url.clone(),
                HeaderMap::new(),
                /*response_body_limit_bytes*/ Some(64),
            )
            .await
            .expect("limited request should succeed after auth retry");
        {
            let request = transport.last_request.lock().unwrap();
            let request = request.as_ref().unwrap();
            assert_eq!(request.response_body_limit_bytes, Some(64));
            assert_eq!(request.url, request_url);
            assert_eq!(request.headers[http::header::AUTHORIZATION], "Bearer test");
        }

        let (models, _) = client
            .list_models(
                request_url,
                HeaderMap::new(),
                /*response_body_limit_bytes*/ None,
            )
            .await
            .expect("ordinary request on the same client should remain unbounded");
        assert!(models.is_empty());
        assert_eq!(
            transport
                .last_request
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .response_body_limit_bytes,
            None
        );
        assert_eq!(
            *auth.request_limits.lock().unwrap(),
            vec![Some(64), Some(64), None]
        );
    }

    #[tokio::test]
    async fn appends_client_version_query() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.99.0");
        let client = ModelsClient::new(transport.clone(), provider, Arc::new(DummyAuth));

        let (models, _) = client
            .list_models(
                request_url,
                HeaderMap::new(),
                /*response_body_limit_bytes*/ None,
            )
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);

        let url = transport
            .last_request
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .url
            .clone();
        assert_eq!(
            url,
            "https://example.com/api/codex/models?client_version=0.99.0"
        );
    }

    #[tokio::test]
    async fn parses_models_response() {
        let response = ModelsResponse {
            models: vec![
                serde_json::from_value(json!({
                    "slug": "gpt-test",
                    "display_name": "gpt-test",
                    "description": "desc",
                    "default_reasoning_level": "medium",
                    "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}, {"effort": "high", "description": "high"}],
                    "shell_type": "shell_command",
                    "visibility": "list",
                    "minimal_client_version": [0, 99, 0],
                    "supported_in_api": true,
                    "priority": 1,
                    "upgrade": null,
                    "support_verbosity": false,
                    "default_verbosity": null,
                    "apply_patch_tool_type": null,
                    "truncation_policy": {"mode": "bytes", "limit": 10_000},
                    "supports_image_detail_original": false,
                    "context_window": 272_000,
                    "experimental_supported_tools": [],
                }))
                .unwrap(),
            ],
        };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: None,
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.99.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let (models, _) = client
            .list_models(
                request_url,
                HeaderMap::new(),
                /*response_body_limit_bytes*/ None,
            )
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 1);
        assert_eq!(models[0].slug, "gpt-test");
        assert_eq!(models[0].supported_in_api, true);
        assert_eq!(models[0].priority, 1);
    }

    #[tokio::test]
    async fn list_models_includes_etag() {
        let response = ModelsResponse { models: Vec::new() };

        let transport = CapturingTransport {
            last_request: Arc::new(Mutex::new(None)),
            body: Arc::new(response),
            etag: Some("\"abc\"".to_string()),
        };

        let provider = provider("https://example.com/api/codex");
        let request_url = ModelsClient::<CapturingTransport>::request_url(&provider, "0.1.0");
        let client = ModelsClient::new(transport, provider, Arc::new(DummyAuth));

        let (models, etag) = client
            .list_models(
                request_url,
                HeaderMap::new(),
                /*response_body_limit_bytes*/ None,
            )
            .await
            .expect("request should succeed");

        assert_eq!(models.len(), 0);
        assert_eq!(etag, Some("\"abc\"".to_string()));
    }

    #[test]
    fn decodes_openai_and_ollama_dynamic_models() {
        let openai_body = br#"{"object":"list","data":[{"id":"moonshotai/kimi-k2.5","context_window":262144},{"id":"qwen/qwen3-coder","max_model_len":131072}]}"#;
        let models =
            decode_dynamic_models_response(openai_body, /*is_hanzo*/ false).expect("openai models");
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].slug, "moonshotai/kimi-k2.5");
        assert_eq!(models[0].context_window, Some(262_144));
        assert_eq!(models[0].visibility, codex_protocol::openai_models::ModelVisibility::List);
        assert!(models[0].include_skills_usage_instructions);
        assert!(!models[0].supported_reasoning_levels.is_empty());
        assert_eq!(models[1].slug, "qwen/qwen3-coder");
        assert_eq!(models[1].context_window, Some(131_072));

        let ollama_body = br#"{"models":[{"name":"deepseek-r1:32b"},{"name":"qwen3:8b"}]}"#;
        let ollama =
            decode_dynamic_models_response(ollama_body, /*is_hanzo*/ false).expect("ollama models");
        assert_eq!(ollama.len(), 2);
        assert_eq!(ollama[0].slug, "deepseek-r1:32b");
        assert_eq!(ollama[1].slug, "qwen3:8b");

        let hanzo_body = br#"{"object":"list","data":[{"id":"aion-labs/aion-1.0"},{"id":"zen-embedding"},{"id":"hanzo/enso"},{"id":"zen6-coder"},{"id":"enso"}]}"#;
        let hanzo_models =
            decode_dynamic_models_response(hanzo_body, /*is_hanzo*/ true).expect("hanzo models");
        let listed_slugs: Vec<&str> = hanzo_models
            .iter()
            .filter(|m| m.visibility == codex_protocol::openai_models::ModelVisibility::List)
            .map(|m| m.slug.as_str())
            .collect();
        assert_eq!(listed_slugs, vec!["aion-labs/aion-1.0", "zen6-coder", "enso"]);
        let alias = hanzo_models.iter().find(|m| m.slug == "hanzo/enso").unwrap();
        assert_eq!(alias.visibility, codex_protocol::openai_models::ModelVisibility::Hide);
        let enso = hanzo_models.iter().find(|m| m.slug == "enso").unwrap();
        let zen6 = hanzo_models.iter().find(|m| m.slug == "zen6-coder").unwrap();
        let third_party = hanzo_models
            .iter()
            .find(|m| m.slug == "aion-labs/aion-1.0")
            .unwrap();
        assert!(enso.priority < zen6.priority);
        assert!(zen6.priority < third_party.priority);

        // Verify disk cache (`models_cache.json`) roundtrip deserialization succeeds.
        let cached_bytes = serde_json::to_vec(&ModelsResponse { models }).expect("serialize cache");
        let reloaded: ModelsResponse =
            serde_json::from_slice(&cached_bytes).expect("deserialize cache");
        assert_eq!(reloaded.models.len(), 2);
    }
}
