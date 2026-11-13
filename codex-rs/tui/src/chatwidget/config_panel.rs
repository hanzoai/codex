//! Entry points for the configuration browser and its existing child editors.

use super::ChatWidget;
use crate::activation_key_guard::ActivationKeySuppression;
use crate::config_panel::ConfigEditor;
use crate::config_panel::ConfigEditorHandoff;
use crate::config_panel::ConfigPanel;
use crate::config_panel::ConfigPanelStatus;
use crate::config_panel::ConfigPanelUpdate;
use crate::config_panel::ConfigPreference;
use crate::config_panel::ConfigToggle;
use crossterm::event::KeyEvent;

impl ChatWidget {
    pub(crate) fn open_config_panel(&mut self) {
        self.bottom_pane.show_view(Box::new(ConfigPanel::new(
            &self.config_panel_tui,
            self.app_event_tx.clone(),
            self.bottom_pane.list_keymap(),
            self.config_panel_status(),
        )));
    }

    pub(crate) fn open_settings_panel(&mut self, section_hint: Option<&str>) {
        if let Some(raw) = section_hint.map(str::trim).filter(|s| !s.is_empty()) {
            match raw.to_ascii_lowercase().as_str() {
                "model" | "models" | "effort" | "reasoning" => {
                    self.open_model_popup();
                    return;
                }
                "theme" | "themes" => {
                    self.open_theme_picker();
                    return;
                }
                "provider" | "providers" => {
                    let (view, event) = crate::app::provider::loading();
                    self.bottom_pane.show_selection_view(view);
                    self.app_event_tx
                        .send(crate::app_event::AppEvent::Provider(event));
                    return;
                }
                "account" | "accounts" | "auth" => {
                    crate::account::show(&mut |line| self.add_info_message(line, None));
                    return;
                }
                "login" => {
                    self.dispatch_command(crate::slash_command::SlashCommand::Login);
                    return;
                }
                "agent" | "agents" | "subagents" | "autodrive" | "auto" => {
                    self.app_event_tx
                        .send(crate::app_event::AppEvent::OpenAgentsOverview);
                    return;
                }
                "memory" | "memories" => {
                    self.open_memories_popup();
                    return;
                }
                "skill" | "skills" => {
                    self.open_skills_list();
                    return;
                }
                "mcp" => {
                    self.add_mcp_output(super::McpServerStatusDetail::ToolsAndAuthOnly);
                    return;
                }
                "permission" | "permissions" | "approval" | "sandbox" | "validation" => {
                    self.open_permissions_popup();
                    return;
                }
                "statusline" | "status-line" => {
                    self.open_status_line_setup();
                    return;
                }
                "title" | "terminal-title" => {
                    self.open_terminal_title_setup();
                    return;
                }
                "keyboard" | "keymap" | "shortcuts" | "keys" => {
                    self.open_keymap_picker();
                    return;
                }
                "limit" | "limits" | "usage" => {
                    self.dispatch_command(crate::slash_command::SlashCommand::Status);
                    return;
                }
                "notification" | "notifications" | "notify" | "config" | "preferences"
                | "appearance" | "effects" | "input" | "sessions" | "selection" => {
                    self.open_config_panel();
                    return;
                }
                _ => {}
            }
        }

        let compact_model =
            hanzo::catalog::compact_name(self.current_model(), self.model_display_name());
        let effort_symbol = hanzo::effort::symbol_for(
            self.current_model(),
            self.effective_reasoning_effort()
                .as_ref()
                .map(codex_protocol::openai_models::ReasoningEffort::as_str),
        );
        let model_summary = format!("{compact_model} {effort_symbol} ›");
        let theme_summary = format!(
            "{} ›",
            self.local_settings
                .tui
                .theme
                .as_deref()
                .unwrap_or("Automatic")
        );
        let provider_summary = format!("{} ›", self.config.model_provider_id);
        let notifications_enabled = !matches!(
            self.local_settings.tui.notification_settings.notifications,
            codex_config::types::Notifications::Enabled(false)
        );
        let notif_summary = if notifications_enabled {
            "On ›".to_string()
        } else {
            "Off ›".to_string()
        };
        let mcp_summary = format!("{} configured ›", self.config.mcp_servers.get().len());

        let sections: [(&str, &str, String, &str); 15] = [
            (
                "model",
                "Model & Effort",
                model_summary,
                "Choose the language model and reasoning effort (○ ◔ ◑ ◕ ●) used for turns.",
            ),
            (
                "theme",
                "Theme",
                theme_summary,
                "Switch between preset color palettes with a live preview.",
            ),
            (
                "providers",
                "Model Providers",
                provider_summary,
                "Choose the default model provider (Hanzo Cloud, Local Engine, or custom).",
            ),
            (
                "accounts",
                "Accounts",
                "Inspect ›".to_string(),
                "View signed-in Hanzo account status and authentication.",
            ),
            (
                "agents",
                "Agents",
                "Command Center ›".to_string(),
                "Open the agent command center and manage subagents.",
            ),
            (
                "memories",
                "Memories",
                "Configure ›".to_string(),
                "Control whether future turns use cross-session memory guidance.",
            ),
            (
                "skills",
                "Skills",
                "Manage ›".to_string(),
                "Manage project-scoped and global skills.",
            ),
            (
                "mcp",
                "MCP",
                mcp_summary,
                "Inspect and manage connected MCP servers and tools.",
            ),
            (
                "permissions",
                "Permissions",
                "Configure ›".to_string(),
                "Choose what Hanzo Dev is allowed to execute and approve.",
            ),
            (
                "notifications",
                "Notifications",
                notif_summary,
                "Adjust desktop and terminal notification preferences.",
            ),
            (
                "statusline",
                "Status Line",
                "Configure ›".to_string(),
                "Choose and reorder items shown below the composer.",
            ),
            (
                "title",
                "Terminal Title",
                "Configure ›".to_string(),
                "Choose and reorder items in your terminal tab or window title.",
            ),
            (
                "keyboard",
                "Keyboard Shortcuts",
                "Remap ›".to_string(),
                "Browse actions and remap their shortcut keys.",
            ),
            (
                "limits",
                "Limits & Usage",
                "Inspect ›".to_string(),
                "Inspect API usage, rate limits, and token activity.",
            ),
            (
                "config",
                "All Preferences",
                "Appearance, Input & Sessions ›".to_string(),
                "Open the full tabbed TUI preferences panel (/config).",
            ),
        ];

        let items = sections
            .into_iter()
            .map(|(key, name, summary, desc)| {
                let section_key = key.to_string();
                crate::bottom_pane::SelectionItem {
                    name: name.to_string(),
                    category_tag: Some(format!("{summary:<28}")),
                    description: Some(desc.to_string()),
                    search_value: Some(format!("{name} {key} {desc}")),
                    actions: vec![Box::new(move |tx| {
                        tx.send(crate::app_event::AppEvent::OpenSettingsSection(
                            section_key.clone(),
                        ));
                    })],
                    dismiss_on_select: true,
                    ..Default::default()
                }
            })
            .collect();

        let keymap = self.bottom_pane.list_keymap();
        self.bottom_pane
            .show_selection_view(crate::bottom_pane::SelectionViewParams {
                view_id: Some("settings-overview"),
                title: Some("Settings".to_string()),
                subtitle: Some("Manage all Hanzo Dev settings in one place".to_string()),
                footer_hint: Some(
                    crate::bottom_pane::popup_consts::picker_hint_line_for_keymap(&keymap),
                ),
                is_searchable: true,
                search_placeholder: Some("Type to search settings".to_string()),
                items,
                ..crate::bottom_pane::SelectionViewParams::picker()
            });
    }

    pub(crate) fn open_config_editor(
        &mut self,
        handoff: ConfigEditorHandoff,
        resume_current_availability: crate::config_panel::ResumeCurrentAvailability,
    ) {
        match handoff.editor {
            ConfigEditor::ModelProviders => {
                let (view, event) = crate::app::provider::loading();
                self.bottom_pane.show_selection_view(view);
                self.app_event_tx
                    .send(crate::app_event::AppEvent::Provider(event));
            }
            ConfigEditor::Theme => self.open_theme_picker(),
            ConfigEditor::Effects => {
                let keymap = self.bottom_pane.list_keymap();
                let params = crate::config_panel::effects(&self.local_settings.tui, &keymap);
                self.bottom_pane.show_selection_view(params);
            }
            ConfigEditor::StatusLine => self.open_status_line_setup(),
            ConfigEditor::TerminalTitle => self.open_terminal_title_setup(),
            ConfigEditor::Keyboard => self.open_keymap_picker(),
            ConfigEditor::SessionPickerLayout => {
                let keymap = self.bottom_pane.list_keymap();
                let params =
                    crate::config_panel::session_picker_layout(&self.local_settings.tui, &keymap);
                self.bottom_pane.show_selection_view(params);
            }
            ConfigEditor::ResumeDirectory => {
                let keymap = self.bottom_pane.list_keymap();
                let params = crate::config_panel::resume_directory(
                    &self.local_settings.tui,
                    &keymap,
                    resume_current_availability,
                );
                self.bottom_pane.show_selection_view(params);
            }
            ConfigEditor::CopyOnSelect => {
                let keymap = self.bottom_pane.list_keymap();
                let params = crate::config_panel::copy_on_select(&self.local_settings.tui, &keymap);
                self.bottom_pane.show_selection_view(params);
            }
            ConfigEditor::RightClickPaste => {
                let keymap = self.bottom_pane.list_keymap();
                let params =
                    crate::config_panel::right_click_paste(&self.local_settings.tui, &keymap);
                self.bottom_pane.show_selection_view(params);
            }
            ConfigEditor::NotificationCondition => {
                let keymap = self.bottom_pane.list_keymap();
                let params =
                    crate::config_panel::notification_condition(&self.local_settings.tui, &keymap);
                self.bottom_pane.show_selection_view(params);
            }
        }
        if let Some(activated_at) = handoff.active_at() {
            self.suppress_current_view_key_until_release(
                handoff.activation_key,
                activated_at,
                ActivationKeySuppression::ViewTransition,
            );
        }
        self.bottom_pane.config_editor_opened();
    }

    pub(crate) fn interrupt_config_editor_transition(&mut self) {
        self.bottom_pane.interrupt_config_editor_transition();
    }

    pub(crate) fn apply_config_preference(
        &mut self,
        preference: ConfigPreference,
        tui: &codex_config::types::Tui,
    ) {
        preference.adopt(&mut self.local_settings.tui, tui);
        let animations_enabled = self.local_settings.tui.animations;
        let disable_paste_burst = self
            .local_settings
            .tui
            .disable_paste_burst
            .unwrap_or(/*default*/ false);
        let ConfigPreference::Toggle { setting, .. } = preference else {
            return;
        };
        match setting {
            ConfigToggle::Animations | ConfigToggle::ComposerStars => {
                let tui = &self.local_settings.tui;
                self.bottom_pane.set_motion_preferences(tui);
                let model = self.current_model().to_string();
                let tui = &self.local_settings.tui;
                self.bottom_pane.stop_ineligible_sparkle(&model, tui);
                self.bottom_pane.select_sparkle_model(&model, tui);
            }
            ConfigToggle::PasteDetection => {
                self.bottom_pane
                    .set_disable_paste_burst(/*disabled*/ disable_paste_burst);
            }
            ConfigToggle::StatusLineColors => {
                self.status_surface_overrides.status_line_use_colors = None;
            }
            ConfigToggle::ThirdPartyModels => {
                self.refresh_open_model_picker();
            }
            ConfigToggle::AutomaticRecaps
            | ConfigToggle::CollapseTurnDetails
            | ConfigToggle::CopyOnSelectClearSelection
            | ConfigToggle::Notifications
            | ConfigToggle::StartInVimMode
            | ConfigToggle::Tips => {}
        }
        if setting == ConfigToggle::Animations
            && let Some(pet) = self.ambient_pet.as_mut()
        {
            pet.set_animations_enabled(animations_enabled);
        }
    }

    pub(crate) fn observe_config_editor_transition_key_event(&mut self, key: KeyEvent) {
        self.bottom_pane
            .observe_config_editor_transition_key_event(key);
    }

    pub(crate) fn refresh_config_panel(&mut self, notice: Option<String>) -> bool {
        self.refresh_config_panel_with_effects_state(notice, Vec::new())
    }

    /// Refresh after an effective save without discarding newer Effects choices still queued.
    pub(crate) fn refresh_config_panel_after_effective_save(
        &mut self,
        notice: Option<String>,
        preference: ConfigPreference,
    ) -> bool {
        if let ConfigPreference::Toggle { setting, .. } = preference {
            match setting {
                ConfigToggle::Animations => {
                    self.bottom_pane.complete_selection_toggle_for_active_view(
                        crate::config_panel::EFFECTS_VIEW_ID,
                        /*index*/ 0,
                    );
                }
                ConfigToggle::ComposerStars => {
                    self.bottom_pane.complete_selection_toggle_for_active_view(
                        crate::config_panel::EFFECTS_VIEW_ID,
                        /*index*/ 1,
                    );
                }
                ConfigToggle::AutomaticRecaps
                | ConfigToggle::CollapseTurnDetails
                | ConfigToggle::CopyOnSelectClearSelection
                | ConfigToggle::Notifications
                | ConfigToggle::PasteDetection
                | ConfigToggle::StartInVimMode
                | ConfigToggle::StatusLineColors
                | ConfigToggle::ThirdPartyModels
                | ConfigToggle::Tips => {}
            }
        }
        let toggle_states = self
            .bottom_pane
            .selection_toggle_states_for_active_view(crate::config_panel::EFFECTS_VIEW_ID)
            .unwrap_or_default();
        self.refresh_config_panel_with_effects_state(notice, toggle_states)
    }

    fn refresh_config_panel_with_effects_state(
        &mut self,
        notice: Option<String>,
        effects_toggle_states: Vec<Option<crate::bottom_pane::SelectionToggleState>>,
    ) -> bool {
        let panel_updated = self.bottom_pane.update_config_panel(&ConfigPanelUpdate {
            tui: self.config_panel_tui.clone(),
            keymap: self.bottom_pane.list_keymap(),
            status: self.config_panel_status(),
            notice: notice.clone(),
        });
        let Some(selected_idx) = self
            .bottom_pane
            .selected_index_for_active_view(crate::config_panel::EFFECTS_VIEW_ID)
        else {
            return panel_updated;
        };
        let keymap = self.bottom_pane.list_keymap();
        let mut params = crate::config_panel::effects_with_toggle_states(
            &self.config_panel_tui,
            &keymap,
            &effects_toggle_states,
        );
        params.initial_selected_idx = Some(selected_idx);
        params.subtitle = notice.or(params.subtitle);
        let effects_updated = self
            .bottom_pane
            .replace_selection_view_if_active(crate::config_panel::EFFECTS_VIEW_ID, params);
        panel_updated || effects_updated
    }

    fn config_panel_status(&self) -> ConfigPanelStatus {
        ConfigPanelStatus {
            transcript_mode: self.local_settings.transcript_mode,
            copy_on_select: self
                .local_settings
                .copy_on_select(&codex_terminal_detection::terminal_info()),
            right_click_paste: crate::app::PasteEnvironment::detect().status(
                self.local_settings.tui.right_click_paste,
                self.local_settings.transcript_mode,
            ),
            custom_notification_filters: self.local_settings.custom_notification_filters,
        }
    }

    pub(crate) fn report_config_save_error(&mut self, message: String) {
        self.add_error_message(message.clone());
        self.refresh_config_panel(Some(message));
    }

    pub(crate) fn config_editor_open_failed(&mut self, message: String) {
        self.bottom_pane.config_editor_opened();
        self.add_error_message(message.clone());
        self.refresh_config_panel(Some(message));
    }
}
