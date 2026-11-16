use super::*;

/// #763: the doctor reported `~/Library/Logs/com.openai.codex`, the Codex app.
#[test]
fn logs_are_hanzo_desktops_on_macos() {
    let home = Path::new("/Users/someone");
    assert_eq!(
        log_root("macos", Some(home), None),
        Some(PathBuf::from("/Users/someone/Library/Logs/com.hanzo.desktop"))
    );
    assert_eq!(log_root("macos", None, None), None);
}

#[test]
fn logs_are_hanzo_desktops_on_windows() {
    assert_eq!(
        log_root("windows", None, Some(Path::new("C:/Users/s/AppData/Local"))),
        Some(PathBuf::from("C:/Users/s/AppData/Local/com.hanzo.desktop/logs"))
    );
    assert_eq!(
        log_root("windows", Some(Path::new("C:/Users/s")), None),
        Some(PathBuf::from("C:/Users/s/AppData/Local/com.hanzo.desktop/logs"))
    );
    assert_eq!(log_root("linux", Some(Path::new("/home/s")), None), None);
}

/// The bundles looked for are Hanzo Desktop's, never ChatGPT.app or Codex.app.
#[test]
fn the_app_looked_for_is_hanzo_desktop() {
    let bundles = macos_bundles(Some(Path::new("/Users/someone")));
    assert_eq!(
        bundles,
        vec![
            PathBuf::from("/Applications/Hanzo Desktop.app"),
            PathBuf::from("/Users/someone/Applications/Hanzo Desktop.app"),
        ]
    );
    assert_eq!(BUNDLE_ID, "com.hanzo.desktop");
}

/// The update probe and PATH count name Hanzo's feed and command, not chatgpt.com
/// and `codex`.
#[test]
fn the_feed_and_command_are_hanzos() {
    assert!(UPDATE_URL.starts_with("https://github.com/hanzoai/desktop/"));
    assert!(!UPDATE_URL.contains("chatgpt.com") && !UPDATE_URL.contains("oaistatic"));
    assert!(UPDATE_URL.starts_with(&format!("https://{UPDATE_HOST}/")));
    assert_eq!(BINARY, "dev");
}
