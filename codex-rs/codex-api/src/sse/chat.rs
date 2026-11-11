//! Native OpenAI Chat Completions SSE (`POST /v1/chat/completions` `text/event-stream`)
//! adapter into Codex `ResponseEvent`s.
//!
//! Enables zero-friction streaming against `llama.cpp` (`llama-server`), `Halogen`,
//! `OpenRouter`, `NVIDIA NIM`, `vLLM`, `Zen Engine`, `ZAP` daemons, and any
//! OpenAI-compatible `/v1/chat/completions` SSE endpoint.

use crate::common::ResponseEvent;
use crate::common::ResponseStream;
use crate::common::ResponsesApiRequest;
use crate::error::ApiError;
use crate::rate_limits::parse_all_rate_limits;
use crate::telemetry::SseTelemetry;
use codex_client::ByteStream;
use codex_client::StreamResponse;
use codex_protocol::ResponseItemId;
use codex_protocol::models::AgentMessageInputContent;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ImageReference;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::TokenUsage;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio::time::timeout;

const OPENAI_MODEL_HEADER: &str = "openai-model";
const REQUEST_ID_HEADER: &str = "x-request-id";
const X_CODEX_TURN_STATE_HEADER: &str = "x-codex-turn-state";

/// Converts a [`ResponsesApiRequest`] into an OpenAI `/v1/chat/completions` JSON body,
/// returning `(body_json, custom_tool_names)` so custom tools round-trip cleanly.
pub(crate) fn build_chat_completions_request(
    req: &ResponsesApiRequest,
) -> (Value, HashSet<String>) {
    let mut messages: Vec<Value> = Vec::new();
    for item in &req.input {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let mapped_role = match role.as_str() {
                    "developer" => "system",
                    other => other,
                };
                let chat_content = convert_message_content(content);
                messages.push(json!({
                    "role": mapped_role,
                    "content": chat_content,
                }));
            }
            ResponseItem::AgentMessage {
                author, content, ..
            } => {
                let text = content
                    .iter()
                    .filter_map(|part| match part {
                        AgentMessageInputContent::InputText { text } => Some(text.as_str()),
                        AgentMessageInputContent::EncryptedContent { .. } => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                if !text.is_empty() {
                    messages.push(json!({
                        "role": "user",
                        "content": format!("[{author}] {text}"),
                    }));
                }
            }
            ResponseItem::FunctionCall {
                name,
                arguments,
                call_id,
                ..
            } => {
                append_assistant_tool_call(&mut messages, call_id, name, arguments);
            }
            ResponseItem::CustomToolCall {
                name,
                input,
                call_id,
                ..
            } => {
                let arguments = json!({ "input": input }).to_string();
                append_assistant_tool_call(&mut messages, call_id, name, &arguments);
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id.clone().unwrap_or_default(),
                    "content": output.body.to_text().unwrap_or_default(),
                }));
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                messages.push(json!({
                    "role": "tool",
                    "tool_call_id": call_id,
                    "content": output.body.to_text().unwrap_or_default(),
                }));
            }
            _ => {}
        }
    }

    let mut custom_tool_names = HashSet::new();
    let mut chat_tools = Vec::new();
    if let Some(tools) = req.tools.as_ref()
        && let Ok(Value::Array(raw_tools)) = serde_json::from_str::<Value>(tools.as_raw_value().get())
    {
        for raw in raw_tools {
            let Some(obj) = raw.as_object() else {
                continue;
            };
            let tool_type = obj.get("type").and_then(Value::as_str).unwrap_or("function");
            match tool_type {
                "function" => {
                    if obj.contains_key("function") {
                        chat_tools.push(raw);
                    } else if let Some(name) = obj.get("name").and_then(Value::as_str) {
                        let mut fn_obj = Map::new();
                        fn_obj.insert("name".to_string(), Value::String(name.to_string()));
                        if let Some(desc) = obj.get("description") {
                            fn_obj.insert("description".to_string(), desc.clone());
                        }
                        if let Some(params) = obj.get("parameters") {
                            fn_obj.insert("parameters".to_string(), params.clone());
                        } else {
                            fn_obj.insert(
                                "parameters".to_string(),
                                json!({ "type": "object", "properties": {} }),
                            );
                        }
                        chat_tools.push(json!({
                            "type": "function",
                            "function": Value::Object(fn_obj),
                        }));
                    }
                }
                "custom" => {
                    if let Some(name) = obj.get("name").and_then(Value::as_str) {
                        custom_tool_names.insert(name.to_string());
                        let description = obj
                            .get("description")
                            .and_then(Value::as_str)
                            .unwrap_or("Custom freeform tool");
                        chat_tools.push(json!({
                            "type": "function",
                            "function": {
                                "name": name,
                                "description": description,
                                "parameters": {
                                    "type": "object",
                                    "properties": {
                                        "input": {
                                            "type": "string",
                                            "description": "Raw freeform input payload for this tool."
                                        }
                                    },
                                    "required": ["input"]
                                }
                            }
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    let mut body = Map::new();
    body.insert("model".to_string(), Value::String(req.model.clone()));
    body.insert("stream".to_string(), Value::Bool(true));
    body.insert(
        "stream_options".to_string(),
        json!({ "include_usage": true }),
    );
    body.insert("messages".to_string(), Value::Array(messages));
    if !chat_tools.is_empty() {
        body.insert("tools".to_string(), Value::Array(chat_tools));
        if !req.tool_choice.is_empty() {
            body.insert(
                "tool_choice".to_string(),
                Value::String(req.tool_choice.clone()),
            );
        }
    }
    if let Some(reasoning) = req.reasoning.as_ref()
        && let Some(effort) = reasoning.effort.as_ref()
    {
        let effort_str = match effort.as_str() {
            "none" | "minimal" | "low" => "low",
            "medium" => "medium",
            _ => "high",
        };
        if effort.as_str() != "none" {
            body.insert(
                "reasoning_effort".to_string(),
                Value::String(effort_str.to_string()),
            );
        }
    }
    if let Some(text) = req.text.as_ref()
        && let Some(format) = text.format.as_ref()
    {
        body.insert(
            "response_format".to_string(),
            json!({
                "type": "json_schema",
                "json_schema": {
                    "name": format.name,
                    "strict": format.strict,
                    "schema": format.schema,
                }
            }),
        );
    }

    (Value::Object(body), custom_tool_names)
}

fn convert_message_content(content: &[ContentItem]) -> Value {
    let has_non_text = content
        .iter()
        .any(|item| matches!(item, ContentItem::InputImage { .. }));
    if !has_non_text {
        let joined = content
            .iter()
            .filter_map(|item| match item {
                ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                    Some(text.as_str())
                }
                ContentItem::InputImage { .. } | ContentItem::InputAudio { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(joined);
    }

    let mut parts = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text, .. } => {
                parts.push(json!({ "type": "text", "text": text }));
            }
            ContentItem::InputImage { image, .. } => {
                if let ImageReference::Inline { image_url } = image {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": image_url }
                    }));
                }
            }
            ContentItem::InputAudio { .. } => {}
        }
    }
    Value::Array(parts)
}

fn append_assistant_tool_call(
    messages: &mut Vec<Value>,
    call_id: &str,
    name: &str,
    arguments: &str,
) {
    let tool_call = json!({
        "id": call_id,
        "type": "function",
        "function": {
            "name": name,
            "arguments": arguments,
        }
    });
    if let Some(last) = messages.last_mut()
        && last.get("role").and_then(Value::as_str) == Some("assistant")
        && let Some(obj) = last.as_object_mut()
    {
        let arr = obj
            .entry("tool_calls")
            .or_insert_with(|| Value::Array(Vec::new()));
        if let Some(list) = arr.as_array_mut() {
            list.push(tool_call);
            return;
        }
    }
    messages.push(json!({
        "role": "assistant",
        "content": Value::Null,
        "tool_calls": [tool_call],
    }));
}

#[derive(Default)]
struct ToolCallAccum {
    id: String,
    name: String,
    arguments: String,
}

pub(crate) fn spawn_chat_completions_stream(
    stream_response: StreamResponse,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    turn_state: Option<Arc<OnceLock<String>>>,
    custom_tool_names: HashSet<String>,
) -> ResponseStream {
    let rate_limit_snapshots = parse_all_rate_limits(&stream_response.headers);
    let server_model = stream_response
        .headers
        .get(OPENAI_MODEL_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string);
    let upstream_request_id = stream_response
        .headers
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    if let Some(turn_state) = turn_state.as_ref()
        && let Some(header_value) = stream_response
            .headers
            .get(X_CODEX_TURN_STATE_HEADER)
            .and_then(|v| v.to_str().ok())
    {
        let _ = turn_state.set(header_value.to_string());
    }

    let (tx_event, rx_event) = mpsc::channel::<Result<ResponseEvent, ApiError>>(1600);
    tokio::spawn(async move {
        if let Some(model) = server_model {
            let _ = tx_event.send(Ok(ResponseEvent::ServerModel(model))).await;
        }
        for snapshot in rate_limit_snapshots {
            let _ = tx_event.send(Ok(ResponseEvent::RateLimits(snapshot))).await;
        }
        process_chat_sse(
            stream_response.bytes,
            tx_event,
            idle_timeout,
            telemetry,
            custom_tool_names,
        )
        .await;
    });

    ResponseStream {
        routing_affinity: None,
        rx_event,
        upstream_request_id,
        interrupt: None,
    }
}

async fn process_chat_sse(
    stream: ByteStream,
    tx_event: mpsc::Sender<Result<ResponseEvent, ApiError>>,
    idle_timeout: Duration,
    telemetry: Option<Arc<dyn SseTelemetry>>,
    custom_tool_names: HashSet<String>,
) {
    let mut stream = stream.eventsource();
    let mut created_sent = false;
    let mut response_id = format!("chatcmpl_{}", uuid::Uuid::new_v4().simple());
    let mut assistant_text = String::new();
    let mut message_item_added = false;
    let mut reasoning_text = String::new();
    let mut reasoning_item_added = false;
    let mut tool_calls: BTreeMap<usize, ToolCallAccum> = BTreeMap::new();
    let mut latest_usage: Option<TokenUsage> = None;
    let mut finish_reason: Option<String> = None;
    let mut server_model_emitted = false;

    loop {
        let start = Instant::now();
        let response = timeout(idle_timeout, stream.next()).await;
        if let Some(t) = telemetry.as_ref() {
            t.on_sse_poll(&response, start.elapsed());
        }
        let sse = match response {
            Ok(Some(Ok(sse))) => sse,
            Ok(Some(Err(err))) => {
                let _ = tx_event.send(Err(ApiError::Stream(err.to_string()))).await;
                return;
            }
            Ok(None) => break,
            Err(_) => {
                let _ = tx_event
                    .send(Err(ApiError::Stream(
                        "idle timeout waiting for SSE".to_string(),
                    )))
                    .await;
                return;
            }
        };

        let data = sse.data.trim();
        if data.is_empty() {
            continue;
        }
        if data == "[DONE]" {
            break;
        }

        let chunk: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(err_obj) = chunk.get("error") {
            let msg = err_obj
                .get("message")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| err_obj.to_string());
            let _ = tx_event.send(Err(ApiError::Stream(msg))).await;
            return;
        }

        if let Some(id) = chunk.get("id").and_then(Value::as_str)
            && !id.is_empty()
        {
            response_id = id.to_string();
        }

        if !created_sent {
            created_sent = true;
            if tx_event
                .send(Ok(ResponseEvent::Created {
                    response_id: Some(response_id.clone()),
                }))
                .await
                .is_err()
            {
                return;
            }
        }

        if !server_model_emitted
            && let Some(model) = chunk.get("model").and_then(Value::as_str)
            && !model.is_empty()
        {
            server_model_emitted = true;
            let _ = tx_event
                .send(Ok(ResponseEvent::ServerModel(model.to_string())))
                .await;
        }

        if let Some(usage) = chunk.get("usage").and_then(parse_chat_usage) {
            latest_usage = Some(usage);
        }

        if let Some(choices) = chunk.get("choices").and_then(Value::as_array) {
            for choice in choices {
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str)
                    && !reason.is_empty()
                {
                    finish_reason = Some(reason.to_string());
                }
                let Some(delta) = choice.get("delta") else {
                    continue;
                };

                if let Some(reasoning_delta) = delta
                    .get("reasoning_content")
                    .or_else(|| delta.get("reasoning"))
                    .and_then(Value::as_str)
                    && !reasoning_delta.is_empty()
                {
                    if !reasoning_item_added {
                        reasoning_item_added = true;
                        let item = ResponseItem::Reasoning {
                            id: Some(ResponseItemId::with_suffix("rs", &response_id)),
                            summary: Vec::new(),
                            content: Some(Vec::new()),
                            encrypted_content: None,
                            internal_chat_message_metadata_passthrough: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    reasoning_text.push_str(reasoning_delta);
                    if tx_event
                        .send(Ok(ResponseEvent::ReasoningContentDelta {
                            delta: reasoning_delta.to_string(),
                            content_index: 0,
                        }))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                if let Some(text_delta) = delta.get("content").and_then(Value::as_str)
                    && !text_delta.is_empty()
                {
                    if !message_item_added {
                        message_item_added = true;
                        let item = ResponseItem::Message {
                            id: Some(ResponseItemId::with_suffix("msg", &response_id)),
                            role: "assistant".to_string(),
                            content: Vec::new(),
                            status: Some("in_progress".to_string()),
                            encrypted_content: None,
                            phase: None,
                            internal_chat_message_metadata_passthrough: None,
                        };
                        if tx_event
                            .send(Ok(ResponseEvent::OutputItemAdded(item)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                    assistant_text.push_str(text_delta);
                    if tx_event
                        .send(Ok(ResponseEvent::OutputTextDelta(text_delta.to_string())))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }

                if let Some(tc_list) = delta.get("tool_calls").and_then(Value::as_array) {
                    for (pos, tc) in tc_list.iter().enumerate() {
                        let idx = tc
                            .get("index")
                            .and_then(Value::as_u64)
                            .map(|v| v as usize)
                            .unwrap_or(pos);
                        let entry = tool_calls.entry(idx).or_default();
                        if let Some(id) = tc.get("id").and_then(Value::as_str)
                            && !id.is_empty()
                        {
                            entry.id = id.to_string();
                        }
                        if let Some(func) = tc.get("function") {
                            if let Some(name) = func.get("name").and_then(Value::as_str)
                                && !name.is_empty()
                            {
                                if entry.name.is_empty() {
                                    entry.name = name.to_string();
                                } else {
                                    entry.name.push_str(name);
                                }
                            }
                            if let Some(args) = func.get("arguments").and_then(Value::as_str)
                                && !args.is_empty()
                            {
                                entry.arguments.push_str(args);
                                let item_id = if entry.id.is_empty() {
                                    format!("fc_{response_id}_{idx}")
                                } else {
                                    format!("fc_{}", entry.id)
                                };
                                let _ = tx_event
                                    .send(Ok(ResponseEvent::ToolCallInputDelta {
                                        item_id,
                                        call_id: (!entry.id.is_empty()).then(|| entry.id.clone()),
                                        delta: args.to_string(),
                                    }))
                                    .await;
                            }
                        }
                    }
                }
            }
        }
    }

    if !created_sent {
        let _ = tx_event
            .send(Ok(ResponseEvent::Created {
                response_id: Some(response_id.clone()),
            }))
            .await;
    }

    if reasoning_item_added {
        let item = ResponseItem::Reasoning {
            id: Some(ResponseItemId::with_suffix("rs", &response_id)),
            summary: Vec::new(),
            content: Some(vec![ReasoningItemContent::ReasoningText {
                text: reasoning_text,
            }]),
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    if message_item_added {
        let item = ResponseItem::Message {
            id: Some(ResponseItemId::with_suffix("msg", &response_id)),
            role: "assistant".to_string(),
            content: vec![ContentItem::OutputText {
                text: assistant_text,
                annotations: None,
                logprobs: None,
            }],
            status: Some("completed".to_string()),
            encrypted_content: None,
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    let has_tool_calls = !tool_calls.is_empty();
    for (idx, tc) in tool_calls {
        let call_id = if tc.id.is_empty() {
            format!("call_{response_id}_{idx}")
        } else {
            tc.id
        };
        let item_id = Some(ResponseItemId::with_suffix("fc", &call_id));
        let item = if custom_tool_names.contains(&tc.name) {
            let input = serde_json::from_str::<Value>(&tc.arguments)
                .ok()
                .and_then(|v| v.get("input").and_then(Value::as_str).map(str::to_string))
                .unwrap_or(tc.arguments);
            ResponseItem::CustomToolCall {
                id: item_id,
                status: Some("completed".to_string()),
                call_id,
                name: tc.name,
                namespace: None,
                input,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            }
        } else {
            let arguments = if tc.arguments.trim().is_empty() {
                "{}".to_string()
            } else {
                tc.arguments
            };
            ResponseItem::FunctionCall {
                id: item_id,
                status: Some("completed".to_string()),
                name: tc.name,
                namespace: None,
                arguments,
                encrypted_function_args: None,
                call_id,
                encrypted_content: None,
                internal_chat_message_metadata_passthrough: None,
            }
        };
        if tx_event
            .send(Ok(ResponseEvent::OutputItemDone(item)))
            .await
            .is_err()
        {
            return;
        }
    }

    let end_turn = match finish_reason.as_deref() {
        Some("tool_calls") => Some(false),
        Some("stop") => Some(!has_tool_calls),
        _ => (!has_tool_calls).then_some(true),
    };

    let _ = tx_event
        .send(Ok(ResponseEvent::Completed {
            response_id,
            token_usage: latest_usage,
            usage_metadata: None,
            end_turn,
        }))
        .await;
}

fn parse_chat_usage(usage: &Value) -> Option<TokenUsage> {
    let obj = usage.as_object()?;
    let input_tokens = obj.get("prompt_tokens")?.as_i64()?;
    let output_tokens = obj.get("completion_tokens")?.as_i64()?;
    let total_tokens = obj
        .get("total_tokens")
        .and_then(Value::as_i64)
        .unwrap_or(input_tokens + output_tokens);
    let cached_input_tokens = obj
        .get("prompt_tokens_details")
        .and_then(|d| d.get("cached_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let reasoning_output_tokens = obj
        .get("completion_tokens_details")
        .and_then(|d| d.get("reasoning_tokens"))
        .and_then(Value::as_i64)
        .unwrap_or(0);

    Some(TokenUsage {
        input_tokens,
        cached_input_tokens,
        cache_write_input_tokens: 0,
        output_tokens,
        reasoning_output_tokens,
        total_tokens,
        codex_rollout_budget_units: None,
    })
}
