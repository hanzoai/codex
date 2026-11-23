use super::*;
use pretty_assertions::assert_eq;
use rmcp::model::ResourceContents;
use serde_json::json;

fn resource(uri: &str, name: &str) -> Resource {
    Resource::new(uri, name)
}

fn template(uri_template: &str, name: &str) -> ResourceTemplate {
    ResourceTemplate::new(uri_template, name)
}

#[test]
fn resource_with_server_serializes_server_field() {
    let entry = ResourceWithServer::new("test".to_string(), resource("memo://id", "memo"));
    let value = serde_json::to_value(&entry).expect("serialize resource");

    assert_eq!(value["server"], json!("test"));
    assert_eq!(value["uri"], json!("memo://id"));
    assert_eq!(value["name"], json!("memo"));
}

#[test]
fn list_resources_payload_from_single_server_copies_next_cursor() {
    let mut result = ListResourcesResult::with_all_items(vec![resource("memo://id", "memo")]);
    result.next_cursor = Some("cursor-1".to_string());
    let payload = ListResourcesPayload::from_single_server("srv".to_string(), result);
    let value = serde_json::to_value(&payload).expect("serialize payload");

    assert_eq!(value["server"], json!("srv"));
    assert_eq!(value["nextCursor"], json!("cursor-1"));
    let resources = value["resources"].as_array().expect("resources array");
    assert_eq!(resources.len(), 1);
    assert_eq!(resources[0]["server"], json!("srv"));
}

#[test]
fn list_resources_payload_from_all_servers_is_sorted() {
    let mut map = HashMap::new();
    map.insert("beta".to_string(), vec![resource("memo://b-1", "b-1")]);
    map.insert(
        "alpha".to_string(),
        vec![resource("memo://a-1", "a-1"), resource("memo://a-2", "a-2")],
    );

    let payload = ListResourcesPayload::from_all_servers(McpResourceListing {
        by_server: map,
        deferred_servers: Vec::new(),
    });
    let value = serde_json::to_value(&payload).expect("serialize payload");
    let uris: Vec<String> = value["resources"]
        .as_array()
        .expect("resources array")
        .iter()
        .map(|entry| entry["uri"].as_str().unwrap().to_string())
        .collect();

    assert_eq!(
        uris,
        vec![
            "memo://a-1".to_string(),
            "memo://a-2".to_string(),
            "memo://b-1".to_string()
        ]
    );
}

#[test]
fn call_tool_result_from_content_marks_success() {
    let result = call_tool_result_from_content("{}", Some(true));
    assert_eq!(result.is_error, Some(false));
    assert_eq!(result.content.len(), 1);
}

#[test]
fn parse_arguments_handles_empty_and_json() {
    assert!(
        parse_arguments(" \n\t").unwrap().is_none(),
        "expected None for empty arguments"
    );

    assert!(
        parse_arguments("null").unwrap().is_none(),
        "expected None for null arguments"
    );

    let value = parse_arguments(r#"{"server":"figma"}"#)
        .expect("parse json")
        .expect("value present");
    assert_eq!(value["server"], json!("figma"));
}

#[test]
fn list_resource_args_normalizes_server_and_cursor() {
    let args: ListResourceArgs = serde_json::from_value(json!({
        "server": "  hosted  ",
        "cursor": "  next-page  "
    }))
    .expect("parse resource-list arguments");

    assert_eq!(
        args.normalized(),
        ListResourceArgs {
            server: Some("hosted".to_string()),
            cursor: Some("next-page".to_string()),
        }
    );
}

#[test]
fn template_with_server_serializes_server_field() {
    let entry = ResourceWithServer::new("srv".to_string(), template("memo://{id}", "memo"));
    let value = serde_json::to_value(&entry).expect("serialize template");

    assert_eq!(
        value,
        json!({
            "server": "srv",
            "uriTemplate": "memo://{id}",
            "name": "memo"
        })
    );
}

#[test]
fn list_resource_templates_payload_from_all_servers_is_sorted() {
    let mut templates_by_server = HashMap::new();
    templates_by_server.insert(
        "beta".to_string(),
        vec![template("memo://beta/{id}", "beta")],
    );
    templates_by_server.insert(
        "alpha".to_string(),
        vec![template("memo://alpha/{id}", "alpha")],
    );

    let payload = ListResourceTemplatesPayload::from_all_servers(McpResourceListing {
        by_server: templates_by_server,
        deferred_servers: Vec::new(),
    });

    assert_eq!(
        serde_json::to_value(payload).expect("serialize resource templates"),
        json!({
            "resourceTemplates": [
                {"server": "alpha", "uriTemplate": "memo://alpha/{id}", "name": "alpha"},
                {"server": "beta", "uriTemplate": "memo://beta/{id}", "name": "beta"}
            ]
        })
    );
}

#[test]
fn serialize_function_output_preserves_small_payload() {
    let payload = json!({"server": "hosted", "resources": []});
    let expected = serde_json::to_string(&payload).expect("serialize payload");

    let output = serialize_function_output(payload, TruncationPolicy::Bytes(1_024))
        .expect("serialize function output")
        .into_text();

    assert_eq!(output, expected);
}

#[test]
fn serialize_function_output_caps_read_resource_payload() {
    let truncation_policy = TruncationPolicy::Bytes(8_000);
    let payload = ReadResourcePayload {
        server: "hosted".to_string(),
        uri: "skill://large/SKILL.md".to_string(),
        result: ReadResourceResult::new(vec![ResourceContents::TextResourceContents {
            uri: "skill://large/SKILL.md".to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: "x".repeat(16_000),
            meta: None,
        }]),
    };
    let serialized = serde_json::to_string(&payload).expect("serialize payload");
    let expected = truncate_text(&serialized, truncation_policy * 1.2);

    let output = serialize_function_output(payload, truncation_policy)
        .expect("serialize bounded function output")
        .into_text();

    assert_ne!(output, serialized);
    assert_eq!(output, expected);
}

/// hanzo-inc/cloud#781: a model that left `server` out of `read_mcp_resource` got
/// `failed to parse function arguments: missing field `server`` from the router.
#[test]
fn read_resource_args_take_the_server_as_optional() {
    assert_eq!(
        ReadResourceArgs::parse(Some(json!({"uri": " memo://id "}))).expect("parse without server"),
        ReadResourceArgs {
            server: None,
            uri: "memo://id".to_string(),
        }
    );
    assert_eq!(
        ReadResourceArgs::parse(Some(json!({"server": "  ", "uri": "memo://id"})))
            .expect("parse blank server")
            .server,
        None
    );
    assert_eq!(
        ReadResourceArgs::parse(Some(json!({"server": " srv ", "uri": "memo://id"})))
            .expect("parse with server")
            .server,
        Some("srv".to_string())
    );
    assert!(ReadResourceArgs::parse(Some(json!({"server": "srv"}))).is_err());
}

fn listing(
    by_server: &[(&str, &[&str])],
    deferred_servers: &[&str],
) -> McpResourceListing<Resource> {
    McpResourceListing {
        by_server: by_server
            .iter()
            .map(|(server, uris)| {
                let resources = uris.iter().map(|uri| resource(uri, uri)).collect();
                (server.to_string(), resources)
            })
            .collect(),
        deferred_servers: deferred_servers.iter().map(ToString::to_string).collect(),
    }
}

fn refusal(result: Result<String, FunctionCallError>) -> String {
    match result {
        Err(FunctionCallError::RespondToModel(message)) => message,
        other => panic!("expected a refusal to the model, got {other:?}"),
    }
}

#[test]
fn a_read_without_a_server_goes_to_the_server_listing_its_uri() {
    let servers = listing(
        &[
            ("cloud", &["cloud://a"]),
            ("hanzo", &["hanzo://kit", "hanzo://b"]),
        ],
        &["slow"],
    );
    assert_eq!(
        resource_server_for("hanzo://kit", &servers).expect("one owner"),
        "hanzo"
    );
}

#[test]
fn a_read_without_a_server_goes_to_the_only_server() {
    let servers = listing(&[("hanzo", &[])], &[]);
    assert_eq!(
        resource_server_for("hanzo://from-a-template", &servers).expect("only server"),
        "hanzo"
    );
}

#[test]
fn a_read_the_servers_cannot_settle_names_the_choices() {
    let unlisted = refusal(resource_server_for(
        "memo://nowhere",
        &listing(&[("hanzo", &[]), ("cloud", &[])], &["slow"]),
    ));
    assert!(unlisted.contains("needs `server`"), "{unlisted}");
    assert!(
        unlisted.contains("no server lists `memo://nowhere`, so pass one of: cloud, hanzo, slow"),
        "{unlisted}"
    );

    let shared = refusal(resource_server_for(
        "memo://both",
        &listing(
            &[("beta", &["memo://both"]), ("alpha", &["memo://both"])],
            &[],
        ),
    ));
    assert!(
        shared.contains("`memo://both` is listed by alpha, beta"),
        "{shared}"
    );

    let none = refusal(resource_server_for("memo://x", &listing(&[], &[])));
    assert!(
        none.contains("no MCP server is serving resources"),
        "{none}"
    );
}

/// A server that only deferred its listing could still own the URI, so a single ready
/// server is not "the only server" while another is still starting.
#[test]
fn a_starting_server_keeps_a_read_from_being_guessed() {
    let message = refusal(resource_server_for(
        "memo://x",
        &listing(&[("hanzo", &[])], &["cloud"]),
    ));
    assert!(message.contains("pass one of: cloud, hanzo"), "{message}");
}

#[test]
fn list_resource_args_default_invocation_server_uses_hanzo_app_slug() {
    let args = ListResourceArgs::parse(Some(json!({}))).expect("parse empty args");
    assert_eq!(
        args.server.unwrap_or_else(|| hanzo::APP_SLUG.to_string()),
        "hanzo"
    );

    let blank = ListResourceArgs::parse(Some(json!({"server": "   "}))).expect("parse blank args");
    assert_eq!(
        blank.server.unwrap_or_else(|| hanzo::APP_SLUG.to_string()),
        "hanzo"
    );

    let explicit =
        ListResourceArgs::parse(Some(json!({"server": " docs "}))).expect("parse explicit server");
    assert_eq!(
        explicit
            .server
            .unwrap_or_else(|| hanzo::APP_SLUG.to_string()),
        "docs"
    );
}
