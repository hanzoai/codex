use codex_protocol::config_types::Personality;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ConfigShellToolType;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelVisibility;
use codex_protocol::openai_models::TruncationMode;
use codex_protocol::openai_models::TruncationPolicyConfig;
use codex_protocol::openai_models::WebSearchToolType;
use codex_protocol::openai_models::default_input_modalities;

use crate::config::ModelsManagerConfig;
use codex_utils_output_truncation::approx_bytes_for_tokens;
use tracing::warn;

pub const BASE_INSTRUCTIONS: &str = include_str!("../prompt.md");
const PERSONALITY_SECTION_HEADER: &str = "# Personality";

pub fn with_config_overrides(mut model: ModelInfo, config: &ModelsManagerConfig) -> ModelInfo {
    if let Some(context_window) = config.model_context_window {
        model.context_window = Some(
            model
                .max_context_window
                .map_or(context_window, |max_context_window| {
                    context_window.min(max_context_window)
                }),
        );
    }
    if let Some(auto_compact_token_limit) = config.model_auto_compact_token_limit {
        model.auto_compact_token_limit = Some(auto_compact_token_limit);
    }
    if let Some(token_limit) = config.tool_output_token_limit {
        model.truncation_policy = match model.truncation_policy.mode {
            TruncationMode::Bytes => {
                let byte_limit =
                    i64::try_from(approx_bytes_for_tokens(token_limit)).unwrap_or(i64::MAX);
                TruncationPolicyConfig::bytes(byte_limit)
            }
            TruncationMode::Tokens => {
                let limit = i64::try_from(token_limit).unwrap_or(i64::MAX);
                TruncationPolicyConfig::tokens(limit)
            }
        };
    }

    if let Some(base_instructions) = &config.base_instructions {
        let model_messages = model.model_messages.get_or_insert_default();
        model_messages.instructions_template = Some(base_instructions.clone());
    } else if config.personality == Some(Personality::None)
        && let Some(instructions_template) = model
            .model_messages
            .as_mut()
            .and_then(|messages| messages.instructions_template.as_mut())
    {
        *instructions_template = strip_personality_section(std::mem::take(instructions_template));
    }

    model
}

fn strip_personality_section(mut instructions: String) -> String {
    let mut section_start = None;
    let mut section_end = None;
    let mut offset = 0;

    for line_with_ending in instructions.split_inclusive('\n') {
        let line = match line_with_ending.strip_suffix('\n') {
            Some(line) => line.strip_suffix('\r').unwrap_or(line),
            None => line_with_ending,
        };
        if section_start.is_some() {
            if is_h1_heading(line) {
                section_end = Some(offset);
                break;
            }
        } else if line == PERSONALITY_SECTION_HEADER {
            section_start = Some(offset);
        }
        offset += line_with_ending.len();
    }

    if let Some(section_start) = section_start {
        let section_end = section_end.unwrap_or(instructions.len());
        instructions.replace_range(section_start..section_end, "");
    }

    instructions
}

fn is_h1_heading(line: &str) -> bool {
    let Some(rest) = line.strip_prefix('#') else {
        return false;
    };
    rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t')
}

/// Build a minimal fallback model descriptor for missing/unknown slugs.
pub fn model_info_from_slug(slug: &str) -> ModelInfo {
    let known = hanzo::catalog::find(slug);
    if known.is_none() {
        warn!("Unknown model {slug} is used. This will use fallback model metadata.");
    }
    let window = known.map_or(272_000, |model| model.window);
    ModelInfo {
        slug: slug.to_string(),
        display_name: known.map_or_else(|| slug.to_string(), |model| model.name.to_string()),
        description: known.map(|model| model.name.to_string()),
        // Hanzo: every model (curated or dynamic /v1/models / local) gets reasoning-effort controls.
        default_reasoning_level: hanzo::effort::DEFAULT.parse().ok(),
        supported_reasoning_levels: hanzo::effort::EFFORTS
            .iter()
            .filter_map(|(name, description)| {
                Some(codex_protocol::openai_models::ReasoningEffortPreset {
                    effort: name.parse().ok()?,
                    description: (*description).to_string(),
                })
            })
            .collect(),
        shell_type: ConfigShellToolType::UnifiedExec,
        visibility: ModelVisibility::None,
        supported_in_api: true,
        priority: 99,
        additional_speed_tiers: if known.is_some() {
            vec!["fast".to_string()]
        } else {
            Vec::new()
        },
        service_tiers: if known.is_some() {
            vec![codex_protocol::openai_models::ModelServiceTier {
                id: "priority".to_string(),
                name: "Fast".to_string(),
                description: "2x speed, increased usage".to_string(),
            }]
        } else {
            Vec::new()
        },
        default_service_tier: None,
        available_access_programs: None,
        availability_nux: None,
        upgrade: None,
        model_messages: Some(if known.is_some() {
            hanzo_model_messages()
        } else {
            local_model_messages()
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
        truncation_policy: TruncationPolicyConfig::bytes(/*limit*/ 10_000),
        supports_image_detail_original: false,
        context_window: Some(window),
        max_context_window: Some(window),
        auto_compact_token_limit: None,
        comp_hash: None,
        effective_context_window_percent: 95,
        experimental_supported_tools: Vec::new(),
        input_modalities: match known {
            Some(model) if !model.vision => vec![codex_protocol::openai_models::InputModality::Text],
            _ => default_input_modalities(),
        },
        supports_search_tool: false,
        supports_experimental_context: false,
        use_responses_lite: false,
        supports_reasoning_effort_updates: false,
        guardian: None,
        node_repl_auto_review_required: false,
        node_repl_disabled: false,
        auto_review_model_override: None,
        model_specialty: None,
        tool_mode: None,
        multi_agent_version: None,
        multi_agent_reasoning_effort: None,
    }
}

const HANZO_INSTRUCTIONS_SUPPLEMENT: &str = r#"

# Hanzo Dev: Native Enso, Kai, Zen, HIP-0300 MCP & OSS Skills

- **Identity & Runtime**: You are running inside **Hanzo Dev (`dev`)**, the sovereign terminal coding agent by Hanzo AI:
  - **Enso** (`enso`, `enso-auto`, `enso-pro`, `enso-ultra`): Multi-model coding orchestration across the Hanzo catalog (`api.hanzo.ai/v1`).
  - **Kai** (`kai`, `~/.hanzo/dev/kai.toml`): Real-time decision agent over the Hanzo Decisions API (`api.hanzo.ai/v1/decisions`) providing model/reasoning routing, trigram workspace context discovery (`hanzo-trigram`), skill & tool selection, command risk gating, progress steering, and completion checks.
  - **Zen** (`zen5`, `zen5-pro`, `zen5-coder`, `zen6`, `zen6-flash`): Hanzo flagship reasoning and coding models (`hanzoai/engine`).
- **Native Agentic Coding Flow (Built-in Codex Tools First)**:
  - Always use built-in **`exec_command`** (and **`write_stdin`**) for all workspace exploration (`rg`, `rg --files`, `ls`, `sed -n`, `cat`, `head`, `tail`, `find`), git commands (`git status`, `git diff`, `git log`), builds, and test runs so the TUI groups read/search/list operations into native **`Exploring` / `Explored`** cells.
  - Always use built-in **`apply_patch`** (`{"patch": "*** Begin Patch\n*** Update File: ..."}`) to create, update, or delete files so changes render as native diff cells.
  - Use **`update_plan`** for multi-step task plans, **`view_image`** for local images, **`request_user_input`** for structured user questions, and **`spawn_agent`** / subagent tools for parallel subtasks.
  - Never use MCP `fs` or `exec` tools for local file reading, listing, searching, patching, or shell execution.
- **Slim Companion MCP (`hanzo-mcp` / `@hanzo/mcp`)**: `dev` connects to `hanzo-mcp` (`mcp_servers.hanzo`) for the 5 specialized non-overlapping tools (`mcp__hanzo__*`):
  - `code_search`: Trigram symbol and cross-repo code search (`hanzo-trigram`).
  - `lsp`: Language Server Protocol navigation (`definition`, `references`, `diagnostics`, `symbols`).
  - `browser`: Persistent browser automation (`hanzo-browser` / ZAP bridge `$XDG_RUNTIME_DIR/zap/zapd.sock` / CDP `127.0.0.1:9222`).
  - `web_search`: Live web search.
  - `web_read`: Clean web page and documentation extraction.
- **Native Hanzo OSS Skills Stack (`SKILL.md` & `hanzoskills.com`)**:
  - Skills live in `<cwd>/.hanzo/dev/skills/*/SKILL.md`, `<cwd>/.hanzo/skills/*/SKILL.md`, `<cwd>/.agents/skills/*/SKILL.md`, and `~/.hanzo/dev/skills/*/SKILL.md` (including built-in `dev`, `code-review`, `ui`, `hanzo-mcp`, and `hanzo-oss` skills).
  - Use progressive disclosure when consulting `hanzoskills.com`: Level 0 index (`https://hanzoskills.com/mcp/index.json` and `https://hanzoskills.com/oss/index.json`) -> Level 1 per-tool/skill (`https://hanzoskills.com/mcp/<tool>.json` or `https://hanzoskills.com/<skill>.md`) -> Level 2 live contract (`POST https://api.hanzo.ai/v1/mcp` `describe` or `hanzo <group> --help`).
  - When working in Hanzo repositories (`hanzoai/*`, `zap-proto/*`), use Hanzo IAM (`https://hanzo.id`), Hanzo KMS (`https://kms.hanzo.ai`), unified `/v1/*` routes on `https://api.hanzo.ai`, Hanzo CD (`https://cd.hanzo.ai`), and the `hanzo` CLI.
- **Diagnostics & Bug Reporting**: Users can run `dev doctor` (`hanzo doctor --share`) or `dev report` (`/report`, `/bug`, `/share` inside the TUI) at any time to bundle diagnostics, session rollouts, and Kai decision ledgers for Hanzo engineering.
- **Never End a Turn on a Preamble**: When you send a brief status update or preamble announcing your next action (e.g. "Let me inspect...", "I'll run the tests...", "Now checking..."), you **must** emit the corresponding tool call in the **same** response. Never stop generating right after a preamble without calling the tool; only finish a turn without a tool call when the user's request is genuinely complete or when blocked on required user input.
"#;

fn hanzo_model_messages() -> ModelMessages {
    let base = BASE_INSTRUCTIONS
        .replace(
            "You are a coding agent running in the Codex CLI, a terminal-based coding assistant. Codex CLI is an open source project led by OpenAI.",
            "You are a coding agent running in Hanzo Dev (`dev`), a terminal-based coding assistant built by Hanzo AI.",
        )
        .replace(
            "Within this context, Codex refers to the open-source agentic coding interface (not the old Codex language model built by OpenAI).",
            "Within this context, Hanzo Dev (`dev`) refers to the agentic coding interface paired with Hanzo MCP (`hanzo-mcp`) and Hanzo Skills (`SKILL.md`).",
        )
        .replace(
            r#"{"command":["apply_patch","*** Begin Patch\n*** Update File: path/to/file.py\n@@ def example():\n- pass\n+ return 123\n*** End Patch"]}"#,
            r#"{"patch":"*** Begin Patch\n*** Update File: path/to/file.py\n@@ def example():\n- pass\n+ return 123\n*** End Patch"}"#,
        );
    ModelMessages {
        instructions_template: Some(format!("{base}{HANZO_INSTRUCTIONS_SUPPLEMENT}")),
        ..Default::default()
    }
}

fn local_model_messages() -> ModelMessages {
    ModelMessages {
        instructions_template: Some(BASE_INSTRUCTIONS.to_string()),
        ..Default::default()
    }
}

#[cfg(test)]
#[path = "model_info_tests.rs"]
mod tests;
