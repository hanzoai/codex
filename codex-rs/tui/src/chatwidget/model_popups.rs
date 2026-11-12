//! Model, collaboration, and reasoning popups for `ChatWidget`.
//!
//! These surfaces are tightly related because changing one often redirects
//! into another, especially while Plan mode is active.

use super::*;
use crate::bottom_pane::SearchMode;
use crate::model_catalog::LUNA_RESERVE_MODEL;

const ULTRA_REASONING_CONCURRENCY_WARNING_THRESHOLD: usize = 8;
pub(super) const MODEL_OPTIONS_VIEW_ID: &str = "model-options";
pub(super) const MODEL_LOADING_VIEW_ID: &str = "model-loading";
pub(super) const MODEL_SELECTION_VIEW_ID: &str = "model-selection";
pub(super) const ALL_MODELS_SELECTION_VIEW_ID: &str = "all-models-selection";

impl ChatWidget {
    pub(crate) fn set_active_model_provider(&mut self, provider_id: &str) {
        self.config.model_provider_id = provider_id.to_string();
        if let Some(provider) = self.config.model_providers.get(provider_id).cloned() {
            self.config.model_provider = provider;
        } else if provider_id == "local" {
            self.config.model_provider = codex_model_provider_info::ModelProviderInfo {
                name: "Hanzo Engine (local)".to_string(),
                base_url: Some("http://127.0.0.1:1234/v1".to_string()),
                wire_api: codex_model_provider_info::WireApi::Responses,
                requires_openai_auth: false,
                supports_websockets: false,
                ..Default::default()
            };
        }
    }

    pub(crate) fn refresh_models_after_provider_switch(&mut self) {
        let request_id = uuid::Uuid::new_v4();
        self.model_popup_request_id = Some(request_id);
        self.app_event_tx.send(AppEvent::FetchModels { request_id });
    }

    pub(super) fn qualified_picker_model(&self, preset: &ModelPreset) -> String {
        match preset.model_provider.as_deref() {
            Some(provider) if provider != self.config.model_provider_id => {
                format!("{provider}::{}", preset.model)
            }
            _ => preset.model.clone(),
        }
    }

    fn is_current_picker_preset(&self, preset: &ModelPreset) -> bool {
        preset.model.as_str() == self.current_model()
            && preset
                .model_provider
                .as_deref()
                .is_none_or(|provider| provider == self.config.model_provider_id)
    }

    fn prepare_picker_presets(&self, presets: Vec<ModelPreset>) -> (Vec<ModelPreset>, bool) {
        let mut presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker)
            .collect();

        let distinct_providers = presets
            .iter()
            .filter_map(|preset| preset.model_provider.as_deref())
            .collect::<std::collections::HashSet<_>>()
            .len();
        let is_hanzo_context =
            crate::app_server_session::models::is_hanzo_ecosystem_config(&self.config)
                || presets.iter().any(|preset| {
                    matches!(preset.model_provider.as_deref(), Some("hanzo" | "local"))
                });

        if is_hanzo_context {
            let has_first_party = presets
                .iter()
                .any(|preset| hanzo::catalog::is_first_party(&preset.model));
            for preset in &mut presets {
                if preset.model_provider.is_none() && has_first_party {
                    let provider = if hanzo::catalog::is_local_only(&preset.model) {
                        "local"
                    } else {
                        self.config.model_provider_id.as_str()
                    };
                    preset.model_provider = Some(provider.to_string());
                }
            }
            if has_first_party
                && !presets
                    .iter()
                    .any(|preset| preset.model == "zen6-nano")
            {
                presets.push(crate::app_server_session::models::local_zen6_nano_preset());
            }
            if has_first_party && !self.local_settings.tui.show_third_party_models {
                let current_model = self.current_model();
                presets.retain(|preset| {
                    let is_hanzo_cloud = match preset.model_provider.as_deref() {
                        Some("hanzo") => true,
                        None => self.config.model_provider_id == "hanzo",
                        Some(_) => false,
                    };
                    !is_hanzo_cloud
                        || hanzo::catalog::is_first_party(&preset.model)
                        || Self::is_auto_model(&preset.model)
                        || preset.model == current_model
                });
            }
            if has_first_party || distinct_providers > 1 {
                crate::app_server_session::models::sort_aggregated_models(&mut presets);
            }
            return (presets, has_first_party);
        }

        if distinct_providers > 1 {
            crate::app_server_session::models::sort_aggregated_models(&mut presets);
        }

        (presets, false)
    }

    /// Open a popup to choose a quick auto model. Selecting "All models"
    /// opens the full picker with every available preset.
    pub(crate) fn open_model_popup(&mut self) {
        if !self.is_session_configured() {
            self.add_info_message(
                "Model selection is disabled until startup completes.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        if let Some(message) = self.model_picker_unavailable() {
            self.add_info_message(message, /*hint*/ None);
            return;
        }

        let presets: Vec<ModelPreset> = match self.model_catalog.try_list_models() {
            Ok(models) => models,
            Err(_) => {
                self.add_info_message(
                    "Models are being updated; please try /model again in a moment.".to_string(),
                    /*hint*/ None,
                );
                return;
            }
        };
        let request_id = uuid::Uuid::new_v4();
        self.model_popup_request_id = Some(request_id);
        if presets.is_empty() {
            self.bottom_pane.show_selection_view(SelectionViewParams {
                view_id: Some(MODEL_LOADING_VIEW_ID),
                title: Some("Loading models…".into()),
                ..SelectionViewParams::picker()
            });
        } else {
            self.open_model_popup_with_presets(presets);
        }
        // Show cached choices immediately and update any still-present picker when the reply arrives.
        self.app_event_tx.send(AppEvent::FetchModels { request_id });
    }

    pub(super) fn model_menu_header(&self, title: &str, subtitle: &str) -> Box<dyn Renderable> {
        let title = title.to_string();
        let subtitle = subtitle.to_string();
        let mut header = vec![Line::from(title.bold())];
        if !subtitle.is_empty() {
            header.push(Line::from(subtitle.dim()));
        }
        if let Some(selection) = &self.cloud_picker_selection {
            let effort = selection
                .effort
                .as_ref()
                .map_or_else(|| "server effort".to_string(), Self::reasoning_effort_label);
            header.push(
                format!("Accepted for future turns: {} · {effort}", selection.model)
                    .dim()
                    .into(),
            );
        }
        if let Some(warning) = self.model_menu_warning_line() {
            header.push(warning);
        }
        Box::new(Paragraph::new(header).wrap(Wrap { trim: false }))
    }

    fn model_menu_warning_line(&self) -> Option<Line<'static>> {
        let base_url = self.custom_openai_base_url()?;
        let warning = format!(
            "Warning: OpenAI base URL is overridden to {base_url}. Selecting models may not be supported or work properly."
        );
        Some(Line::from(warning.red()))
    }

    fn custom_openai_base_url(&self) -> Option<String> {
        if !self.config.model_provider.is_openai() {
            return None;
        }

        let base_url = self.config.model_provider.base_url.as_ref()?;
        let trimmed = base_url.trim();
        if trimmed.is_empty() {
            return None;
        }

        let normalized = trimmed.trim_end_matches('/');
        if normalized == DEFAULT_OPENAI_BASE_URL || hanzo::is_hanzo(Some(trimmed)) {
            return None;
        }

        Some(trimmed.to_string())
    }

    pub(crate) fn open_model_popup_with_presets(&mut self, presets: Vec<ModelPreset>) {
        if self.restrict_model_picker_to_luna_reserve() {
            self.open_luna_reserve_model_popup(presets, MODEL_SELECTION_VIEW_ID);
            return;
        }
        let presets: Vec<ModelPreset> = presets
            .into_iter()
            .filter(|preset| preset.show_in_picker && !Self::is_auto_model(&preset.model))
            .collect();

        self.open_all_models_popup_with_view_id(presets, MODEL_SELECTION_VIEW_ID);
    }

    pub(super) fn is_auto_model(model: &str) -> bool {
        model.starts_with("codex-auto-")
    }

    fn auto_model_order(model: &str) -> usize {
        match model {
            "codex-auto-fast" => 0,
            "codex-auto-balanced" => 1,
            "codex-auto-thorough" => 2,
            _ => 3,
        }
    }

    pub(crate) fn open_all_models_popup(&mut self) {
        if self.restrict_model_picker_to_luna_reserve() {
            self.open_luna_reserve_model_popup(
                self.model_catalog.try_list_models().unwrap_or_default(),
                ALL_MODELS_SELECTION_VIEW_ID,
            );
            return;
        }
        let presets = self
            .model_catalog
            .try_list_models()
            .unwrap_or_default()
            .into_iter()
            .filter(|preset| preset.show_in_picker && !Self::is_auto_model(&preset.model))
            .collect();
        self.open_all_models_popup_with_view_id(presets, ALL_MODELS_SELECTION_VIEW_ID);
    }

    fn open_all_models_popup_with_view_id(
        &mut self,
        presets: Vec<ModelPreset>,
        view_id: &'static str,
    ) {
        let (presets, is_hanzo_context) = self.prepare_picker_presets(presets);
        if presets.is_empty() {
            self.bottom_pane.dismiss_view_by_id(view_id);
            self.add_info_message(
                "No additional models are available right now.".to_string(),
                /*hint*/ None,
            );
            return;
        }

        let _ = is_hanzo_context;
        let active_effort = self.effective_reasoning_effort();
        let model_ids: Vec<String> = presets
            .iter()
            .map(|preset| self.qualified_picker_model(preset))
            .collect();
        let default_efforts = crate::bottom_pane::ModelSwitcherView::default_effort_options();

        let items: Vec<crate::bottom_pane::ModelSwitcherItem> = presets
            .into_iter()
            .map(|preset| {
                let is_current = self.is_current_picker_preset(&preset);
                let qualified_model = self.qualified_picker_model(&preset);
                let target_effort = if is_current {
                    active_effort
                        .clone()
                        .unwrap_or(preset.default_reasoning_effort.clone())
                } else {
                    preset.default_reasoning_effort.clone()
                };
                let initial_effort_idx = match target_effort {
                    ReasoningEffortConfig::Low | ReasoningEffortConfig::Minimal => 0,
                    ReasoningEffortConfig::Medium => 1,
                    _ => 2,
                };
                let actions_low = self.model_selection_actions(
                    qualified_model.clone(),
                    Some(ReasoningEffortConfig::Low),
                    self.should_prompt_plan_mode_reasoning_scope(
                        qualified_model.as_str(),
                        Some(ReasoningEffortConfig::Low),
                    ),
                );
                let actions_medium = self.model_selection_actions(
                    qualified_model.clone(),
                    Some(ReasoningEffortConfig::Medium),
                    self.should_prompt_plan_mode_reasoning_scope(
                        qualified_model.as_str(),
                        Some(ReasoningEffortConfig::Medium),
                    ),
                );
                let actions_high = self.model_selection_actions(
                    qualified_model.clone(),
                    Some(ReasoningEffortConfig::High),
                    self.should_prompt_plan_mode_reasoning_scope(
                        qualified_model.as_str(),
                        Some(ReasoningEffortConfig::High),
                    ),
                );

                let on_select: std::sync::Arc<
                    dyn Fn(&crate::app_event_sender::AppEventSender, ReasoningEffortConfig)
                        + Send
                        + Sync,
                > = std::sync::Arc::new(move |tx, effort| {
                    let actions = match effort {
                        ReasoningEffortConfig::Low | ReasoningEffortConfig::Minimal => &actions_low,
                        ReasoningEffortConfig::Medium => &actions_medium,
                        _ => &actions_high,
                    };
                    for action in actions {
                        action(tx);
                    }
                });

                crate::bottom_pane::ModelSwitcherItem {
                    model: qualified_model,
                    display_name: preset.display_name,
                    is_current,
                    efforts: default_efforts.clone(),
                    initial_effort_idx,
                    on_select,
                }
            })
            .collect();

        let existing_index = self.bottom_pane.selected_index_for_present_view(view_id);
        let existing_query = self.bottom_pane.search_query_for_present_view(view_id);
        let selected_model = existing_index.and_then(|idx| self.model_popup_model_ids.get(idx));
        let initial_selected_idx = model_ids
            .iter()
            .position(|model| Some(model) == selected_model);
        self.model_popup_model_ids = model_ids;

        let view = crate::bottom_pane::ModelSwitcherView::new(
            Some(view_id),
            items,
            initial_selected_idx,
            existing_query,
            self.app_event_tx.clone(),
        );
        if existing_index.is_some() {
            self.bottom_pane
                .replace_view_if_present(view_id, Box::new(view));
        } else {
            self.bottom_pane.show_view(Box::new(view));
        }
    }

    fn model_selection_actions(
        &self,
        model_for_action: String,
        effort_for_action: Option<ReasoningEffortConfig>,
        should_prompt_plan_mode_scope: bool,
    ) -> Vec<SelectionAction> {
        let warning = effort_for_action
            .as_ref()
            .and_then(|effort| self.ultra_reasoning_concurrency_warning(effort));
        let thread_id = self.thread_id();
        let sparkle_thread = self.sparkle_thread_for_picker_action(&model_for_action);
        let scoped = self.scope_model_picker_event();
        vec![Box::new(move |tx| {
            if model_for_action == LUNA_RESERVE_MODEL {
                // Reserve is temporary: update the active task without persisting a model default.
                if let Some(thread_id) = thread_id {
                    tx.send(scoped(AppEvent::UpdateLunaReserveReasoning {
                        thread_id,
                        effort: effort_for_action.clone(),
                    }));
                }
            } else if effort_for_action == Some(ReasoningEffortConfig::Ultra) {
                tx.send(scoped(
                    AstraModelPickerAction::ApplyAdvancedReasoning {
                        effort: ReasoningEffortConfig::Ultra,
                    }
                    .into_picker_event(sparkle_thread, model_for_action.clone()),
                ));
            } else if should_prompt_plan_mode_scope {
                tx.send(scoped(AppEvent::OpenPlanReasoningScopePrompt {
                    model: model_for_action.clone(),
                    effort: effort_for_action.clone(),
                }));
            } else {
                tx.send(scoped(
                    AstraModelPickerAction::ApplyModelSelection {
                        effort: effort_for_action.clone(),
                    }
                    .into_picker_event(sparkle_thread, model_for_action.clone()),
                ));
            }
            if let Some(warning) = warning.clone() {
                tx.send(scoped(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_warning_event(warning),
                ))));
            }
        })]
    }

    fn should_prompt_plan_mode_reasoning_scope(
        &self,
        selected_model: &str,
        selected_effort: Option<ReasoningEffortConfig>,
    ) -> bool {
        if !self.collaboration_modes_enabled()
            || selected_model == LUNA_RESERVE_MODEL
            || self.active_mode_kind() != ModeKind::Plan
            || selected_model != self.current_model()
        {
            return false;
        }

        // Prompt whenever the selection is not a true no-op for both:
        // 1) the active Plan-mode effective reasoning, and
        // 2) the stored global defaults that would be updated by the fallback path.
        selected_effort != self.effective_reasoning_effort()
            || selected_model != self.current_collaboration_mode.model()
            || selected_effort != self.current_collaboration_mode.reasoning_effort()
    }

    pub(crate) fn open_plan_reasoning_scope_prompt(
        &mut self,
        model: String,
        effort: Option<ReasoningEffortConfig>,
    ) {
        let reasoning_phrase = match effort.as_ref() {
            Some(ReasoningEffortConfig::None) => "no reasoning".to_string(),
            Some(selected_effort) => {
                format!(
                    "{} reasoning",
                    Self::reasoning_effort_sentence_label(selected_effort)
                )
            }
            None => "the selected reasoning".to_string(),
        };
        let plan_only_description = format!("Always use {reasoning_phrase} in Plan mode");
        let plan_reasoning_source = if let Some(plan_override) =
            self.config.plan_mode_reasoning_effort.as_ref()
        {
            format!(
                "user-chosen Plan override ({})",
                Self::reasoning_effort_sentence_label(plan_override)
            )
        } else if let Some(plan_mask) = collaboration_modes::plan_mask(self.model_catalog.as_ref())
        {
            match plan_mask
                .reasoning_effort
                .as_ref()
                .and_then(|effort| effort.as_ref())
            {
                Some(plan_effort) => format!(
                    "built-in Plan default ({})",
                    Self::reasoning_effort_sentence_label(plan_effort)
                ),
                None => "built-in Plan default (no reasoning)".to_string(),
            }
        } else {
            "built-in Plan default".to_string()
        };
        let all_modes_description = format!(
            "Set the global default reasoning level and Plan mode override, replacing the current {plan_reasoning_source}"
        );
        let subtitle = format!("Choose where to apply {reasoning_phrase}.");
        let warning = effort
            .as_ref()
            .and_then(|effort| self.ultra_reasoning_concurrency_warning(effort));
        let sparkle_thread = self.sparkle_thread_for_picker_action(&model);

        let scoped = self.scope_model_picker_event();
        let plan_only_actions: Vec<SelectionAction> = vec![Box::new({
            let model = model.clone();
            let effort = effort.clone();
            let warning = warning.clone();
            move |tx| {
                tx.send(scoped(
                    AstraModelPickerAction::UpdateModel
                        .into_picker_event(sparkle_thread, model.clone()),
                ));
                tx.send(scoped(AppEvent::UpdatePlanModeReasoningEffort(
                    effort.clone(),
                )));
                tx.send(scoped(AppEvent::PersistPlanModeReasoningEffort(
                    effort.clone(),
                )));
                if let Some(warning) = warning.clone() {
                    tx.send(scoped(AppEvent::InsertHistoryCell(Box::new(
                        history_cell::new_warning_event(warning),
                    ))));
                }
            }
        })];
        let scoped = self.scope_model_picker_event();
        let all_modes_actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
            tx.send(scoped(
                AstraModelPickerAction::UpdateModel
                    .into_picker_event(sparkle_thread, model.clone()),
            ));
            tx.send(scoped(AppEvent::UpdateReasoningEffort(effort.clone())));
            tx.send(scoped(AppEvent::UpdatePlanModeReasoningEffort(
                effort.clone(),
            )));
            tx.send(scoped(AppEvent::PersistPlanModeReasoningEffort(
                effort.clone(),
            )));
            tx.send(scoped(AppEvent::PersistModelSelection {
                model: model.clone(),
                effort: effort.clone(),
            }));
            if let Some(warning) = warning.clone() {
                tx.send(scoped(AppEvent::InsertHistoryCell(Box::new(
                    history_cell::new_warning_event(warning),
                ))));
            }
        })];

        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(MODEL_OPTIONS_VIEW_ID),
            title: Some(PLAN_MODE_REASONING_SCOPE_TITLE.to_string()),
            subtitle: Some(subtitle),
            items: vec![
                SelectionItem {
                    name: PLAN_MODE_REASONING_SCOPE_PLAN_ONLY.to_string(),
                    description: Some(plan_only_description),
                    actions: plan_only_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
                SelectionItem {
                    name: PLAN_MODE_REASONING_SCOPE_ALL_MODES.to_string(),
                    description: Some(all_modes_description),
                    actions: all_modes_actions,
                    dismiss_on_select: true,
                    ..Default::default()
                },
            ],
            ..SelectionViewParams::picker()
        });
        self.notify(Notification::PlanModePrompt {
            title: PLAN_MODE_REASONING_SCOPE_TITLE.to_string(),
        });
    }

    /// Open a popup to choose the standard reasoning effort for the given model.
    ///
    /// Max and Ultra require an explicit second step so expensive efforts cannot
    /// be selected accidentally while moving through the normal effort scale.
    pub(crate) fn open_reasoning_popup(&mut self, preset: ModelPreset) {
        let default_effort = preset.default_reasoning_effort.clone();
        let supported = &preset.supported_reasoning_efforts;
        let in_plan_mode =
            self.collaboration_modes_enabled() && self.active_mode_kind() == ModeKind::Plan;

        let warn_effort = if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::XHigh)
        {
            Some(ReasoningEffortConfig::XHigh)
        } else if supported
            .iter()
            .any(|option| option.effort == ReasoningEffortConfig::High)
        {
            Some(ReasoningEffortConfig::High)
        } else {
            None
        };
        let warning_text = warn_effort.as_ref().map(|effort| {
            let effort_label = Self::reasoning_effort_label(effort);
            format!("⚠ {effort_label} reasoning effort can quickly consume Plus plan rate limits")
        });
        let warn_for_model = preset.model.starts_with("gpt-5.1-codex")
            || preset.model.starts_with("gpt-5.1-codex-max")
            || preset.model.starts_with("gpt-5.2");

        let mut all_choices: Vec<ReasoningEffortConfig> = supported
            .iter()
            .map(|option| option.effort.clone())
            .collect();
        if all_choices.is_empty() {
            all_choices.push(default_effort.clone());
        }
        let (choices, advanced_choices): (Vec<_>, Vec<_>) = all_choices
            .into_iter()
            .partition(|effort| !Self::is_advanced_reasoning_effort(effort));

        if choices.len() == 1 && advanced_choices.is_empty() {
            let selected_effort = choices.first().cloned();
            let selected_model = self.qualified_picker_model(&preset);
            if self
                .should_prompt_plan_mode_reasoning_scope(&selected_model, selected_effort.clone())
            {
                let scoped = self.scope_model_picker_event();
                self.app_event_tx
                    .send(scoped(AppEvent::OpenPlanReasoningScopePrompt {
                        model: selected_model,
                        effort: selected_effort,
                    }));
            } else {
                self.apply_model_and_effort(selected_model, selected_effort);
            }
            return;
        }

        let default_choice = choices
            .contains(&default_effort)
            .then(|| default_effort.clone());

        let model_slug = self.qualified_picker_model(&preset);
        let model_label = preset.display_name.clone();
        let is_current_model = self.is_current_picker_preset(&preset);
        let highlight_choice = if is_current_model {
            if in_plan_mode {
                self.config
                    .plan_mode_reasoning_effort
                    .clone()
                    .or_else(|| self.effective_reasoning_effort())
            } else {
                self.effective_reasoning_effort()
            }
        } else {
            default_choice.clone().or_else(|| choices.first().cloned())
        };
        let selection_choice = highlight_choice.clone().or_else(|| default_choice.clone());
        let initial_selected_idx = choices
            .iter()
            .position(|choice| Some(choice) == selection_choice.as_ref());
        let mut items: Vec<SelectionItem> = Vec::new();
        for choice in choices.iter() {
            let effort = choice.clone();
            let mut effort_label = Self::reasoning_effort_label(&effort);
            if Some(choice) == default_choice.as_ref() {
                effort_label.push_str(" (default)");
            }

            let description = supported
                .iter()
                .find(|option| option.effort == effort)
                .map(|option| option.description.to_string())
                .filter(|text| !text.is_empty());

            let show_warning = warn_for_model && warn_effort.as_ref() == Some(&effort);
            let selected_description = if show_warning {
                warning_text.as_ref().map(|warning_message| {
                    description.as_ref().map_or_else(
                        || warning_message.clone(),
                        |d| format!("{d}\n{warning_message}"),
                    )
                })
            } else {
                None
            };

            let choice_effort = Some(effort);
            let should_prompt_plan_mode_scope = self.should_prompt_plan_mode_reasoning_scope(
                model_slug.as_str(),
                choice_effort.clone(),
            );
            let actions = self.model_selection_actions(
                model_slug.clone(),
                choice_effort.clone(),
                should_prompt_plan_mode_scope,
            );

            items.push(SelectionItem {
                name: effort_label,
                description,
                selected_description,
                is_current: is_current_model && Some(choice) == highlight_choice.as_ref(),
                secondary_action: self
                    .session_model_selection_action(model_slug.clone(), choice_effort),
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        if !advanced_choices.is_empty() {
            let advanced_label = advanced_choices
                .iter()
                .map(Self::reasoning_effort_label)
                .collect::<Vec<_>>()
                .join(" and ");
            let verb = if advanced_choices.len() == 1 {
                "consumes"
            } else {
                "consume"
            };
            let preset_for_action = preset;
            let scoped = self.scope_model_picker_event();
            let actions: Vec<SelectionAction> = vec![Box::new(move |tx| {
                tx.send(scoped(AppEvent::OpenAdvancedReasoningPopup {
                    model: preset_for_action.clone(),
                }));
            })];
            items.push(SelectionItem {
                name: "More reasoning…".to_string(),
                description: Some(format!("{advanced_label} {verb} usage limits faster")),
                is_current: is_current_model
                    && highlight_choice
                        .as_ref()
                        .is_some_and(Self::is_advanced_reasoning_effort),
                actions,
                dismiss_parent_on_child_accept: true,
                ..Default::default()
            });
        }

        let header = Paragraph::new(Line::from(
            format!("Select Reasoning Level for {model_label}").bold(),
        ))
        .wrap(Wrap { trim: false });

        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(MODEL_OPTIONS_VIEW_ID),
            header: Box::new(header),
            items,
            initial_selected_idx,
            ..SelectionViewParams::picker()
        });
    }

    /// Open the explicit Max/Ultra effort picker for the given model.
    pub(crate) fn open_advanced_reasoning_popup(&mut self, preset: ModelPreset) {
        let mut choices = preset
            .supported_reasoning_efforts
            .iter()
            .map(|option| option.effort.clone())
            .filter(Self::is_advanced_reasoning_effort)
            .collect::<Vec<_>>();
        if choices.is_empty()
            && Self::is_advanced_reasoning_effort(&preset.default_reasoning_effort)
        {
            choices.push(preset.default_reasoning_effort.clone());
        }
        choices.sort_by_key(|effort| matches!(effort, ReasoningEffortConfig::Ultra));
        if choices.is_empty() {
            return;
        }

        let model_slug = self.qualified_picker_model(&preset);
        let is_current_model = self.is_current_picker_preset(&preset);
        let highlight_choice = is_current_model
            .then(|| self.effective_reasoning_effort())
            .flatten();
        let mut items = Vec::new();
        for effort in choices {
            let description = match &effort {
                ReasoningEffortConfig::Max => {
                    "For difficult problems when quality matters more than speed · higher usage"
                }
                ReasoningEffortConfig::Ultra => {
                    "For demanding work using multiple agents · highest usage"
                }
                _ => unreachable!("advanced choices are limited to Max and Ultra"),
            };
            let should_prompt_plan_mode_scope = self
                .should_prompt_plan_mode_reasoning_scope(model_slug.as_str(), Some(effort.clone()));
            let actions = self.model_selection_actions(
                model_slug.clone(),
                Some(effort.clone()),
                should_prompt_plan_mode_scope,
            );

            items.push(SelectionItem {
                name: Self::reasoning_effort_label(&effort),
                description: Some(description.to_string()),
                is_current: is_current_model && Some(&effort) == highlight_choice.as_ref(),
                secondary_action: self
                    .session_model_selection_action(model_slug.clone(), Some(effort.clone())),
                actions,
                dismiss_on_select: true,
                ..Default::default()
            });
        }

        let header = Paragraph::new(vec![
            Line::from("Advanced Reasoning".bold()),
            Line::from("⚠ Consumes usage limits faster".cyan()),
        ])
        .wrap(Wrap { trim: false });
        self.bottom_pane.show_selection_view(SelectionViewParams {
            view_id: Some(MODEL_OPTIONS_VIEW_ID),
            header: Box::new(header),
            items,
            ..SelectionViewParams::picker()
        });
    }

    pub(super) fn is_advanced_reasoning_effort(effort: &ReasoningEffortConfig) -> bool {
        matches!(
            effort,
            ReasoningEffortConfig::Max | ReasoningEffortConfig::Ultra
        )
    }

    pub(super) fn reasoning_effort_label(effort: &ReasoningEffortConfig) -> String {
        match effort {
            ReasoningEffortConfig::None => "None".to_string(),
            ReasoningEffortConfig::Minimal => "Minimal".to_string(),
            ReasoningEffortConfig::Low => "Low".to_string(),
            ReasoningEffortConfig::Medium => "Medium".to_string(),
            ReasoningEffortConfig::High => "High".to_string(),
            // Hanzo: upstream's xhigh is the table's `max`.
            ReasoningEffortConfig::XHigh => capitalized(hanzo::effort::shown("xhigh")),
            ReasoningEffortConfig::Max => "Max".to_string(),
            ReasoningEffortConfig::Ultra => "Ultra".to_string(),
            ReasoningEffortConfig::Persistent => "Persistent".to_string(),
            ReasoningEffortConfig::Custom(value) => capitalized(value),
        }
    }

    pub(super) fn reasoning_effort_sentence_label(effort: &ReasoningEffortConfig) -> String {
        match effort {
            ReasoningEffortConfig::Custom(value) => value.clone(),
            effort => Self::reasoning_effort_label(effort).to_lowercase(),
        }
    }

    pub(super) fn ultra_reasoning_concurrency_warning(
        &self,
        effort: &ReasoningEffortConfig,
    ) -> Option<String> {
        if effort != &ReasoningEffortConfig::Ultra {
            return None;
        }

        let max_threads = self
            .config
            .multi_agent_v2
            .max_concurrent_threads_per_session;
        if max_threads < ULTRA_REASONING_CONCURRENCY_WARNING_THRESHOLD {
            return None;
        }

        let max_subagents = max_threads.saturating_sub(1);
        Some(format!(
            "Ultra reasoning may proactively use multiple agents. This session is configured for \
             {max_threads} concurrent threads with up to {max_subagents} subagents which can \
             increase usage quickly. Consider setting \
             features.multi_agent_v2.max_concurrent_threads_per_session below 8."
        ))
    }

    fn apply_model_and_effort(&self, model: String, effort: Option<ReasoningEffortConfig>) {
        for action in self
            .model_selection_actions(model, effort, /*should_prompt_plan_mode_scope*/ false)
        {
            action(&self.app_event_tx);
        }
    }
}

/// Hanzo: an effort's name as a label (`auto` is "Auto"), as upstream's built-in names read.
fn capitalized(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}
