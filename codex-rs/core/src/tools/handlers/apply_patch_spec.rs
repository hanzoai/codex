use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

const APPLY_PATCH_LARK_GRAMMAR: &str = include_str!("../../../assets/tools/apply_patch.lark");

/// Returns a custom tool that can be used to edit files. Well-suited for GPT-5 models
/// https://platform.openai.com/docs/guides/function-calling#custom-tools
pub fn create_apply_patch_freeform_tool(include_environment_id: bool) -> ToolSpec {
    let definition = if include_environment_id {
        APPLY_PATCH_LARK_GRAMMAR.replace(
            "start: begin_patch hunk+ end_patch",
            "start: begin_patch environment_id? hunk+ end_patch\nenvironment_id: \"*** Environment ID: \" filename LF",
        )
    } else {
        APPLY_PATCH_LARK_GRAMMAR.to_string()
    };
    ToolSpec::Freeform(FreeformTool {
        name: "apply_patch".to_string(),
        description: "The `apply_patch` tool can be used to edit files. This is a FREEFORM tool, so do not wrap the patch in JSON.".to_string(),
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition,
        },
    })
}

/// Returns a standard JSON function `apply_patch` tool compatible with all OpenAI-compatible
/// `/v1/responses` providers (Enso, Zen, Claude, Gemini, Qwen, DeepSeek, local engines).
pub fn create_apply_patch_json_tool(include_environment_id: bool) -> ToolSpec {
    let mut properties = BTreeMap::from([(
        "patch".to_string(),
        JsonSchema::string(Some(
            "Patch text starting with `*** Begin Patch` and ending with `*** End Patch` (using `*** Add File: <path>`, `*** Update File: <path>`, or `*** Delete File: <path>` sections)."
                .to_string(),
        )),
    )]);
    if include_environment_id {
        properties.insert(
            "environment_id".to_string(),
            JsonSchema::string(Some(
                "Environment id from <environment_context>. Omit to use the primary environment."
                    .to_string(),
            )),
        );
    }
    ToolSpec::Function(ResponsesApiTool {
        name: "apply_patch".to_string(),
        description: "Edit, create, or delete files in the workspace using a structured patch (`*** Begin Patch ... *** End Patch`).".to_string(),
        strict: false,
        defer_loading: None,
        parameters: JsonSchema::object(
            properties,
            Some(vec!["patch".to_string()]),
            Some(false.into()),
        ),
        output_schema: None,
    })
}

#[cfg(test)]
#[path = "apply_patch_spec_tests.rs"]
mod tests;
