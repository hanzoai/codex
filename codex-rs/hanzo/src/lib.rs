//! Hanzo Dev's names and defaults, its model catalog, the controller interface
//! the agent loop consults, and the version the process runs as.
//!
//! Upstream crates read these where they would read their own. Nothing here
//! calls out of this repository: what a binary adds (Kai, the Hanzo account)
//! reaches the engines through the hooks codex-app-server and codex-tui offer.

pub mod catalog;
pub mod controller;
pub mod logo;
pub mod model;
pub mod version;

pub const PRODUCT_NAME: &str = "Hanzo Dev";
pub const INPUT_PLACEHOLDER: &str = "Ask Hanzo Dev to do anything";
pub const STARTUP_TIP: &str = "Use **/mcp** to inspect connected tools. Hanzo MCP provides cloud controls and local development tools.";

/// Dev checks its own latest release at startup and offers the way it was installed
/// to update it: npm, Homebrew, or hanzo.sh.
pub const UPDATE_CHECK: bool = true;
pub const LATEST_RELEASE: &str = "https://api.github.com/repos/hanzoai/dev/releases/latest";
pub const RELEASE_NOTES: &str = "https://github.com/hanzoai/dev/releases/latest";
/// A release is tagged `v` and its version.
pub const RELEASE_TAG_PREFIX: &str = "v";
/// The installer every Hanzo tool comes from; run again, it updates.
pub const INSTALL_URL: &str = "https://hanzo.sh";
pub const INSTALL_SCRIPT: &str = "curl -fsSL https://hanzo.sh | sh -s -- -q dev";
/// The npm package Dev ships as, its registry document, and the variable its launcher
/// sets so the binary knows npm owns it.
pub const NPM_PACKAGE: &str = "@hanzo/dev";
pub const NPM_REGISTRY: &str = "https://registry.npmjs.org/@hanzo%2fdev";
pub const NPM_MANAGED: &str = "DEV_MANAGED_BY_NPM";
/// The Homebrew formula in Hanzo's tap; the release that publishes Dev updates it.
pub const BREW_FORMULA: &str = "hanzoai/tap/hanzo-dev";

/// The Hanzo mark (brand/assets/logo/favicon.svg) as one path, with its bounds: the
/// shape the welcome animation draws.
pub const MARK: (&str, [f64; 4]) = (
    "M22.21 67V44.6369H0V67H22.21ZM66.7038 22.3184H22.2534L0.0878906 44.6367H44.4634L66.7038 22.3184ZM22.21 0H0V22.3184H22.21V0ZM66.7198 0H44.5098V22.3184H66.7198V0ZM66.7198 67V44.6369H44.5098V67H66.7198Z",
    [0.0, 0.0, 66.7198, 67.0],
);
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
