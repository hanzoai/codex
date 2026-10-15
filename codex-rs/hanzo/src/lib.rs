//! Hanzo Dev's names and defaults, its model catalog, the controller interface
//! the agent loop consults, and the version the process runs as.
//!
//! Upstream crates read these where they would read their own. Nothing here
//! calls out of this repository: what a binary adds (Kai, the Hanzo account)
//! reaches the engines through the hooks codex-app-server and codex-tui offer.

pub mod catalog;
pub mod controller;
pub mod model;
pub mod version;

pub const PRODUCT_NAME: &str = "Hanzo Dev";
pub const INPUT_PLACEHOLDER: &str = "Ask Hanzo Dev to do anything";
pub const STARTUP_TIP: &str = "Use **/mcp** to inspect connected tools. Hanzo MCP provides cloud controls and local development tools.";

/// Dev checks no one else's releases. The check upstream makes compares this build
/// with another product's latest release and offers that product's installer.
pub const UPDATE_CHECK: bool = false;
/// Dev runs its server inside the process. Upstream's shared background server
/// starts from an installer's package directory, which Dev does not ship.
pub const BACKGROUND_SERVER: bool = false;
/// Dev draws in the terminal's own foreground and background: no hue and no grey,
/// with weight and reverse video for emphasis.
pub const MONOCHROME: bool = true;
/// One line under the input: the status line takes the row the shortcut hint would.
pub const SEPARATE_STATUS_LINE: bool = false;

/// Keep light terminals readable and dark input bars much subtler than the upstream default.
pub fn input_background(background: (u8, u8, u8), light: bool) -> (u8, u8, u8) {
    let (target, alpha) = if light { (0.0, 0.04) } else { (255.0, 0.055) };
    let mix = |channel: u8| (channel as f32 * (1.0 - alpha) + target * alpha).round() as u8;
    (mix(background.0), mix(background.1), mix(background.2))
}

/// Hanzo's API, which serves [`catalog::MODELS`].
pub const API_BASE: &str = "https://api.hanzo.ai/v1";
/// The model a profile that names none runs on.
pub const DEFAULT_MODEL: &str = "enso-auto";

/// Whether a provider's base URL is [`API_BASE`].
pub fn is_hanzo(base_url: Option<&str>) -> bool {
    base_url.is_some_and(|url| url.trim_end_matches('/') == API_BASE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_hanzos_api_is_hanzo() {
        assert!(is_hanzo(Some(API_BASE)));
        assert!(is_hanzo(Some("https://api.hanzo.ai/v1/")));
        assert!(!is_hanzo(Some("http://dgx.local:1235/v1")));
        assert!(!is_hanzo(Some("https://api.hanzo.ai.example/v1")));
        assert!(!is_hanzo(None));
    }
}
