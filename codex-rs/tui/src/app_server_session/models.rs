//! Fetch picker models without blocking the event loop and keep new-thread defaults in sync.

use super::AppServerSession;
use super::ThreadParamsMode;
use super::model_preset_from_api_model;
use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::legacy_core::config::Config;
use codex_app_server_client::AppServerRequestHandle;
use codex_app_server_client::TypedRequestError;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::ModelListParams;
use codex_app_server_protocol::ModelListResponse;
use codex_app_server_protocol::RequestId;
use codex_protocol::openai_models::ModelPreset;
use codex_protocol::openai_models::ModelVisibility;
use std::collections::HashSet;
use std::time::Duration;
use uuid::Uuid;

const EXTRA_PROVIDER_FETCH_TIMEOUT: Duration = Duration::from_millis(1200);

pub(crate) fn local_zen6_nano_preset() -> ModelPreset {
    let mut info = codex_models_manager::model_info::model_info_from_slug("zen6-nano");
    info.visibility = ModelVisibility::List;
    let mut preset = ModelPreset::from(info);
    preset.supported_reasoning_efforts.clear();
    preset.model_provider = Some("local".to_string());
    preset.is_default = false;
    preset
}

pub(crate) fn is_hanzo_ecosystem_config(config: &Config) -> bool {
    #[cfg(not(test))]
    let env_has_hanzo = std::env::var_os("HANZO_API_KEY").is_some();
    #[cfg(test)]
    let env_has_hanzo = false;

    config.model_provider_id == "hanzo"
        || config.model_provider_id == "local"
        || hanzo::is_hanzo(config.model_provider.base_url.as_deref())
        || env_has_hanzo
        || config
            .config_layer_stack
            .effective_config()
            .get("model_providers")
            .and_then(toml::Value::as_table)
            .is_some_and(|table| table.contains_key("hanzo"))
}

pub(crate) fn configured_coding_providers(config: &Config) -> Vec<String> {
    let mut providers = vec![config.model_provider_id.clone()];
    let memory_provider = config.memories.model_provider.as_deref();
    let mut push_unique = |id: &str| {
        if id == "local_memory" || Some(id) == memory_provider {
            return;
        }
        if !providers.iter().any(|existing| existing == id) {
            providers.push(id.to_string());
        }
    };

    if is_hanzo_ecosystem_config(config) {
        if config.model_providers.contains_key("hanzo") {
            push_unique("hanzo");
        }
        push_unique("local");
    }

    if let Some(table) = config
        .config_layer_stack
        .effective_config()
        .get("model_providers")
        .and_then(toml::Value::as_table)
    {
        let mut keys: Vec<&str> = table.keys().map(String::as_str).collect();
        keys.sort_unstable();
        for key in keys {
            push_unique(key);
        }
    }

    providers
}

pub(crate) fn sort_aggregated_models(models: &mut [ModelPreset]) {
    models.sort_by(|left, right| {
        sort_key_for_preset(left).cmp(&sort_key_for_preset(right))
    });
}

fn sort_key_for_preset(preset: &ModelPreset) -> (u8, i32, String, String, String) {
    let provider = preset.model_provider.as_deref().unwrap_or("");
    let is_hanzo_or_local = provider.is_empty() || provider == "hanzo" || provider == "local";
    let group = if (provider.is_empty() || provider == "hanzo")
        && hanzo::catalog::is_enso(&preset.model)
    {
        0
    } else if is_hanzo_or_local && hanzo::catalog::is_first_party(&preset.model) {
        1
    } else if !provider.is_empty() && provider != "hanzo" {
        2
    } else {
        3
    };
    let priority = if group <= 1 || group == 3 {
        hanzo::catalog::picker_priority(&preset.model)
    } else {
        i32::from(preset.upgrade.is_some())
    };
    let provider_rank = if group == 2 {
        provider.to_ascii_lowercase()
    } else {
        String::new()
    };
    (
        group,
        priority,
        provider_rank,
        preset.display_name.to_ascii_lowercase(),
        preset.model.to_ascii_lowercase(),
    )
}

fn normalize_provider_tag(provider_id: &str, preset: &mut ModelPreset) {
    let effective_provider = if provider_id == "hanzo" && hanzo::catalog::is_local_only(&preset.model)
    {
        "local"
    } else {
        provider_id
    };
    preset.model_provider = Some(effective_provider.to_string());
}

impl AppServerSession {
    pub(crate) fn set_available_models(&mut self, models: Vec<ModelPreset>) {
        if let Some(default) = models
            .iter()
            .find(|model| model.is_default)
            .or(models.first())
        {
            self.default_model = Some(default.model.clone());
        }
        self.available_models = models;
    }

    pub(crate) fn fetch_models(&self, request_id: Uuid, app_event_tx: AppEventSender) {
        let models = list(
            self.thread_params_mode,
            self.request_handle(),
            RequestId::String(format!("model-list-{request_id}")),
        );
        tokio::spawn(async move {
            let result = models.await.map_err(|err| err.to_string());
            app_event_tx.send(AppEvent::ModelsLoaded { request_id, result });
        });
    }

    pub(crate) fn fetch_models_for_config(
        &self,
        request_id: Uuid,
        config: &Config,
        app_event_tx: AppEventSender,
    ) {
        let hanzo_ecosystem = is_hanzo_ecosystem_config(config);
        let providers = configured_coding_providers(config);
        if !hanzo_ecosystem && providers.len() <= 1 {
            self.fetch_models(request_id, app_event_tx);
            return;
        }

        let primary_provider = config.model_provider_id.clone();
        let extra_providers: Vec<String> = providers
            .into_iter()
            .filter(|id| id != &primary_provider)
            .collect();
        let mode = self.thread_params_mode;
        let handle = self.request_handle();

        tokio::spawn(async move {
            let primary_req_id = RequestId::String(format!("model-list-{request_id}"));
            let primary_fut = list_for_provider(mode, handle.clone(), primary_req_id, None);
            let extra_futs = extra_providers.into_iter().map(|provider_id| {
                let handle = handle.clone();
                let req_id =
                    RequestId::String(format!("model-list-{request_id}-{provider_id}"));
                async move {
                    let fetched = tokio::time::timeout(
                        EXTRA_PROVIDER_FETCH_TIMEOUT,
                        list_for_provider(mode, handle, req_id, Some(provider_id.clone())),
                    )
                    .await;
                    (provider_id, fetched)
                }
            });

            let (primary_res, extra_results) =
                tokio::join!(primary_fut, futures::future::join_all(extra_futs));

            let mut aggregated = Vec::new();
            let mut primary_err = None;
            match primary_res {
                Ok(mut models) => {
                    for preset in &mut models {
                        normalize_provider_tag(&primary_provider, preset);
                    }
                    aggregated.extend(models);
                }
                Err(err) => {
                    primary_err = Some(err.to_string());
                }
            }

            for (provider_id, result) in extra_results {
                if let Ok(Ok(mut models)) = result {
                    for preset in &mut models {
                        preset.is_default = false;
                        normalize_provider_tag(&provider_id, preset);
                    }
                    aggregated.extend(models);
                }
            }

            if aggregated.is_empty()
                && let Some(err) = primary_err
            {
                app_event_tx.send(AppEvent::ModelsLoaded {
                    request_id,
                    result: Err(err),
                });
                return;
            }

            if hanzo_ecosystem
                && !aggregated.iter().any(|preset| {
                    preset.model == "zen6-nano"
                        && preset.model_provider.as_deref() == Some("local")
                })
            {
                aggregated.push(local_zen6_nano_preset());
            }

            let mut seen = HashSet::new();
            aggregated.retain(|preset| {
                let key = (
                    preset.model_provider.clone().unwrap_or_default(),
                    preset.model.clone(),
                );
                seen.insert(key)
            });

            sort_aggregated_models(&mut aggregated);
            app_event_tx.send(AppEvent::ModelsLoaded {
                request_id,
                result: Ok(aggregated),
            });
        });
    }
}

pub(super) async fn list(
    mode: ThreadParamsMode,
    request_handle: AppServerRequestHandle,
    request_id: RequestId,
) -> Result<Vec<ModelPreset>, TypedRequestError> {
    list_for_provider(mode, request_handle, request_id, None).await
}

pub(super) async fn list_for_provider(
    mode: ThreadParamsMode,
    request_handle: AppServerRequestHandle,
    request_id: RequestId,
    model_provider: Option<String>,
) -> Result<Vec<ModelPreset>, TypedRequestError> {
    let response: ModelListResponse = match request_handle
        .request_typed(ClientRequest::ModelList {
            request_id,
            params: ModelListParams {
                cursor: None,
                limit: None,
                include_hidden: Some(true),
                model_provider,
            },
        })
        .await
    {
        Ok(response) => response,
        // Older Cloud servers do not expose model discovery. Preserve their startup behavior.
        Err(TypedRequestError::Server { source, .. })
            if mode == ThreadParamsMode::Cloud && source.code == -32601 =>
        {
            return Ok(Vec::new());
        }
        Err(error) => return Err(error),
    };
    Ok(response
        .data
        .into_iter()
        .map(model_preset_from_api_model)
        .collect())
}

#[cfg(test)]
#[path = "models_tests.rs"]
mod tests;
