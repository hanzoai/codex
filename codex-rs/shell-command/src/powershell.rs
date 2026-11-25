use std::borrow::Cow;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use codex_utils_absolute_path::AbsolutePathBuf;
use once_cell::sync::Lazy;
use regex::Regex;

use crate::command_safety::try_parse_powershell_commands;
use crate::shell_detect::ShellType;
use crate::shell_detect::detect_shell_type;

const POWERSHELL_SWITCH_FLAGS: &[&str] = &["-nologo", "-noprofile", "-noninteractive", "-sta", "-mta"];
const POWERSHELL_VALUE_FLAGS: &[&str] = &[
    "-inputformat",
    "-if",
    "-outputformat",
    "-of",
    "-executionpolicy",
    "-ep",
];

/// Prefixed command for powershell shell calls to request UTF-8 console output.
pub const UTF8_OUTPUT_PREFIX: &str =
    "try { [Console]::OutputEncoding=[System.Text.Encoding]::UTF8 } catch {}\n";

/// Prologue prepended inside `-EncodedCommand` scripts so both pipeline `$OutputEncoding`
/// and `[Console]::OutputEncoding` emit BOM-less UTF-8 without progress/ANSI noise.
pub const ENCODED_EXEC_PROLOGUE: &str = concat!(
    "$ProgressPreference = 'SilentlyContinue'\n",
    "if ($PSStyle) { $PSStyle.OutputRendering = 'PlainText' }\n",
    "try { $OutputEncoding = [Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false) } catch {}\n",
);

/// Epilogue appended inside non-interactive `-EncodedCommand` scripts so native process
/// exit codes (`$LASTEXITCODE`) and cmdlet failures (`$?`) propagate accurately, with a
/// blank line guarding against a trailing backtick on the user's final line.
pub const ENCODED_EXEC_EPILOGUE: &str = concat!(
    "\n\n",
    "$__ok = $?; $__code = $LASTEXITCODE\n",
    "if (-not $__ok) { if ($__code) { exit $__code } else { exit 1 } }\n",
    "exit 0\n",
);

pub fn prefix_powershell_script_with_utf8(command: &[String]) -> Vec<String> {
    let Some((_, script)) = extract_powershell_command(command) else {
        return command.to_vec();
    };

    let trimmed = script.trim_start();
    let script = if trimmed.starts_with(UTF8_OUTPUT_PREFIX) {
        script.to_string()
    } else {
        format!("{UTF8_OUTPUT_PREFIX}{script}")
    };

    let mut command: Vec<String> = command[..(command.len() - 1)]
        .iter()
        .map(std::string::ToString::to_string)
        .collect();
    command.push(script);
    command
}

/// Encodes a PowerShell script string as UTF-16LE Base64 for `-EncodedCommand`.
pub fn encode_powershell_utf16le_base64(script: &str) -> String {
    let mut utf16le = Vec::with_capacity(script.len() * 2);
    for unit in script.encode_utf16() {
        utf16le.extend_from_slice(&unit.to_le_bytes());
    }
    BASE64_STANDARD.encode(utf16le)
}

/// Decodes a UTF-16LE Base64 `-EncodedCommand` payload back into UTF-8.
pub fn decode_powershell_utf16le_base64(encoded: &str) -> Option<String> {
    let bytes = BASE64_STANDARD.decode(encoded.trim()).ok()?;
    if bytes.len() % 2 != 0 {
        return None;
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();
    String::from_utf16(&units).ok()
}

fn strip_encoded_wrapper(script: &str) -> &str {
    let without_prologue = script
        .strip_prefix(ENCODED_EXEC_PROLOGUE)
        .or_else(|| script.strip_prefix(UTF8_OUTPUT_PREFIX))
        .unwrap_or(script);
    without_prologue
        .strip_suffix(ENCODED_EXEC_EPILOGUE)
        .unwrap_or(without_prologue)
}

/// Wraps a raw PowerShell script with BOM-less UTF-8 encoding setup and optional
/// exit-code preservation for `-EncodedCommand` execution.
pub fn wrap_powershell_script_for_encoded_exec(script: &str, preserve_exit_code: bool) -> String {
    let body = strip_encoded_wrapper(script);
    if preserve_exit_code {
        format!("{ENCODED_EXEC_PROLOGUE}{body}{ENCODED_EXEC_EPILOGUE}")
    } else {
        format!("{ENCODED_EXEC_PROLOGUE}{body}")
    }
}

/// Rewrites a PowerShell `-Command <raw_script>` argv into a quote/newline-safe
/// `-NoProfile -NonInteractive -InputFormat Text -OutputFormat Text -EncodedCommand <UTF-16LE-Base64>`
/// invocation for `CreateProcessW`, while leaving already-encoded or non-PowerShell
/// commands untouched.
pub fn encode_powershell_command_for_exec(command: &[String]) -> Vec<String> {
    encode_powershell_command_for_exec_with_options(command, /*non_interactive*/ true)
}

/// Variant of [`encode_powershell_command_for_exec`] that omits `-NonInteractive` and
/// `-InputFormat Text` when launching an interactive `tty: true` PTY session.
pub fn encode_powershell_command_for_exec_with_options(
    command: &[String],
    non_interactive: bool,
) -> Vec<String> {
    if has_encoded_command_flag(command) {
        return command.to_vec();
    }
    let Some((shell, script)) = extract_powershell_command(command) else {
        return command.to_vec();
    };

    let has_nologo = command[1..]
        .iter()
        .take_while(|arg| !arg.eq_ignore_ascii_case("-Command") && !arg.eq_ignore_ascii_case("-c"))
        .any(|arg| arg.eq_ignore_ascii_case("-NoLogo"));

    let wrapped = wrap_powershell_script_for_encoded_exec(script, non_interactive);
    let encoded = encode_powershell_utf16le_base64(&wrapped);

    let mut args = Vec::with_capacity(9);
    args.push(shell.to_string());
    if has_nologo {
        args.push("-NoLogo".to_string());
    }
    args.push("-NoProfile".to_string());
    if non_interactive {
        args.push("-NonInteractive".to_string());
        args.push("-InputFormat".to_string());
        args.push("Text".to_string());
        args.push("-OutputFormat".to_string());
        args.push("Text".to_string());
    }
    args.push("-EncodedCommand".to_string());
    args.push(encoded);
    args
}

fn has_encoded_command_flag(command: &[String]) -> bool {
    if command.len() < 3 {
        return false;
    }
    if !matches!(
        detect_shell_type(PathBuf::from(&command[0])),
        Some(ShellType::PowerShell)
    ) {
        return false;
    }
    let mut i = 1usize;
    while i + 1 < command.len() {
        let lower = command[i].to_ascii_lowercase();
        if matches!(lower.as_str(), "-encodedcommand" | "-ec") {
            return i + 2 == command.len();
        }
        if POWERSHELL_VALUE_FLAGS.contains(&lower.as_str()) {
            i += 2;
            continue;
        }
        if POWERSHELL_SWITCH_FLAGS.contains(&lower.as_str()) {
            i += 1;
            continue;
        }
        return false;
    }
    false
}

/// Extract the PowerShell script body from an invocation such as:
///
/// - `["pwsh", "-NoProfile", "-Command", "Get-ChildItem -Recurse | Select-String foo"]`
/// - `["powershell.exe", "-Command", "Write-Host hi"]`
/// - `["powershell", "-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "...script..."]`
///
/// Returns `(shell, script)` when the first arg is a PowerShell executable and a
/// `-Command` (or `-c`) flag is present as the final flag followed by a script string.
pub fn extract_powershell_command(command: &[String]) -> Option<(&str, &str)> {
    if command.len() < 3 {
        return None;
    }

    let shell = &command[0];
    if !matches!(
        detect_shell_type(PathBuf::from(shell)),
        Some(ShellType::PowerShell)
    ) {
        return None;
    }

    let mut i = 1usize;
    while i + 1 < command.len() {
        let flag = &command[i];
        let lower = flag.to_ascii_lowercase();
        if lower == "-command" || lower == "-c" {
            return (i + 2 == command.len()).then_some((shell.as_str(), command[i + 1].as_str()));
        }
        if POWERSHELL_VALUE_FLAGS.contains(&lower.as_str()) {
            if i + 2 >= command.len() {
                return None;
            }
            i += 2;
            continue;
        }
        if !POWERSHELL_SWITCH_FLAGS.contains(&lower.as_str()) {
            return None;
        }
        i += 1;
    }
    None
}

/// Extracts the PowerShell script from either `-Command` or `-EncodedCommand` invocations,
/// stripping the internal UTF-8/exit-code wrapper if present.
pub fn extract_powershell_script(command: &[String]) -> Option<(&str, Cow<'_, str>)> {
    if let Some((shell, script)) = extract_powershell_command(command) {
        return Some((shell, Cow::Borrowed(strip_encoded_wrapper(script))));
    }
    if has_encoded_command_flag(command) {
        let decoded = decode_powershell_utf16le_base64(command.last()?)?;
        let stripped = strip_encoded_wrapper(&decoded).to_string();
        return Some((command[0].as_str(), Cow::Owned(stripped)));
    }
    None
}

static CLIXML_BLOCK_RE: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r#"(?s)#< CLIXML\r?\n<Objs Version="[^"]*" xmlns="http://schemas\.microsoft\.com/powershell/2004/04">(.*?)</Objs>"#,
    )
    .expect("valid CLIXML block regex")
});
static CLIXML_STRING_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?s)<S S="([^"]*)">(.*?)</S>"#).expect("valid CLIXML string regex"));

/// Decodes Windows PowerShell 5.1 `#< CLIXML` serialized stderr blocks back into readable
/// UTF-8 plain text. Non-CLIXML output is returned unchanged without allocation overhead.
pub fn decode_powershell_clixml(output: &str) -> String {
    if !output.contains("#< CLIXML") {
        return output.to_string();
    }
    CLIXML_BLOCK_RE
        .replace_all(output, |caps: &regex::Captures<'_>| {
            let mut decoded = String::new();
            for item in CLIXML_STRING_RE.captures_iter(&caps[1]) {
                let stream = &item[1];
                if stream.eq_ignore_ascii_case("progress") {
                    continue;
                }
                let unescaped_xml = unescape_xml_entities(&item[2]);
                decoded.push_str(&unescape_clixml_hex(&unescaped_xml));
            }
            decoded.replace("\r\n", "\n")
        })
        .into_owned()
}

fn unescape_xml_entities(input: &str) -> String {
    input
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn unescape_clixml_hex(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut utf16_run: Vec<u16> = Vec::new();
    let flush_utf16 = |buf: &mut Vec<u16>, dest: &mut String| {
        if !buf.is_empty() {
            dest.push_str(&String::from_utf16_lossy(buf));
            buf.clear();
        }
    };
    let mut i = 0usize;
    while i < bytes.len() {
        if i + 7 <= bytes.len()
            && bytes[i] == b'_'
            && (bytes[i + 1] == b'x' || bytes[i + 1] == b'X')
            && bytes[i + 6] == b'_'
            && let Ok(hex_str) = std::str::from_utf8(&bytes[i + 2..i + 6])
            && let Ok(unit) = u16::from_str_radix(hex_str, 16)
        {
            utf16_run.push(unit);
            i += 7;
            continue;
        }
        flush_utf16(&mut utf16_run, &mut out);
        let ch = input[i..].chars().next().expect("valid char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    flush_utf16(&mut utf16_run, &mut out);
    out
}

/// Parse the script body from a top-level PowerShell wrapper into argv-like commands.
pub fn parse_powershell_command_into_plain_commands(
    command: &[String],
) -> Option<Vec<Vec<String>>> {
    let (_, script) = extract_powershell_script(command)?;
    try_parse_powershell_commands(&script)
}

/// Parse literal PowerShell commands without starting a PowerShell executable.
pub fn parse_powershell_script_into_plain_commands(script: &str) -> Option<Vec<Vec<String>>> {
    try_parse_powershell_commands(script)
}

/// Attempts to find a `powershell.exe` executable on the system.
pub fn try_find_powershell_executable_blocking() -> Option<AbsolutePathBuf> {
    try_find_powershellish_executable_in_path(&["powershell.exe"])
}

/// Attempts to find a `pwsh.exe` executable on the system.
pub fn try_find_pwsh_executable_blocking() -> Option<AbsolutePathBuf> {
    if let Some(ps_home) = std::process::Command::new("cmd")
        .args(["/C", "pwsh", "-NoProfile", "-Command", "$PSHOME"])
        .output()
        .ok()
        .and_then(|out| {
            if !out.status.success() {
                return None;
            }
            let stdout = String::from_utf8_lossy(&out.stdout);
            let trimmed = stdout.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        })
    {
        let candidate = AbsolutePathBuf::resolve_path_against_base("pwsh.exe", &ps_home);

        if is_powershellish_executable_available(candidate.as_path()) {
            return Some(candidate);
        }
    }

    try_find_powershellish_executable_in_path(&["pwsh.exe"])
}

fn try_find_powershellish_executable_in_path(candidates: &[&str]) -> Option<AbsolutePathBuf> {
    for candidate in candidates {
        let Ok(resolved_path) = which::which(candidate) else {
            continue;
        };

        if !is_powershellish_executable_available(&resolved_path) {
            continue;
        }

        let Ok(abs_path) = AbsolutePathBuf::from_absolute_path(resolved_path) else {
            continue;
        };

        return Some(abs_path);
    }

    None
}

fn is_powershellish_executable_available(powershell_or_pwsh_exe: &std::path::Path) -> bool {
    std::process::Command::new(powershell_or_pwsh_exe)
        .args(["-NoLogo", "-NoProfile", "-Command", "Write-Output ok"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "powershell_tests.rs"]
mod tests;
