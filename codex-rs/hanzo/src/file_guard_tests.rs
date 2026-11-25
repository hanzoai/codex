use std::fs;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use super::FileTruncationGuard;
use super::detect_backup_restore_source;

fn make_temp_workspace(label: &str) -> std::path::PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "hanzo-file-guard-{label}-{}-{nanos}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("create temp workspace");
    dir
}

#[test]
fn auto_restores_non_empty_python_file_wiped_to_zero_bytes() {
    let dir = make_temp_workspace("zero-byte-restore");
    let target = dir.join("llm_client.py");
    let original = b"class LLMClient:\n    def chat(self, prompt: str) -> str:\n        return prompt\n";
    fs::write(&target, original).expect("write llm_client.py");

    let guard = FileTruncationGuard::new();
    let cmd = r#"Set-Content -Path llm_client.py -Value """broken multiline quote"#;
    let snapshot = guard
        .pre_shell_command(cmd, Some(&dir))
        .expect("pre check should allow initial command");

    // Simulate PowerShell truncating `llm_client.py` to 0 bytes before failing on quote syntax.
    fs::write(&target, b"").expect("truncate to 0 bytes");
    assert_eq!(fs::metadata(&target).unwrap().len(), 0);

    let warning = guard
        .post_shell_command(cmd, Some(1), snapshot)
        .expect("must emit truncation restore warning");

    assert!(warning.contains("[FILE-TRUNCATION GUARD]"));
    assert!(warning.contains("llm_client.py"));
    assert!(warning.contains("AUTOMATICALLY RESTORED"));
    assert_eq!(fs::read(&target).expect("restored bytes"), original);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn preserves_legitimate_non_empty_edits_and_new_empty_files() {
    let dir = make_temp_workspace("legit-edit");
    let target = dir.join("llm_client.py");
    fs::write(&target, b"print('v1')\n").expect("write v1");

    let guard = FileTruncationGuard::new();
    let cmd = "python update.py";
    let snapshot = guard.pre_shell_command(cmd, Some(&dir)).expect("pre check");

    fs::write(&target, b"print('v2 updated')\n").expect("write v2");
    let new_empty = dir.join("created_empty.py");
    fs::write(&new_empty, b"").expect("create new empty");

    assert!(guard.post_shell_command(cmd, Some(0), snapshot).is_none());
    assert_eq!(fs::read(&target).unwrap(), b"print('v2 updated')\n");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn immediately_blocks_copying_from_zero_byte_bak_file() {
    let dir = make_temp_workspace("empty-bak");
    let target = dir.join("llm_client.py");
    let bak = dir.join("llm_client.py.bak");
    fs::write(&target, b"class LLMClient: pass\n").expect("write target");
    fs::write(&bak, b"").expect("write 0-byte bak");

    let guard = FileTruncationGuard::new();
    for restore_cmd in [
        "Copy-Item llm_client.py.bak llm_client.py",
        "cp llm_client.py.bak llm_client.py",
        "Get-Content llm_client.py.bak | Set-Content llm_client.py",
        "cat llm_client.py.bak > llm_client.py",
    ] {
        let err = guard
            .pre_shell_command(restore_cmd, Some(&dir))
            .expect_err("must block 0-byte .bak restore");
        assert!(err.contains("CIRCUIT BREAKER"), "cmd={restore_cmd}: {err}");
        assert!(err.contains("0 bytes"), "cmd={restore_cmd}: {err}");
    }

    // Creating a backup FROM non-empty target TO .bak is NOT blocked!
    assert!(
        guard
            .pre_shell_command("Copy-Item llm_client.py llm_client.py.bak", Some(&dir))
            .is_ok()
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn trips_circuit_breaker_on_repeated_missing_or_unchanged_bak_restore_loop() {
    let dir = make_temp_workspace("bak-loop");
    let target = dir.join("llm_client.py");
    fs::write(&target, b"class LLMClient: pass\n").expect("write target");

    let guard = FileTruncationGuard::new();
    let cmd = "Copy-Item -LiteralPath llm_client.py.bak -Destination llm_client.py";

    // Attempt 1 against a missing `.bak` runs and fails with exit code 1.
    let snap1 = guard
        .pre_shell_command(cmd, Some(&dir))
        .expect("first missing bak check");
    let _ = guard.post_shell_command(cmd, Some(1), snap1);

    // Attempt 2 immediately trips the circuit breaker before spawning another shell!
    let err = guard
        .pre_shell_command(cmd, Some(&dir))
        .expect_err("second failed bak restore must trip circuit breaker");
    assert!(err.contains("CIRCUIT BREAKER"));
    assert!(err.contains("apply_patch"));

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn distinguishes_backup_restore_from_backup_creation_and_inspection() {
    assert_eq!(
        detect_backup_restore_source("Copy-Item llm_client.py.bak llm_client.py").as_deref(),
        Some("llm_client.py.bak")
    );
    assert_eq!(
        detect_backup_restore_source("Copy-Item -Path src\\app.py.bak -Destination src\\app.py")
            .as_deref(),
        Some("src\\app.py.bak")
    );
    assert_eq!(
        detect_backup_restore_source("Copy-Item llm_client.py llm_client.py.bak"),
        None
    );
    assert_eq!(
        detect_backup_restore_source("Remove-Item llm_client.py.bak"),
        None
    );
    assert_eq!(detect_backup_restore_source("Get-ChildItem *.bak"), None);
}
