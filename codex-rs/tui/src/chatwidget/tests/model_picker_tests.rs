//! Model-family presentation at constrained sizes and with configured list bindings.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn model_picker_compact_hint_and_actions_follow_configured_bindings() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    let mut keymap = crate::keymap::RuntimeKeymap::defaults();
    keymap.list.accept = vec![key_hint::plain(KeyCode::F(3))];
    keymap.list.cancel = vec![key_hint::plain(KeyCode::F(2))];
    chat.bottom_pane.set_keymap_bindings(&keymap);
    chat.open_all_models_popup();
    let popup = render_bottom_popup(&chat, /*width*/ 40);
    assert_eq!(popup.lines().last().unwrap().trim(), "f3 select · f2 back");
    chat.handle_key_event(KeyEvent::from(KeyCode::F(3)));
    assert_matches!(rx.try_recv(), Ok(AppEvent::OpenReasoningPopup { .. }));
    chat.handle_key_event(KeyEvent::from(KeyCode::F(2)));
    assert!(chat.no_modal_or_popup_active());
}

#[tokio::test]
async fn model_picker_opens_after_discovery_populates_an_empty_catalog() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(Some("gpt-5.5")).await;
    chat.thread_id = Some(ThreadId::new());
    let presets = std::mem::take(&mut Arc::make_mut(&mut chat.model_catalog).models);
    chat.open_model_popup();
    let loading = render_bottom_popup(&chat, /*width*/ 80);
    let loading_title = loading.lines().find(|line| line.contains("Loading models"));
    insta::assert_snapshot!(loading_title.unwrap().trim(), @"Loading models…");
    let obsolete = chat.model_popup_request_id.unwrap();
    chat.handle_key_event(KeyCode::Esc.into());
    assert!(chat.no_modal_or_popup_active());
    chat.open_model_popup();
    assert!(!chat.on_models_loaded(obsolete, Ok(presets.clone())));
    let request_id = chat.model_popup_request_id.unwrap();
    assert!(chat.on_models_loaded(request_id, Ok(presets)));
    let popup = render_bottom_popup(&chat, /*width*/ 80);
    assert_chatwidget_snapshot!("model_selection_popup", popup);
}

#[tokio::test]
async fn hanzo_model_picker_orders_enso_then_zen_hides_third_party_by_default_and_hot_switches() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(Some("enso")).await;
    chat.thread_id = Some(ThreadId::new());
    chat.config.model_provider_id = "hanzo".to_string();
    chat.config.model_provider.base_url = Some("https://api.hanzo.ai/v1".to_string());

    let base = get_available_model(&chat, "enso");
    let make_preset = |slug: &str, name: &str, provider: Option<&str>| {
        let mut preset = base.clone();
        preset.id = slug.to_string();
        preset.model = slug.to_string();
        preset.display_name = name.to_string();
        preset.description = format!("{name} model");
        preset.show_in_picker = true;
        preset.supported_reasoning_efforts.clear();
        preset.model_provider = provider.map(str::to_owned);
        preset
    };

    let presets = vec![
        make_preset("aion-labs/aion-1.0", "Aion 1.0", Some("hanzo")),
        make_preset("qwen3-coder", "Qwen3 Coder", Some("ollama")),
        make_preset("zen6-coder", "Zen 6 Coder", Some("hanzo")),
        make_preset("enso", "Enso", Some("hanzo")),
    ];
    chat.model_catalog = Arc::new(ModelCatalog::new(presets.clone()));
    chat.open_model_popup_with_presets(presets);

    let popup = render_bottom_popup(&chat, /*width*/ 100);
    assert!(!popup.contains("1. "), "expected numberless picker:\n{popup}");
    assert!(
        !popup.contains("Aion 1.0"),
        "expected third-party cloud model hidden by default:\n{popup}"
    );
    let enso_pos = popup.find("Enso").expect("Enso shown");
    let zen_pos = popup.find("Zen 6 Coder").expect("Zen 6 Coder shown");
    let local_pos = popup.find("Zen 6 Nano").expect("Zen 6 Nano [local] shown");
    let ollama_pos = popup.find("Qwen3 Coder").expect("configured ollama model shown");
    assert!(popup.contains("[hanzo]"), "expected [hanzo] tag:\n{popup}");
    assert!(popup.contains("[local]"), "expected [local] tag:\n{popup}");
    assert!(popup.contains("[ollama]"), "expected [ollama] tag:\n{popup}");
    assert!(enso_pos < zen_pos, "Enso must precede Zen:\n{popup}");
    assert!(zen_pos < local_pos, "Zen 6 Coder must precede Zen 6 Nano:\n{popup}");
    assert!(
        local_pos < ollama_pos,
        "Zen models must precede custom providers:\n{popup}"
    );

    // Enabling third-party cloud models reveals Aion 1.0 after Enso, Zen, and custom providers.
    chat.local_settings.tui.show_third_party_models = true;
    chat.refresh_open_model_picker();
    let expanded = render_bottom_popup(&chat, /*width*/ 100);
    let ollama_expanded = expanded.find("Qwen3 Coder").expect("Qwen3 Coder shown");
    let aion_pos = expanded
        .find("Aion 1.0")
        .expect("third-party cloud model shown when enabled");
    assert!(
        ollama_expanded < aion_pos,
        "third-party cloud models must follow Enso, Zen, and custom providers:\n{expanded}"
    );

    // Selecting Zen 6 Nano [local] via session shortcut 's' qualifies the provider for hot-switch.
    chat.local_settings.tui.show_third_party_models = false;
    chat.refresh_open_model_picker();
    while rx.try_recv().is_ok() {}
    // Move from Enso (row 0) -> Zen 6 Coder (row 1) -> Zen 6 Nano [local] (row 2).
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Down));
    chat.handle_key_event(KeyEvent::from(KeyCode::Char('s')));
    let events: Vec<AppEvent> = std::iter::from_fn(|| rx.try_recv().ok()).collect();
    assert!(
        events.iter().any(|event| matches!(
            event,
            AppEvent::SelectSessionModel { model, .. } if model == "local::zen6-nano"
        )),
        "expected SelectSessionModel with local::zen6-nano, got {events:?}"
    );
}
