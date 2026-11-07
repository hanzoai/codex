//! Hanzo: a turn whose model makes the same tool calls twice in a row and is answered the
//! same both times ends there, failing with [`MARK`] and the calls.
//!
//! A step is what the model asked for in one sampling request and what its tools answered:
//! the items one step appended to history, read when the next step begins. Two steps in a
//! row with the same calls (name, namespace and arguments; never the call id) and the same
//! results fail the turn before a third request is sent. A result is compared without the
//! exec envelope's `Chunk ID` and `Wall time`, which differ on every run of one command.
//! A step that made no call, a user message between steps, or history that shrank under
//! compaction starts the comparison over.

use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::models::FunctionCallOutputBody;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use serde::Serialize;

/// What the failure says ahead of the repeated calls, each one JSON. Callers read the
/// calls back from after it.
pub(crate) const MARK: &str =
    "repeated identical tool-use cycle detected; refusing to continue after duplicate tool calls:";

/// One tool call as the model made it, in the shape the failure names it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Call {
    FunctionCall {
        name: String,
        namespace: Option<String>,
        arguments: String,
    },
    CustomToolCall {
        name: String,
        namespace: Option<String>,
        input: String,
    },
    LocalShellCall {
        action: serde_json::Value,
    },
    ToolSearchCall {
        execution: String,
        arguments: serde_json::Value,
    },
}

/// One step's calls and what its tools answered.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Step {
    calls: Vec<Call>,
    results: Vec<String>,
}

/// The turn's view of its steps: how much history the last step began with, and that
/// step's calls and results.
#[derive(Debug, Default)]
pub(crate) struct Repeat {
    seen: Option<usize>,
    previous: Option<Step>,
}

impl Repeat {
    /// Called with the history a step is about to send. Fails when the step that just ended
    /// repeated the one before it.
    pub(crate) fn check<'a>(
        &mut self,
        history: impl ExactSizeIterator<Item = &'a ResponseItem>,
    ) -> CodexResult<()> {
        let total = history.len();
        let start = self.seen.replace(total);
        let Some(start) = start.filter(|start| *start <= total) else {
            self.previous = None;
            return Ok(());
        };
        let current = Step::read(history.skip(start));
        if current.is_some() && current == self.previous {
            let calls = current
                .iter()
                .flat_map(|step| &step.calls)
                .map(|call| serde_json::to_string(call).unwrap_or_default())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(CodexErr::UnsupportedOperation(format!("{MARK} {calls}")));
        }
        self.previous = current;
        Ok(())
    }

    /// History was rewritten (compaction): what came before it is no longer a step.
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }
}

impl Step {
    fn read<'a>(items: impl Iterator<Item = &'a ResponseItem>) -> Option<Self> {
        let mut calls = Vec::new();
        let mut results = Vec::new();
        for item in items {
            match item {
                ResponseItem::Message { role, .. } if role == "user" => return None,
                ResponseItem::FunctionCall {
                    name,
                    namespace,
                    arguments,
                    ..
                } => calls.push(Call::FunctionCall {
                    name: name.clone(),
                    namespace: namespace.clone(),
                    arguments: arguments.clone(),
                }),
                ResponseItem::CustomToolCall {
                    name,
                    namespace,
                    input,
                    ..
                } => calls.push(Call::CustomToolCall {
                    name: name.clone(),
                    namespace: namespace.clone(),
                    input: input.clone(),
                }),
                ResponseItem::LocalShellCall { action, .. } => calls.push(Call::LocalShellCall {
                    action: serde_json::to_value(action).unwrap_or_default(),
                }),
                ResponseItem::ToolSearchCall {
                    execution,
                    arguments,
                    ..
                } => calls.push(Call::ToolSearchCall {
                    execution: execution.clone(),
                    arguments: arguments.clone(),
                }),
                ResponseItem::FunctionCallOutput { output, .. }
                | ResponseItem::CustomToolCallOutput { output, .. } => results.push(result(output)),
                ResponseItem::ToolSearchOutput { status, tools, .. } => results
                    .push(serde_json::json!({ "status": status, "tools": tools }).to_string()),
                _ => {}
            }
        }
        (!calls.is_empty() && !results.is_empty()).then_some(Self { calls, results })
    }
}

/// A tool's answer without the fields that change on every run of the same command.
fn result(output: &FunctionCallOutputPayload) -> String {
    let body = match &output.body {
        FunctionCallOutputBody::Text(text) => FunctionCallOutputBody::Text(steady(text)),
        other => other.clone(),
    };
    serde_json::json!({ "body": body, "success": output.success }).to_string()
}

/// `text` with the exec envelope's `Chunk ID` and `Wall time` lines dropped. Only the
/// header, which ends at the `Output:` line, is read that way; the output is kept whole.
fn steady(text: &str) -> String {
    let Some((header, output)) = text.split_once("Output:") else {
        return text.to_string();
    };
    let header = header
        .lines()
        .filter(|line| !line.starts_with("Chunk ID: ") && !line.starts_with("Wall time: "))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{header}\nOutput:{output}")
}

#[cfg(test)]
#[path = "cycle_tests.rs"]
mod tests;
