//! Zero-byte file-truncation protection and `.bak` restore loop circuit breaker.
//!
//! On Windows PowerShell (and shell pipelines generally), malformed quotes, newlines, or
//! failing RHS pipelines around `Set-Content` / `Out-File` / `>` / `python -c` can truncate
//! an existing non-empty source file (such as `llm_client.py`) to `0` bytes before failing.
//! Models frequently react by repeatedly copying empty or missing `.bak` files in a tight
//! credit-burning loop.
//!
//! [`FileTruncationGuard`] snapshots existing non-empty source files before shell execution,
//! automatically restores any source file truncated to `0` bytes, and trips a deterministic
//! circuit breaker on empty or repeated `.bak` restore attempts.

use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;

const MAX_SNAPSHOT_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_SNAPSHOT_TOTAL_BYTES: usize = 8 * 1024 * 1024;
const MAX_SNAPSHOT_FILES: usize = 256;
const MAX_WALK_DEPTH: usize = 3;
const MAX_BAK_ATTEMPTS: u32 = 2;

const PROTECTED_SOURCE_EXTENSIONS: &[&str] = &[
    "py", "pyi", "rs", "ts", "tsx", "js", "jsx", "mjs", "cjs", "go", "java", "kt", "swift", "c",
    "cc", "cpp", "cxx", "h", "hpp", "cs", "rb", "php", "scala", "lua", "zig", "sql", "sh", "bash",
    "zsh", "ps1", "psm1", "psd1", "toml", "yaml", "yml", "json", "md", "html", "css", "scss",
    "vue", "svelte", "proto", "graphql",
];

const IGNORED_DIRS: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    ".venv",
    "venv",
    "__pycache__",
    "dist",
    "build",
    ".next",
    ".cache",
];

const BACKUP_SUFFIXES: &[&str] = &[".bak", ".backup", ".orig", ".old", ".save"];

/// Pre-command snapshot of non-empty workspace source files.
#[derive(Debug, Default)]
pub struct WorkspaceSnapshot {
    cwd: Option<PathBuf>,
    files: Vec<(PathBuf, Vec<u8>)>,
    bak_restore_key: Option<String>,
}

#[derive(Debug, Default)]
struct GuardState {
    bak_attempts: HashMap<String, u32>,
    restored_counts: HashMap<PathBuf, u32>,
}

/// Thread-safe per-session file-truncation and `.bak` loop guard.
#[derive(Clone, Debug, Default)]
pub struct FileTruncationGuard {
    state: Arc<Mutex<GuardState>>,
}

impl FileTruncationGuard {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, GuardState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Inspects a shell command before execution:
    /// 1. Blocks restores from `0`-byte `.bak` files immediately and trips a circuit breaker
    ///    on repeated failing/unchanged `.bak` restore loops (`>= 2` attempts).
    /// 2. Snapshots existing non-empty protected source files in `cwd` (prioritizing paths
    ///    mentioned in `cmd`) so accidental `0`-byte truncation can be auto-restored.
    pub fn pre_shell_command(
        &self,
        cmd: &str,
        cwd: Option<&Path>,
    ) -> Result<WorkspaceSnapshot, String> {
        let bak_restore = detect_backup_restore_source(cmd);
        let mut bak_restore_key = None;

        if let Some(ref raw_bak_source) = bak_restore {
            let resolved_bak = cwd.map(|base| resolve_candidate_path(base, raw_bak_source));
            let key = resolved_bak
                .as_ref()
                .map(|p| p.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_else(|| normalize_cmd_key(cmd));

            if let Some(ref bak_path) = resolved_bak
                && let Ok(meta) = fs::metadata(bak_path)
                && meta.is_file()
                && meta.len() == 0
            {
                let mut state = self.lock();
                let count = state.bak_attempts.entry(key).or_insert(0);
                *count += 1;
                let target_hint = strip_backup_suffix(raw_bak_source).unwrap_or("<target_file>");
                return Err(format!(
                    "CIRCUIT BREAKER: Blocked restore from '{raw_bak_source}' because that backup file is 0 bytes (empty). \
                     Copying an empty .bak file wipes '{target_hint}' to 0 bytes. Do NOT retry restoring from '{raw_bak_source}'. \
                     Restore from git (`git checkout -- {target_hint}` / `git restore {target_hint}`) if tracked, \
                     or write the complete file cleanly using the native `apply_patch` tool."
                ));
            }

            let attempts = {
                let state = self.lock();
                state.bak_attempts.get(&key).copied().unwrap_or(0)
            };
            if attempts + 1 >= MAX_BAK_ATTEMPTS {
                let mut state = self.lock();
                let count = state.bak_attempts.entry(key).or_insert(0);
                *count += 1;
                let target_hint = strip_backup_suffix(raw_bak_source).unwrap_or("<target_file>");
                return Err(format!(
                    "CIRCUIT BREAKER: Halted repeated `.bak` restore loop (attempt #{count}) from '{raw_bak_source}'. \
                     Repeating backup copy/restore commands is burning turns without restoring valid code. \
                     Stop using `.bak` files immediately — run `git restore {target_hint}` or rewrite `{target_hint}` directly with `apply_patch`."
                ));
            }
            bak_restore_key = Some(key);
        }

        let Some(base_cwd) = cwd.filter(|p| p.is_dir()) else {
            return Ok(WorkspaceSnapshot {
                cwd: None,
                files: Vec::new(),
                bak_restore_key,
            });
        };

        let files = collect_snapshot_files(base_cwd, cmd);
        Ok(WorkspaceSnapshot {
            cwd: Some(base_cwd.to_path_buf()),
            files,
            bak_restore_key,
        })
    }

    /// Runs after a shell command finishes:
    /// 1. Tracks `.bak` restore attempts so repeated restores trip the circuit breaker.
    /// 2. Restores any previously non-empty source file that was wiped to `0` bytes and
    ///    returns a warning banner to append to the tool output.
    pub fn post_shell_command(
        &self,
        _cmd: &str,
        exit_code: Option<i32>,
        snapshot: WorkspaceSnapshot,
    ) -> Option<String> {
        if let Some(key) = snapshot.bak_restore_key {
            let mut state = self.lock();
            let count = state.bak_attempts.entry(key).or_insert(0);
            // Every `.bak` restore counts toward the loop cap; non-zero exit counts immediately.
            *count += if exit_code.is_some_and(|code| code != 0) {
                2
            } else {
                1
            };
        }

        let mut warnings = Vec::new();
        for (path, original_bytes) in snapshot.files {
            if original_bytes.is_empty() {
                continue;
            }
            let truncated_to_zero = match fs::metadata(&path) {
                Ok(meta) => meta.is_file() && meta.len() == 0,
                Err(_) => false,
            };
            if !truncated_to_zero {
                continue;
            }

            let restored_ok = fs::write(&path, &original_bytes).is_ok();
            let count = {
                let mut state = self.lock();
                let entry = state.restored_counts.entry(path.clone()).or_insert(0);
                *entry += 1;
                *entry
            };
            let display = snapshot
                .cwd
                .as_deref()
                .and_then(|base| path.strip_prefix(base).ok())
                .unwrap_or(path.as_path())
                .display();
            let byte_len = original_bytes.len();
            if restored_ok {
                let mut msg = format!(
                    "[FILE-TRUNCATION GUARD] WARNING: Your shell command truncated existing source file \
                     '{display}' ({byte_len} bytes -> 0 bytes), likely due to shell quote/newline/redirection escaping. \
                     '{display}' has been AUTOMATICALLY RESTORED to its pre-command contents ({byte_len} bytes). \
                     Do NOT use shell redirection (`>`, `Set-Content`, `Out-File`, `python -c`) or `.bak` copies to edit source files — \
                     use the native `apply_patch` tool instead."
                );
                if count >= 2 {
                    msg.push_str(&format!(
                        " CIRCUIT BREAKER ALERT: '{display}' has been truncated to 0 bytes {count} times in this session; \
                         stop running shell write commands against '{display}' immediately."
                    ));
                }
                warnings.push(msg);
            } else {
                warnings.push(format!(
                    "[FILE-TRUNCATION GUARD] ERROR: Shell command truncated '{display}' ({byte_len} bytes -> 0 bytes) \
                     and automatic restore failed. Use `git checkout -- {display}` or `apply_patch` to restore it."
                ));
            }
        }

        if warnings.is_empty() {
            None
        } else {
            Some(warnings.join("\n"))
        }
    }
}

fn normalize_cmd_key(cmd: &str) -> String {
    cmd.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn is_backup_token(token: &str) -> bool {
    let lower = token.to_ascii_lowercase();
    BACKUP_SUFFIXES.iter().any(|suffix| lower.ends_with(suffix))
}

fn strip_backup_suffix(token: &str) -> Option<&str> {
    let lower = token.to_ascii_lowercase();
    for suffix in BACKUP_SUFFIXES {
        if lower.ends_with(suffix) && token.len() > suffix.len() {
            return Some(&token[..token.len() - suffix.len()]);
        }
    }
    None
}

/// Detects when `cmd` copies, moves, or redirects *from* a `.bak`/`.backup`/`.orig` file
/// (as opposed to creating a backup `cp foo.py foo.py.bak` or deleting `rm foo.py.bak`).
pub fn detect_backup_restore_source(cmd: &str) -> Option<String> {
    let lower = cmd.to_ascii_lowercase();
    if !BACKUP_SUFFIXES.iter().any(|suffix| lower.contains(suffix)) {
        return None;
    }

    // Pipeline / redirection from a backup read: `Get-Content foo.py.bak | Set-Content foo.py` or `cat foo.py.bak > foo.py`
    for segment in cmd.split([';', '\n']) {
        let seg_trim = segment.trim();
        if let Some((lhs, rhs)) = seg_trim.split_once('|')
            && contains_write_sink(rhs)
        {
            for tok in extract_clean_tokens(lhs) {
                if is_backup_token(&tok) {
                    return Some(tok);
                }
            }
        }
        if let Some((lhs, _rhs)) = split_file_redirection(seg_trim) {
            for tok in extract_clean_tokens(lhs) {
                if is_backup_token(&tok) {
                    return Some(tok);
                }
            }
        }

        // Copy / move commands where the first non-flag path argument (or `-Path` / `-LiteralPath`) is `.bak`.
        let tokens = extract_clean_tokens(seg_trim);
        if tokens.is_empty() {
            continue;
        }
        let head = tokens[0].to_ascii_lowercase();
        let is_copy_or_move = matches!(
            head.as_str(),
            "cp" | "mv"
                | "copy"
                | "move"
                | "xcopy"
                | "copy-item"
                | "cpi"
                | "move-item"
                | "mi"
        );
        if is_copy_or_move
            && let Some(src) = extract_copy_source_arg(&tokens[1..])
            && is_backup_token(&src)
        {
            return Some(src);
        }

        // Inline Python `shutil.copy('foo.py.bak', 'foo.py')` or `Path('foo.py.bak').read_text()`
        if (lower.contains("shutil.copy")
            || lower.contains("shutil.move")
            || lower.contains("read_text")
            || lower.contains("read_bytes")
            || lower.contains("open("))
            && let Some(first_bak) = tokens.into_iter().find(|t| is_backup_token(t))
        {
            return Some(first_bak);
        }
    }
    None
}

fn contains_write_sink(rhs: &str) -> bool {
    let lower = rhs.to_ascii_lowercase();
    lower.contains("set-content")
        || lower.contains("out-file")
        || lower.contains("add-content")
        || lower.contains("tee-object")
        || lower.contains("tee ")
}

fn split_file_redirection(segment: &str) -> Option<(&str, &str)> {
    let bytes = segment.as_bytes();
    for i in 0..bytes.len() {
        if bytes[i] == b'>' {
            if i > 0 && matches!(bytes[i - 1], b'-' | b'=' | b'>') {
                continue;
            }
            let after = if i + 1 < bytes.len() && bytes[i + 1] == b'>' {
                &segment[i + 2..]
            } else {
                &segment[i + 1..]
            };
            let trimmed = after.trim_start();
            if trimmed.starts_with('&')
                || trimmed.starts_with("/dev/null")
                || trimmed.to_ascii_lowercase().starts_with("$null")
            {
                continue;
            }
            return Some((&segment[..i], trimmed));
        }
    }
    None
}

fn extract_copy_source_arg(args: &[String]) -> Option<String> {
    let mut i = 0usize;
    while i < args.len() {
        let lower = args[i].to_ascii_lowercase();
        if matches!(lower.as_str(), "-path" | "-literalpath") {
            return args.get(i + 1).cloned();
        }
        if matches!(
            lower.as_str(),
            "-destination" | "-filter" | "-include" | "-exclude"
        ) {
            i += 2;
            continue;
        }
        if lower.starts_with('-') {
            i += 1;
            continue;
        }
        return Some(args[i].clone());
    }
    None
}

fn extract_clean_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '(' | ')' | ',' | ';' | '|' | '>' | '<')
    }) {
        let trimmed = raw.trim_matches(|c: char| matches!(c, '\'' | '"' | '`'));
        if !trimmed.is_empty() {
            out.push(trimmed.to_string());
        }
    }
    out
}

fn resolve_candidate_path(base: &Path, raw: &str) -> PathBuf {
    let cleaned = raw.strip_prefix(".\\").unwrap_or(raw);
    let candidate = Path::new(cleaned);
    if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        base.join(candidate)
    }
}

fn is_protected_source_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| {
            PROTECTED_SOURCE_EXTENSIONS
                .iter()
                .any(|allowed| ext.eq_ignore_ascii_case(allowed))
        })
}

fn collect_snapshot_files(base_cwd: &Path, cmd: &str) -> Vec<(PathBuf, Vec<u8>)> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();
    let mut total_bytes = 0usize;

    let mut try_add = |path: PathBuf| {
        if files.len() >= MAX_SNAPSHOT_FILES || total_bytes >= MAX_SNAPSHOT_TOTAL_BYTES {
            return;
        }
        if !is_protected_source_file(&path) {
            return;
        }
        let canonical = path.canonicalize().unwrap_or_else(|_| path.clone());
        if !seen.insert(canonical) {
            return;
        }
        let Ok(meta) = fs::metadata(&path) else {
            return;
        };
        if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SNAPSHOT_FILE_BYTES {
            return;
        }
        if total_bytes + (meta.len() as usize) > MAX_SNAPSHOT_TOTAL_BYTES {
            return;
        }
        if let Ok(bytes) = fs::read(&path)
            && !bytes.is_empty()
        {
            total_bytes += bytes.len();
            files.push((path, bytes));
        }
    };

    // 1. Prioritize explicit file tokens mentioned in `cmd` (plus stripped `.bak` targets).
    for tok in extract_clean_tokens(cmd) {
        if tok.contains('.') && !tok.starts_with('-') {
            try_add(resolve_candidate_path(base_cwd, &tok));
            if let Some(stripped) = strip_backup_suffix(&tok) {
                try_add(resolve_candidate_path(base_cwd, stripped));
            }
        }
    }

    // 2. Walk shallow workspace source files so globs / indirect scripts (`*.py`) are also covered.
    walk_workspace_files(base_cwd, 0, &mut try_add);
    files
}

fn walk_workspace_files(dir: &Path, depth: usize, add: &mut impl FnMut(PathBuf)) {
    if depth > MAX_WALK_DEPTH {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        if ft.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if IGNORED_DIRS
                .iter()
                .any(|ignored| name_str.eq_ignore_ascii_case(ignored))
            {
                continue;
            }
            walk_workspace_files(&path, depth + 1, add);
        } else if ft.is_file() {
            add(path);
        }
    }
}

#[cfg(test)]
#[path = "file_guard_tests.rs"]
mod tests;
