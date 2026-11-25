use super::ReadOnlyTurnGuard;
use super::assistant_waits_for_confirmation;
use super::detects_readonly_directive;
use super::detects_user_confirmation;
use super::is_mutating_shell_command;
use super::is_mutating_tool_call;

#[test]
fn detects_explicit_readonly_and_wait_for_confirmation_directives() {
    for prompt in [
        "Do a read-only repo scan first and wait for confirmation before writing code.",
        "Please run a read-only codebase audit and summarize the issues.",
        "Read-only: inspect llm_client.py and explain why truncation happens.",
        "Scan the repository, do not modify any files yet.",
        "Audit the auth service without editing files and wait for my approval.",
        "Review the PR changes and ask for confirmation before patching anything.",
        "Inspect src/ (read-only) and report findings.",
    ] {
        assert!(
            detects_readonly_directive(prompt),
            "expected read-only directive for: {prompt:?}"
        );
    }
}

#[test]
fn zero_false_positives_on_normal_bug_fix_prompts() {
    for prompt in [
        "Fix the bug in llm_client.py and run pytest.",
        "Read llm_client.py first and then fix the timeout bug.",
        "Scan the repo for unused imports and remove them.",
        "Fix the read-only input field in SettingsForm.tsx.",
        "Add a --read-only CLI flag to clap args and write unit tests for it.",
        "Why does SQLite fail in read-only mode? Fix the connection string.",
        "> do a read-only repo scan first\nFix this bypass bug in router.rs.",
    ] {
        assert!(
            !detects_readonly_directive(prompt),
            "unexpected false positive for: {prompt:?}"
        );
    }
}

#[test]
fn guard_stays_locked_across_unconfirmed_turn_2_and_unlocks_on_user_confirmation() {
    let guard = ReadOnlyTurnGuard::new();
    assert!(!guard.is_active());

    // Turn 1: User requests read-only repo scan + confirmation before writing code.
    assert!(guard.on_user_turn(
        "Do a read-only repo scan first and wait for confirmation before writing code"
    ));
    assert!(guard.is_active());

    // Turn 2 (automatic continuation with empty user_input, or user asking a follow-up question):
    assert!(guard.on_user_turn(""));
    assert!(guard.on_user_turn("What about tests/test_client.py?"));
    assert!(guard.on_user_turn("No, wait — check config.py first"));
    assert!(guard.is_active());

    // Turn 3: User confirms!
    assert!(!guard.on_user_turn("Looks good, go ahead and apply the fixes!"));
    assert!(!guard.is_active());
}

#[test]
fn confirmation_variants_unlock_while_negations_and_new_readonly_stay_locked() {
    for confirm in [
        "yes",
        "Yes, proceed.",
        "go ahead",
        "approved",
        "LGTM",
        "apply the changes",
        "fix it",
        "Proceed with the patch",
        "No other questions, go ahead and implement the fix",
    ] {
        assert!(
            detects_user_confirmation(confirm),
            "expected confirmation for: {confirm:?}"
        );
    }

    for non_confirm in [
        "no",
        "not yet",
        "wait",
        "hold on, check another file",
        "do not edit anything yet",
        "yes, do another read-only check on module B first",
    ] {
        assert!(
            !detects_user_confirmation(non_confirm),
            "expected non-confirmation for: {non_confirm:?}"
        );
    }
}

#[test]
fn blocks_mutating_tools_and_allows_readonly_inspection_tools() {
    // apply_patch is always blocked during read-only guard.
    assert!(
        is_mutating_tool_call(
            None,
            "apply_patch",
            "*** Begin Patch\n*** Update File: llm_client.py\n*** End Patch"
        )
        .is_some()
    );

    // Mutating shell commands are blocked.
    for bad_cmd in [
        r#"Set-Content -Path llm_client.py -Value "x""#,
        r#""hello" | Out-File llm_client.py"#,
        "echo broken > llm_client.py",
        "cat llm_client.py.bak >> llm_client.py",
        "Copy-Item llm_client.py.bak llm_client.py",
        "sed -i 's/foo/bar/' llm_client.py",
        "python -c \"open('llm_client.py', 'w').write('x')\"",
        "python -c \"from pathlib import Path; Path('llm_client.py').write_text('x')\"",
        "git checkout -- llm_client.py",
        "rm -f llm_client.py",
    ] {
        assert!(
            is_mutating_shell_command(bad_cmd).is_some(),
            "expected mutating classification for: {bad_cmd:?}"
        );
        let payload = serde_json::json!({ "cmd": bad_cmd }).to_string();
        assert!(
            is_mutating_tool_call(None, "exec_command", &payload).is_some(),
            "expected exec_command block for: {bad_cmd:?}"
        );
    }

    // Read-only shell commands are allowed!
    for safe_cmd in [
        "Get-ChildItem -Recurse -Filter *.py",
        "Get-Content -LiteralPath llm_client.py",
        "Select-String -Path *.py -Pattern 'class LLMClient' 2>$null",
        "rg -n 'LLMClient' src/ 2>/dev/null",
        "git status -sb && git diff --stat",
        "git log -n 5 --oneline",
        "python -c \"import ast; ast.parse(open('llm_client.py').read()); print('ok')\"",
        "ls -la > /dev/null 2>&1",
    ] {
        assert_eq!(
            is_mutating_shell_command(safe_cmd),
            None,
            "expected safe read-only classification for: {safe_cmd:?}"
        );
        let payload = serde_json::json!({ "cmd": safe_cmd }).to_string();
        assert_eq!(
            is_mutating_tool_call(None, "exec_command", &payload),
            None
        );
    }

    // MCP `fs` read vs write
    assert_eq!(
        is_mutating_tool_call(
            Some("mcp__hanzo"),
            "fs",
            r#"{"action":"read","path":"llm_client.py"}"#
        ),
        None
    );
    assert!(
        is_mutating_tool_call(
            Some("mcp__hanzo"),
            "fs",
            r#"{"action":"write","path":"llm_client.py","content":"x"}"#
        )
        .is_some()
    );
}

#[test]
fn detects_when_assistant_stops_to_wait_for_confirmation() {
    assert!(assistant_waits_for_confirmation(
        "Here are the 3 bugs found in the scan.\n\nWould you like me to proceed with applying these fixes?"
    ));
    assert!(assistant_waits_for_confirmation(
        "Scan complete. Waiting for your confirmation before making any changes."
    ));
    assert!(!assistant_waits_for_confirmation(
        "Now I have read the config. Let me check the tests:"
    ));
}
