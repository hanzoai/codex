//! Hanzo Desktop as `dev doctor` inspects it: the app bundle, the identifier its
//! logs are filed under, the feed it updates from, and the command Dev installs as.
//!
//! Upstream's doctor looks for OpenAI's Codex app (`com.openai.codex`), probes
//! `chatgpt.com` for its appcast and counts `codex` binaries on PATH. On a Hanzo
//! install none of those exist, so the report described a different product.

use std::path::Path;
use std::path::PathBuf;

/// The bundle identifier Hanzo Desktop ships with (`tauri.conf.json` `identifier`).
pub const BUNDLE_ID: &str = "com.hanzo.desktop";
/// The macOS application bundle name.
pub const APP_BUNDLE: &str = "Hanzo Desktop.app";
/// The update feed Hanzo Desktop's updater reads (`plugins.updater.endpoints`).
pub const UPDATE_URL: &str = "https://github.com/hanzoai/desktop/releases/latest/download/latest.json";
/// The host [`UPDATE_URL`] is served from, named in the doctor's remedy.
pub const UPDATE_HOST: &str = "github.com";
/// The command Dev installs as (`~/.local/bin/dev`).
pub const BINARY: &str = "dev";

/// The application bundles to look for, in order: `/Applications`, then
/// `~/Applications`.
pub fn macos_bundles(home: Option<&Path>) -> Vec<PathBuf> {
    std::iter::once(PathBuf::from("/Applications"))
        .chain(home.map(|home| home.join("Applications")))
        .map(|directory| directory.join(APP_BUNDLE))
        .collect()
}

/// Where Hanzo Desktop writes its logs on `os` (Tauri's log directory): macOS
/// `~/Library/Logs/<id>`, Windows `%LOCALAPPDATA%\<id>\logs`. Other systems have no
/// desktop app to inspect.
pub fn log_root(os: &str, home: Option<&Path>, local_app_data: Option<&Path>) -> Option<PathBuf> {
    match os {
        "macos" => Some(home?.join("Library/Logs").join(BUNDLE_ID)),
        "windows" => Some(
            local_app_data
                .map(Path::to_path_buf)
                .or_else(|| home.map(|home| home.join("AppData/Local")))?
                .join(BUNDLE_ID)
                .join("logs"),
        ),
        _ => None,
    }
}

#[cfg(test)]
#[path = "desktop_tests.rs"]
mod tests;
