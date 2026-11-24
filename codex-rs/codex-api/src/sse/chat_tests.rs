#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use crate::common::ResponsesApiRequest;
use crate::common::ResponsesApiTools;
use codex_protocol::models::ResponseItem;
use serde_json::json;
use serde_json::value::RawValue;
use std::sync::Arc;

#[test]
fn chat_completions_flattens_namespace_tools_and_preserves_roundtrip_metadata() {
    let raw_tools = json!([
        {
            "type": "function",
            "name": "exec_command",
            "description": "Execute shell command",
            "parameters": { "type": "object", "properties": {} }
        },
        {
            "type": "namespace",
            "name": "mcp__hanzo",
            "description": "Hanzo MCP tools",
            "tools": [
                {
                    "type": "function",
                    "name": "fs",
                    "description": "Unified filesystem tool",
                    "parameters": { "type": "object", "properties": { "action": { "type": "string" } } }
                },
                {
                    "type": "function",
                    "name": "kai",
                    "description": "Kai action runner",
                    "parameters": { "type": "object", "properties": { "request": { "type": "string" } } }
                }
            ]
        },
        {
            "type": "namespace",
            "name": "multi_agent_v1",
            "description": "Subagent tools",
            "tools": [
                {
                    "type": "function",
                    "name": "spawn_agent",
                    "description": "Spawn subagent",
                    "parameters": { "type": "object", "properties": {} }
                }
            ]
        }
    ]);
    let tools_raw: Arc<RawValue> = Arc::from(RawValue::from_string(raw_tools.to_string()).unwrap());

    let req = ResponsesApiRequest {
        model: "local-test".to_string(),
        stream: true,
        service_tier: None,
        input: vec![ResponseItem::FunctionCall {
            id: None,
            status: Some("completed".to_string()),
            name: "fs".to_string(),
            namespace: Some("mcp__hanzo".to_string()),
            arguments: "{\"action\":\"read\"}".to_string(),
            encrypted_function_args: None,
            call_id: "call_1".to_string(),
            encrypted_content: None,
            internal_chat_message_metadata_passthrough: None,
        }],
        tools: Some(ResponsesApiTools::from(tools_raw)),
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

    let (body, meta) = build_chat_completions_request(&req);
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();

    assert_eq!(
        names,
        vec![
            "exec_command",
            "mcp__hanzo__fs",
            "mcp__hanzo__kai",
            "multi_agent_v1__spawn_agent",
        ]
    );
    assert_eq!(
        meta.namespaced_tools.get("mcp__hanzo__fs"),
        Some(&("mcp__hanzo".to_string(), "fs".to_string()))
    );
    assert_eq!(
        meta.namespaced_tools.get("multi_agent_v1__spawn_agent"),
        Some(&("multi_agent_v1".to_string(), "spawn_agent".to_string()))
    );
    assert_eq!(
        body["messages"][0]["tool_calls"][0]["function"]["name"].as_str(),
        Some("mcp__hanzo__fs")
    );
}
