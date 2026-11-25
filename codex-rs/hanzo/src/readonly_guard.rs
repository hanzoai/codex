//! Deterministic Read-Only Turn Guard for user prompts requesting a read-only scan,
//! audit, or explicit confirmation before modifying files.
//!
//! Even when the user invokes the CLI without `--sandbox read-only` or `/plan` (where the
//! default configuration uses `approval_policy = "never"` and `sandbox_mode = "workspace-write"`),
//! an explicit prompt directive such as *"do a read-only repo scan first and wait for
//! confirmation before writing code"* activates [`ReadOnlyTurnGuard`] until a subsequent
//! real user turn confirms proceeding.

use std::sync::Arc;
use std::sync::Mutex;

use serde_json::Value;

/// Developer instruction injected into active turns while [`ReadOnlyTurnGuard`] is locked.
pub const READ_ONLY_DEVELOPER_REMINDER: &str = concat!(
    "READ-ONLY TURN GUARD ACTIVE: The user explicitly requested a read-only scan, audit, or ",
    "confirmation before modifying any files. You MUST inspect/search/read only, summarize ",
    "your findings and proposed plan clearly, and STOP to wait for user confirmation. ",
    "Do NOT call `apply_patch`, file-writing MCP tools, or file-mutating shell commands in this turn."
);

/// Standard rejection message returned to the model when a mutating tool call is gated.
pub fn blocked_tool_message(reason: &str) -> String {
    format!(
        "Blocked: user requested a read-only scan / confirmation before modifying files ({reason}). \
         Do NOT edit, write, delete, or patch any files yet — summarize your findings and wait for user confirmation."
    )
}

#[derive(Debug, Default)]
struct GuardState {
    active: bool,
    directive: String,
}

/// Thread-safe per-session read-only confirmation gate.
#[derive(Clone, Debug, Default)]
pub struct ReadOnlyTurnGuard {
    state: Arc<Mutex<GuardState>>,
}

impl ReadOnlyTurnGuard {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn is_active(&self) -> bool {
        self.lock().active
    }

    /// Updates guard state when a *real user turn* (not an automatic continuation or hook)
    /// submits `user_text`. Returns `true` if the guard is active for the turn.
    pub fn on_user_turn(&self, user_text: &str) -> bool {
        let trimmed = user_text.trim();
        if trimmed.is_empty() {
            return self.is_active();
        }
        let mut state = self.lock();
        if detects_readonly_directive(trimmed) {
            state.active = true;
            state.directive = trimmed.chars().take(240).collect();
        } else if state.active && detects_user_confirmation(trimmed) {
            state.active = false;
            state.directive.clear();
        }
        state.active
    }
}

/// Strips fenced code blocks and `>` blockquotes so quoted bug descriptions or code
/// samples do not trigger false-positive intent detection.
fn strip_code_and_quotes(prompt: &str) -> String {
    let mut out = String::with_capacity(prompt.len());
    let mut in_fence = false;
    for line in prompt.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence || trimmed.starts_with('>') {
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn normalize_ws(text: &str) -> String {
    text.to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Returns `true` when `prompt` contains an explicit user directive to perform a
/// read-only scan/audit/plan or wait for confirmation before writing/modifying files,
/// with zero false positives on ordinary bug-fix requests.
pub fn detects_readonly_directive(prompt: &str) -> bool {
    let cleaned = strip_code_and_quotes(prompt);
    let norm = normalize_ws(&cleaned);
    if norm.is_empty() {
        return false;
    }

    // 1. Explicit prohibition on editing/modifying/writing files.
    const PROHIBITIONS: &[&str] = &[
        "do not edit",
        "do not modify",
        "do not write",
        "do not change",
        "do not patch",
        "do not touch",
        "do not create or modify",
        "do not make any changes",
        "do not make changes",
        "don't edit",
        "don't modify",
        "don't write",
        "don't change",
        "don't patch",
        "don't touch",
        "don't make any changes",
        "don't make changes",
        "never edit",
        "never modify",
        "never write files",
        "without editing",
        "without modifying",
        "without writing",
        "without changing",
        "without patching",
        "without touching",
        "without making any changes",
        "without making changes",
        "no file edits",
        "no file modifications",
        "no file changes",
        "no file writes",
        "no code changes",
        "make no changes",
    ];
    if PROHIBITIONS.iter().any(|pat| norm.contains(pat)) {
        return true;
    }

    // 2. Explicit wait-for-confirmation / approval gate before mutating code.
    const CONFIRMATION_GATES: &[&str] = &[
        "wait for confirmation",
        "wait for my confirmation",
        "wait for user confirmation",
        "wait for approval",
        "wait for my approval",
        "wait for user approval",
        "wait for permission",
        "wait for my permission",
        "wait for go-ahead",
        "wait for my go-ahead",
        "wait for sign-off",
        "await confirmation",
        "await my confirmation",
        "await approval",
        "await my approval",
        "pause for confirmation",
        "pause for approval",
        "stop for confirmation",
        "stop for approval",
        "stop and wait",
        "ask for confirmation before",
        "ask for my confirmation before",
        "ask for approval before",
        "ask for my approval before",
        "ask before editing",
        "ask before modifying",
        "ask before writing",
        "ask before patching",
        "ask before making",
        "confirm before editing",
        "confirm before modifying",
        "confirm before writing",
        "confirm before patching",
        "confirm before making",
        "confirm before applying",
        "confirm with me before",
        "get confirmation before",
        "get my confirmation before",
        "get approval before",
        "get my approval before",
        "until i confirm",
        "until i approve",
    ];
    if CONFIRMATION_GATES.iter().any(|pat| norm.contains(pat)) {
        return true;
    }

    // 3. Explicit agent-mode `read-only` / `read only` / `readonly` directives
    //    (carefully scoped so domain phrases like `fix the read-only input field` or
    //    `add a --read-only flag` do NOT false-positive).
    let ro_norm = norm
        .replace("read-only", "readonly")
        .replace("read only", "readonly");
    if ro_norm.contains("readonly") {
        let starts_readonly = ro_norm.starts_with("readonly ")
            || ro_norm.starts_with("readonly:")
            || ro_norm.starts_with("readonly,")
            || ro_norm.starts_with("readonly -")
            || ro_norm.contains("(readonly)");
        const RO_AGENT_PHRASES: &[&str] = &[
            "do a readonly",
            "run a readonly",
            "perform a readonly",
            "make a readonly",
            "start with a readonly",
            "stay in readonly",
            "keep it readonly",
            "keep this readonly",
            "operate in readonly",
            "work in readonly",
            "readonly scan",
            "readonly repo scan",
            "readonly repository scan",
            "readonly codebase scan",
            "readonly project scan",
            "readonly workspace scan",
            "readonly audit",
            "readonly repo audit",
            "readonly codebase audit",
            "readonly review",
            "readonly repo review",
            "readonly codebase review",
            "readonly pass",
            "readonly analysis",
            "readonly inspection",
            "readonly check",
            "readonly exploration",
            "readonly assessment",
            "readonly survey",
            "readonly overview",
            "readonly first",
        ];
        if starts_readonly || RO_AGENT_PHRASES.iter().any(|pat| ro_norm.contains(pat)) {
            return true;
        }
    }

    false
}

/// Returns `true` when a follow-up user message confirms or authorizes proceeding
/// with edits after a read-only turn.
pub fn detects_user_confirmation(prompt: &str) -> bool {
    if detects_readonly_directive(prompt) {
        return false;
    }
    let cleaned = strip_code_and_quotes(prompt);
    let norm = normalize_ws(&cleaned);
    let trimmed = norm.trim_matches(|c: char| c.is_ascii_punctuation() || c.is_whitespace());
    if trimmed.is_empty() {
        return false;
    }

    const EXACT_CONFIRMATIONS: &[&str] = &[
        "y",
        "yes",
        "yep",
        "yeah",
        "yup",
        "ok",
        "okay",
        "sure",
        "go",
        "go ahead",
        "proceed",
        "continue",
        "approved",
        "approve",
        "confirmed",
        "confirm",
        "lgtm",
        "looks good",
        "sounds good",
        "do it",
        "ship it",
        "lets do it",
        "let's do it",
        "make it so",
        "apply",
        "apply it",
        "apply them",
        "fix it",
        "fix them",
        "patch it",
        "implement it",
    ];
    if EXACT_CONFIRMATIONS.contains(&trimmed) {
        return true;
    }

    const ACTION_UNLOCK_PHRASES: &[&str] = &[
        "go ahead",
        "please proceed",
        "proceed with",
        "proceed and",
        "now apply",
        "please apply",
        "apply the ",
        "apply those",
        "apply these",
        "apply all",
        "apply your",
        "now implement",
        "please implement",
        "implement the ",
        "implement those",
        "implement these",
        "now fix",
        "please fix",
        "fix the ",
        "fix those",
        "fix these",
        "now write",
        "now edit",
        "now patch",
        "make the changes",
        "make those changes",
        "make the edits",
        "make the fixes",
        "write the code",
        "patch the files",
        "you can now ",
        "you may now ",
        "feel free to ",
    ];
    let has_unlock_phrase = ACTION_UNLOCK_PHRASES.iter().any(|pat| norm.contains(pat));

    // Reject standalone negations ("no", "not yet", "wait", "hold on", "stop") unless paired
    // with an explicit unlock phrase ("no other questions, go ahead and apply the fix").
    const NEGATION_PREFIXES: &[&str] = &[
        "no", "nope", "wait", "hold", "stop", "not yet", "cancel", "abort", "don't", "do not",
    ];
    let starts_negated = NEGATION_PREFIXES.iter().any(|neg| {
        trimmed == *neg
            || trimmed
                .strip_prefix(neg)
                .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with(','))
    });
    if starts_negated && !has_unlock_phrase {
        return false;
    }

    if has_unlock_phrase {
        return true;
    }

    // Leading affirmative + instruction: "yes, proceed", "ok, update llm_client.py", "approved - ship it"
    for lead in [
        "yes",
        "yep",
        "yeah",
        "ok",
        "okay",
        "sure",
        "approved",
        "confirmed",
        "lgtm",
        "looks good",
        "sounds good",
    ] {
        if let Some(rest) = trimmed.strip_prefix(lead)
            && rest.starts_with([' ', ',', '.', '-', ':', '!', ';'])
        {
            return true;
        }
    }

    false
}

/// Returns `true` when the assistant's final message explicitly pauses to wait for user
/// confirmation or approval before modifying code.
pub fn assistant_waits_for_confirmation(answer: &str) -> bool {
    let tail: String = answer
        .trim()
        .lines()
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" ");
    let norm = normalize_ws(&tail);
    if norm.is_empty() {
        return false;
    }
    const WAITING_MARKERS: &[&str] = &[
        "wait for your confirmation",
        "waiting for your confirmation",
        "awaiting your confirmation",
        "awaiting confirmation",
        "waiting for confirmation",
        "waiting for your approval",
        "awaiting your approval",
        "once you confirm",
        "once you approve",
        "before making any changes",
        "before writing any code",
        "before editing any files",
        "before patching",
        "before applying",
        "would you like me to proceed",
        "would you like me to apply",
        "would you like me to implement",
        "would you like me to make",
        "shall i proceed",
        "shall i apply",
        "shall i implement",
        "should i proceed",
        "should i apply",
        "confirm before i ",
        "please confirm if you",
        "let me know if you'd like me to proceed",
        "let me know if you want me to proceed",
    ];
    WAITING_MARKERS.iter().any(|m| norm.contains(m))
}

/// Checks whether a shell command (`sh`, `bash`, `pwsh`, `powershell`, `cmd`) mutates
/// workspace files, returning a short classification reason when mutating.
pub fn is_mutating_shell_command(cmd: &str) -> Option<&'static str> {
    let trimmed = cmd.trim();
    if trimmed.is_empty() {
        return None;
    }

    if has_file_output_redirection(trimmed) {
        return Some("shell file redirection");
    }

    let lower = normalize_ws(trimmed);

    // PowerShell file-writing / mutating cmdlets and .NET file APIs.
    const PS_MUTATING: &[&str] = &[
        "set-content",
        "add-content",
        "out-file",
        "new-item",
        "remove-item",
        "clear-content",
        "copy-item",
        "move-item",
        "rename-item",
        "tee-object",
        "io.file]::write",
        "io.file]::append",
        "io.file]::delete",
        "io.file]::copy",
        "io.file]::move",
    ];
    if PS_MUTATING.iter().any(|kw| lower.contains(kw)) {
        return Some("PowerShell file-mutating command");
    }

    // Inline script file writes (`python -c`, `node -e`, etc.).
    const INLINE_WRITE_MARKERS: &[&str] = &[
        ".write(",
        ".writelines(",
        ".write_text(",
        ".write_bytes(",
        "writefilesync(",
        "appendfilesync(",
        "unlinksync(",
        "shutil.copy",
        "shutil.move",
        "shutil.rmtree",
        "os.remove(",
        "os.unlink(",
        "os.replace(",
        "os.rename(",
    ];
    if INLINE_WRITE_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some("inline script file write");
    }
    if lower.contains("open(")
        && [
            ", 'w'", ", \"w\"", ", 'a'", ", \"a\"", ", 'x'", ", \"x\"", ", 'wb'", ", \"wb\"",
            "mode='w", "mode=\"w",
        ]
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some("inline open() write mode");
    }

    // Check segment-leading commands across `;`, `&&`, `||`, `|`, and newlines.
    for seg in trimmed.split([';', '\n', '|']) {
        for sub in seg.split("&&").flat_map(|s| s.split("||")) {
            let words: Vec<&str> = sub.split_whitespace().collect();
            let Some(&first_raw) = words.first() else {
                continue;
            };
            let first = first_raw
                .trim_matches(|c: char| matches!(c, '\'' | '"' | '('))
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(first_raw)
                .to_ascii_lowercase();
            let first = first.strip_suffix(".exe").unwrap_or(&first);

            if matches!(
                first,
                "rm" | "mv"
                    | "cp"
                    | "touch"
                    | "mkdir"
                    | "rmdir"
                    | "truncate"
                    | "tee"
                    | "dd"
                    | "install"
                    | "patch"
                    | "apply_patch"
                    | "del"
                    | "erase"
                    | "copy"
                    | "move"
                    | "ren"
                    | "rename"
                    | "cpi"
                    | "mi"
                    | "ri"
                    | "ni"
                    | "clc"
                    | "rni"
                    | "ac"
            ) {
                return Some("filesystem mutating command");
            }

            if first == "sed"
                && words[1..]
                    .iter()
                    .any(|w| w.starts_with("-i") || *w == "--in-place")
            {
                return Some("sed in-place edit");
            }
            if first == "perl" && words[1..].iter().any(|w| w.starts_with("-i") || w.starts_with("-pi")) {
                return Some("perl in-place edit");
            }

            if first == "git" {
                let git_sub = words
                    .iter()
                    .skip(1)
                    .find(|w| !w.starts_with('-'))
                    .map(|w| w.to_ascii_lowercase());
                if let Some(subcmd) = git_sub
                    && matches!(
                        subcmd.as_str(),
                        "add"
                            | "am"
                            | "apply"
                            | "checkout"
                            | "cherry-pick"
                            | "clean"
                            | "commit"
                            | "merge"
                            | "mv"
                            | "pull"
                            | "push"
                            | "rebase"
                            | "reset"
                            | "restore"
                            | "revert"
                            | "rm"
                            | "stash"
                            | "switch"
                    )
                {
                    return Some("mutating git command");
                }
            }
        }
    }

    None
}

fn has_file_output_redirection(cmd: &str) -> bool {
    let bytes = cmd.as_bytes();
    let mut in_single = false;
    let mut in_double = false;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if b == b'\\' && in_double {
            i += 2;
            continue;
        }
        if b == b'\'' && !in_double {
            in_single = !in_single;
            i += 1;
            continue;
        }
        if b == b'"' && !in_single {
            in_double = !in_double;
            i += 1;
            continue;
        }
        if !in_single && !in_double && b == b'>' {
            // Skip `->`, `=>`
            if i > 0 && matches!(bytes[i - 1], b'-' | b'=') {
                i += 1;
                continue;
            }
            // Advance past `>>`
            let after_idx = if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                i + 2
            } else {
                i + 1
            };
            // Skip `>=` or `>>=`
            if after_idx < bytes.len() && bytes[after_idx] == b'=' {
                i = after_idx + 1;
                continue;
            }
            let rest = cmd[after_idx..].trim_start();
            // Skip stream duplication `2>&1`, `>&2` and null sinks `>/dev/null`, `>$null`
            if rest.starts_with('&')
                || rest.starts_with("/dev/null")
                || rest.to_ascii_lowercase().starts_with("$null")
                || rest.to_ascii_lowercase().starts_with("nul")
            {
                i = after_idx;
                continue;
            }
            return true;
        }
        i += 1;
    }
    false
}

/// Checks whether a tool invocation (`ToolName` namespace + name + JSON/custom payload)
/// mutates workspace state and should be gated while [`ReadOnlyTurnGuard`] is active.
pub fn is_mutating_tool_call(
    namespace: Option<&str>,
    name: &str,
    payload_text: &str,
) -> Option<String> {
    let lower_name = name.to_ascii_lowercase();
    let short_name = lower_name
        .rsplit("__")
        .next()
        .unwrap_or(&lower_name)
        .rsplit('/')
        .next()
        .unwrap_or(&lower_name);

    // 1. Direct file-mutation tools.
    if matches!(
        short_name,
        "apply_patch"
            | "write_file"
            | "edit_file"
            | "create_file"
            | "delete_file"
            | "remove_file"
            | "move_file"
            | "copy_file"
            | "rename_file"
            | "multi_edit"
            | "write"
            | "edit"
            | "patch"
            | "refactor"
    ) {
        return Some(format!("mutating tool `{name}`"));
    }

    // 2. Shell / terminal execution tools.
    if matches!(
        short_name,
        "exec_command" | "shell" | "local_shell" | "bash" | "powershell"
    ) {
        if let Ok(json) = serde_json::from_str::<Value>(payload_text) {
            if let Some(cmd) = json
                .get("cmd")
                .or_else(|| json.get("command"))
                .and_then(Value::as_str)
                && let Some(reason) = is_mutating_shell_command(cmd)
            {
                return Some(format!("{name}: {reason}"));
            }
            if let Some(arr) = json.get("command").and_then(Value::as_array) {
                let joined = arr
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(" ");
                if let Some(reason) = is_mutating_shell_command(&joined) {
                    return Some(format!("{name}: {reason}"));
                }
            }
        } else if let Some(reason) = is_mutating_shell_command(payload_text) {
            return Some(format!("{name}: {reason}"));
        }
        return None;
    }

    if short_name == "write_stdin" {
        if let Ok(json) = serde_json::from_str::<Value>(payload_text)
            && let Some(chars) = json.get("chars").and_then(Value::as_str)
            && !chars.trim().is_empty()
            && let Some(reason) = is_mutating_shell_command(chars)
        {
            return Some(format!("write_stdin: {reason}"));
        }
        return None;
    }

    // 3. MCP unified tools (`hanzo/fs`, `hanzo/exec`, `hanzo/git`, etc.).
    let is_mcp = namespace.is_some() || lower_name.starts_with("mcp__");
    if is_mcp
        && let Ok(json) = serde_json::from_str::<Value>(payload_text)
    {
        let action = json
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase();
        if short_name == "fs"
            && matches!(
                action.as_str(),
                "write"
                    | "edit"
                    | "patch"
                    | "apply_patch"
                    | "append"
                    | "create"
                    | "mkdir"
                    | "delete"
                    | "remove"
                    | "rm"
                    | "move"
                    | "mv"
                    | "copy"
                    | "cp"
                    | "rename"
                    | "touch"
            )
        {
            return Some(format!("MCP fs(action='{action}')"));
        }
        if matches!(short_name, "exec" | "proc")
            && let Some(cmd) = json.get("command").and_then(Value::as_str)
            && let Some(reason) = is_mutating_shell_command(cmd)
        {
            return Some(format!("MCP {short_name}: {reason}"));
        }
        if short_name == "git"
            && matches!(
                action.as_str(),
                "commit"
                    | "add"
                    | "stage"
                    | "checkout"
                    | "restore"
                    | "reset"
                    | "clean"
                    | "stash"
                    | "merge"
                    | "rebase"
                    | "push"
                    | "pull"
            )
        {
            return Some(format!("MCP git(action='{action}')"));
        }
    }

    None
}

#[cfg(test)]
#[path = "readonly_guard_tests.rs"]
mod tests;
