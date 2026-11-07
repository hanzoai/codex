use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn item(value: serde_json::Value) -> ResponseItem {
    serde_json::from_value(value).expect("a response item")
}

fn user(text: &str) -> ResponseItem {
    item(
        json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": text}]}),
    )
}

fn call(id: &str, cmd: &str) -> ResponseItem {
    item(json!({
        "type": "function_call",
        "name": "exec_command",
        "arguments": json!({"cmd": cmd}).to_string(),
        "call_id": id,
    }))
}

fn output(id: &str, chunk: &str, wall: &str, text: &str) -> ResponseItem {
    item(json!({
        "type": "function_call_output",
        "call_id": id,
        "output": format!("Chunk ID: {chunk}\nWall time: {wall} seconds\nProcess exited with code 0\nOutput:\n{text}"),
    }))
}

/// Grows `history` by `step` and checks it, as the turn loop does before each request.
fn step(
    repeat: &mut Repeat,
    history: &mut Vec<ResponseItem>,
    step: Vec<ResponseItem>,
) -> CodexResult<()> {
    history.extend(step);
    repeat.check(history.iter())
}

#[test]
fn the_same_call_answered_the_same_twice_fails_the_turn() {
    let mut repeat = Repeat::default();
    let mut history = vec![user("list the files")];
    repeat.check(history.iter()).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c1", "ls"), output("c1", "a1", "0.0010", "src\n")],
    )
    .unwrap();
    let err = step(
        &mut repeat,
        &mut history,
        vec![call("c2", "ls"), output("c2", "b2", "0.0042", "src\n")],
    )
    .unwrap_err();
    assert_eq!(
        err.to_string(),
        r#"unsupported operation: repeated identical tool-use cycle detected; refusing to continue after duplicate tool calls: {"type":"function_call","name":"exec_command","namespace":null,"arguments":"{\"cmd\":\"ls\"}"}"#
    );
}

#[test]
fn a_different_result_or_call_is_progress() {
    let mut repeat = Repeat::default();
    let mut history = vec![user("go")];
    repeat.check(history.iter()).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c1", "ls"), output("c1", "a", "0.1", "one\n")],
    )
    .unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c2", "ls"), output("c2", "b", "0.1", "two\n")],
    )
    .unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c3", "ls -a"), output("c3", "c", "0.1", "two\n")],
    )
    .unwrap();
}

#[test]
fn a_user_message_or_a_step_without_calls_starts_over() {
    let mut repeat = Repeat::default();
    let mut history = vec![user("go")];
    repeat.check(history.iter()).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c1", "ls"), output("c1", "a", "0.1", "x\n")],
    )
    .unwrap();
    step(&mut repeat, &mut history, vec![user("try again")]).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c2", "ls"), output("c2", "b", "0.1", "x\n")],
    )
    .unwrap();
    let reply = item(
        json!({"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}),
    );
    step(&mut repeat, &mut history, vec![reply]).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c3", "ls"), output("c3", "c", "0.1", "x\n")],
    )
    .unwrap();
}

#[test]
fn history_that_shrank_or_was_reset_is_not_compared() {
    let mut repeat = Repeat::default();
    let mut history = vec![user("go")];
    repeat.check(history.iter()).unwrap();
    step(
        &mut repeat,
        &mut history,
        vec![call("c1", "ls"), output("c1", "a", "0.1", "x\n")],
    )
    .unwrap();
    let mut compacted = vec![user("summary")];
    repeat.check(compacted.iter()).unwrap();
    step(
        &mut repeat,
        &mut compacted,
        vec![call("c2", "ls"), output("c2", "b", "0.1", "x\n")],
    )
    .unwrap();
    repeat.reset();
    step(
        &mut repeat,
        &mut compacted,
        vec![call("c3", "ls"), output("c3", "c", "0.1", "x\n")],
    )
    .unwrap();
}

#[test]
fn only_the_exec_header_is_made_steady() {
    assert_eq!(
        steady(
            "Chunk ID: 1a\nWall time: 0.5 seconds\nProcess exited with code 2\nOutput:\nWall time: kept\n"
        ),
        "Process exited with code 2\nOutput:\nWall time: kept\n"
    );
    assert_eq!(steady("plain text"), "plain text");
}
