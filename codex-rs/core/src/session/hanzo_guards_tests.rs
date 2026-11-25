use std::fs;

use codex_protocol::user_input::UserInput;
use codex_tools::ToolName;
use pretty_assertions::assert_eq;

use super::check_tool_allowed_in_turn;
use super::file_guard;
use super::is_readonly_guard_active;
use super::readonly_reminder_item;
use super::update_readonly_guard_from_user_input;
use crate::function_tool::FunctionCallError;
use crate::session::tests::make_session_and_context;
use crate::tools::context::ToolPayload;

fn text_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

#[tokio::test]
async fn readonly_turn_guard_blocks_turn_2_mutations_until_explicit_user_confirmation() {
    let (session, _ctx) = make_session_and_context().await;
    assert!(!is_readonly_guard_active(&session));

    // Turn 1: User asks for read-only repo scan first and confirmation before writing code.
    let active = update_readonly_guard_from_user_input(
        &session,
        &text_input("Do a read-only repo scan first and wait for confirmation before writing code"),
    );
    assert!(active);
    assert!(is_readonly_guard_active(&session));

    // Developer reminder item is generated cleanly.
    let reminder = format!("{:?}", readonly_reminder_item());
    assert!(reminder.contains("READ-ONLY TURN GUARD ACTIVE"));

    // Read-only exec_command is allowed on Turn 1.
    let ro_exec = ToolPayload::Function {
        arguments: serde_json::json!({
            "cmd": "Get-ChildItem -Recurse -Filter *.py"
        })
        .to_string(),
    };
    assert!(
        check_tool_allowed_in_turn(&session, &ToolName::plain("exec_command"), &ro_exec).is_ok()
    );

    // Turn 2 (automatic continuation with empty UserInput): guard stays active and blocks patching!
    assert!(update_readonly_guard_from_user_input(&session, &[]));
    let patch_payload = ToolPayload::Custom {
        input: "*** Begin Patch\n*** Update File: llm_client.py\n@@\n-old\n+new\n*** End Patch"
            .to_string(),
    };
    let err = check_tool_allowed_in_turn(&session, &ToolName::plain("apply_patch"), &patch_payload)
        .expect_err("apply_patch must be blocked on unconfirmed turn 2");
    match err {
        FunctionCallError::RespondToModel(msg) => {
            assert!(msg.contains("Blocked: user requested a read-only scan"));
        }
        other => panic!("expected RespondToModel, got {other:?}"),
    }

    let mutating_ps = ToolPayload::Function {
        arguments: serde_json::json!({
            "cmd": "Set-Content -Path llm_client.py -Value 'patched'"
        })
        .to_string(),
    };
    assert!(
        check_tool_allowed_in_turn(&session, &ToolName::plain("exec_command"), &mutating_ps)
            .is_err()
    );

    // Turn 3: User confirms! Guard unlocks and allows apply_patch + mutating commands.
    let active_after_confirm = update_readonly_guard_from_user_input(
        &session,
        &text_input("Approved — go ahead and apply the fix."),
    );
    assert!(!active_after_confirm);
    assert!(!is_readonly_guard_active(&session));
    assert!(
        check_tool_allowed_in_turn(&session, &ToolName::plain("apply_patch"), &patch_payload)
            .is_ok()
    );
}

#[tokio::test]
async fn session_file_guard_restores_zero_byte_truncation_and_halts_bak_loop() {
    let (session, _ctx) = make_session_and_context().await;
    let dir = tempfile::tempdir().expect("tempdir");
    let llm_py = dir.path().join("llm_client.py");
    let original = b"import httpx\n\nclass LLMClient:\n    pass\n";
    fs::write(&llm_py, original).expect("write llm_client.py");

    let guard = file_guard(&session);
    let cmd = "Set-Content llm_client.py \"broken";
    let snap = guard
        .pre_shell_command(cmd, Some(dir.path()))
        .expect("pre check");

    // Simulate shell truncating llm_client.py to 0 bytes before failing.
    fs::write(&llm_py, b"").expect("truncate");
    let warning = guard
        .post_shell_command(cmd, Some(1), snap)
        .expect("restore warning");
    assert!(warning.contains("AUTOMATICALLY RESTORED"));
    assert_eq!(fs::read(&llm_py).expect("read restored"), original);

    // Empty .bak file copy is blocked immediately by the same session's guard.
    let empty_bak = dir.path().join("llm_client.py.bak");
    fs::write(&empty_bak, b"").expect("write empty bak");
    let err = guard
        .pre_shell_command("Copy-Item llm_client.py.bak llm_client.py", Some(dir.path()))
        .expect_err("empty bak must be blocked");
    assert!(err.contains("CIRCUIT BREAKER"));
}
