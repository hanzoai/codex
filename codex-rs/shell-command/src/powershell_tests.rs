use pretty_assertions::assert_eq;

use super::ENCODED_EXEC_EPILOGUE;
use super::ENCODED_EXEC_PROLOGUE;
use super::UTF8_OUTPUT_PREFIX;
use super::decode_powershell_clixml;
use super::decode_powershell_utf16le_base64;
use super::encode_powershell_command_for_exec;
use super::encode_powershell_command_for_exec_with_options;
use super::encode_powershell_utf16le_base64;
use super::extract_powershell_command;
use super::extract_powershell_script;
use super::parse_powershell_command_into_plain_commands;
use super::parse_powershell_script_into_plain_commands;
use super::prefix_powershell_script_with_utf8;

#[test]
fn extracts_basic_powershell_command() {
    let cmd = vec![
        "powershell".to_string(),
        "-Command".to_string(),
        "Write-Host hi".to_string(),
    ];
    let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
    assert_eq!(script, "Write-Host hi");
}

#[test]
fn extracts_lowercase_flags() {
    let cmd = vec![
        "powershell".to_string(),
        "-nologo".to_string(),
        "-command".to_string(),
        "Write-Host hi".to_string(),
    ];
    let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
    assert_eq!(script, "Write-Host hi");
}

#[test]
fn extracts_full_path_powershell_command() {
    let command = if cfg!(windows) {
        "C:\\windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe".to_string()
    } else {
        "/usr/local/bin/powershell.exe".to_string()
    };
    let cmd = vec![command, "-Command".to_string(), "Write-Host hi".to_string()];
    let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
    assert_eq!(script, "Write-Host hi");
}

#[test]
fn extracts_with_noprofile_noninteractive_and_alias() {
    let cmd = vec![
        "pwsh".to_string(),
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-c".to_string(),
        "Get-ChildItem | Select-String foo".to_string(),
    ];
    let (_shell, script) = extract_powershell_command(&cmd).expect("extract");
    assert_eq!(script, "Get-ChildItem | Select-String foo");
}

#[test]
fn parses_powershell_command_chains_and_preserves_windows_paths() {
    assert_eq!(
        parse_powershell_script_into_plain_commands(r"echo safe && Remove-Item C:\important"),
        Some(vec![
            vec!["echo".to_string(), "safe".to_string()],
            vec!["Remove-Item".to_string(), r"C:\important".to_string()],
        ]),
    );
}

#[test]
fn prefixes_powershell_command_with_best_effort_utf8() {
    let cmd = vec![
        "powershell".to_string(),
        "-Command".to_string(),
        "Write-Host hi".to_string(),
    ];

    let prefixed = prefix_powershell_script_with_utf8(&cmd);

    assert_eq!(
        prefixed,
        vec![
            "powershell".to_string(),
            "-Command".to_string(),
            format!("{UTF8_OUTPUT_PREFIX}Write-Host hi"),
        ]
    );
}

#[test]
fn does_not_duplicate_utf8_prefix() {
    let cmd = vec![
        "powershell".to_string(),
        "-Command".to_string(),
        format!("{UTF8_OUTPUT_PREFIX}Write-Host hi"),
    ];

    assert_eq!(prefix_powershell_script_with_utf8(&cmd), cmd);
}

#[test]
fn encodes_multiline_python_llm_client_to_utf16le_encoded_command() {
    let python_payload = r#"@'
import os
import httpx

class LLMClient:
    """Multi-line docstring with "double quotes", 'single quotes', `backticks`, and $env:OPENAI_API_KEY."""
    def complete(self, prompt: str = "line1\nline2 $USER") -> str:
        headers = {"Authorization": f"Bearer {os.environ.get('API_KEY', '')}"}
        return f"{prompt} -> {headers['Authorization']}"
'@ | Set-Content -LiteralPath llm_client.py -Encoding utf8"#;

    let cmd = vec![
        "pwsh.exe".to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        python_payload.to_string(),
    ];

    let encoded_cmd = encode_powershell_command_for_exec(&cmd);
    assert_eq!(
        &encoded_cmd[..7],
        &[
            "pwsh.exe",
            "-NoProfile",
            "-NonInteractive",
            "-InputFormat",
            "Text",
            "-OutputFormat",
            "Text",
        ]
    );
    assert_eq!(encoded_cmd[7], "-EncodedCommand");

    // Verify UTF-16LE Base64 payload round-trips byte-for-byte with zero quote/newline loss.
    let decoded = decode_powershell_utf16le_base64(&encoded_cmd[8]).expect("valid utf16le b64");
    assert_eq!(
        decoded,
        format!("{ENCODED_EXEC_PROLOGUE}{python_payload}{ENCODED_EXEC_EPILOGUE}")
    );

    // Calling encode_powershell_command_for_exec again is idempotent.
    assert_eq!(encode_powershell_command_for_exec(&encoded_cmd), encoded_cmd);

    // extract_powershell_script recovers the exact unwrapped script body.
    let (shell, recovered) = extract_powershell_script(&encoded_cmd).expect("extract encoded");
    assert_eq!(shell, "pwsh.exe");
    assert_eq!(recovered.as_ref(), python_payload);
}

#[test]
fn encodes_tty_powershell_without_noninteractive_flag() {
    let cmd = vec![
        "powershell.exe".to_string(),
        "-NoLogo".to_string(),
        "-Command".to_string(),
        "Write-Output 'interactive ok'".to_string(),
    ];
    let encoded_cmd =
        encode_powershell_command_for_exec_with_options(&cmd, /*non_interactive*/ false);
    assert_eq!(
        &encoded_cmd[..4],
        &[
            "powershell.exe",
            "-NoLogo",
            "-NoProfile",
            "-EncodedCommand",
        ]
    );
    let decoded = decode_powershell_utf16le_base64(&encoded_cmd[4]).expect("decode");
    assert_eq!(
        decoded,
        format!("{ENCODED_EXEC_PROLOGUE}Write-Output 'interactive ok'")
    );
}

#[test]
fn utf16le_roundtrip_preserves_unicode_surrogates_and_backticks() {
    let tricky = "print('✓ 🚀 $HOME `n \"quoted\" \\\\server\\share')\r\nWrite-Output foo `";
    let b64 = encode_powershell_utf16le_base64(tricky);
    assert_eq!(
        decode_powershell_utf16le_base64(&b64).as_deref(),
        Some(tricky)
    );
}

#[test]
fn decodes_powershell_5_1_clixml_errors_and_drops_progress_noise() {
    let clixml = concat!(
        "stdout before\n",
        "#< CLIXML\r\n",
        r#"<Objs Version="1.1.0.1" xmlns="http://schemas.microsoft.com/powershell/2004/04">"#,
        r#"<Obj S="progress" RefId="0"><MS><AV>Preparing modules for first use.</AV></MS></Obj>"#,
        r#"<S S="Error">At line:1 char:12_x000D__x000A_</S>"#,
        r#"<S S="Error">+ node -v &amp;&amp; python --version_x000D__x000A_</S>"#,
        r#"<S S="Error">The token &apos;&amp;&amp;&apos; is not a valid statement separator._x000D__x000A_</S>"#,
        r#"</Objs>"#,
    );
    let decoded = decode_powershell_clixml(clixml);
    assert!(decoded.contains("stdout before\n"));
    assert!(decoded.contains("+ node -v && python --version\n"));
    assert!(decoded.contains("The token '&&' is not a valid statement separator.\n"));
    assert!(!decoded.contains("CLIXML"));
    assert!(!decoded.contains("Preparing modules"));
    assert!(!decoded.contains("_x000D_"));
}

#[test]
fn parses_plain_commands_from_encoded_powershell_command() {
    let cmd = encode_powershell_command_for_exec(&[
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "echo safe && Remove-Item C:\\important".to_string(),
    ]);
    assert_eq!(
        parse_powershell_command_into_plain_commands(&cmd),
        Some(vec![
            vec!["echo".to_string(), "safe".to_string()],
            vec!["Remove-Item".to_string(), r"C:\important".to_string()],
        ])
    );
}

#[cfg(windows)]
#[test]
fn parses_plain_powershell_commands() {
    let commands = parse_powershell_command_into_plain_commands(&[
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "echo hi".to_string(),
    ])
    .expect("parse");

    assert_eq!(commands, vec![vec!["echo".to_string(), "hi".to_string()]]);
}

#[cfg(windows)]
#[test]
fn parses_multiple_plain_powershell_commands() {
    let commands = parse_powershell_command_into_plain_commands(&[
        "powershell.exe".to_string(),
        "-NoProfile".to_string(),
        "-Command".to_string(),
        "Write-Output foo | Measure-Object".to_string(),
    ])
    .expect("parse");

    assert_eq!(
        commands,
        vec![
            vec!["Write-Output".to_string(), "foo".to_string()],
            vec!["Measure-Object".to_string()],
        ]
    );
}
