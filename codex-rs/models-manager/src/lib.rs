pub mod cache;
mod catalog;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
mod originator;
pub mod test_support;

pub use catalog::CatalogModel;
pub use catalog::ModelOrigin;
pub use codex_protocol::auth::AuthMode;
pub use config::ModelsManagerConfig;
pub use originator::ModelCatalogOriginator;

/// Load the bundled model catalog shipped with `codex-models-manager`.
pub fn bundled_models_response()
-> std::result::Result<codex_protocol::openai_models::ModelsResponse, serde_json::Error> {
    use codex_protocol::openai_models::ModelVisibility;
    let mut response: codex_protocol::openai_models::ModelsResponse =
        serde_json::from_str(include_str!("../models.json"))?;
    // Hanzo: the Hanzo catalog leads the picker, with the default first. The bundled
    // OpenAI models stay known, for lookups, but unlisted.
    for model in &mut response.models {
        model.visibility = ModelVisibility::Hide;
    }
    let mut models: Vec<_> = hanzo::catalog::MODELS
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let mut info = model_info::model_info_from_slug(model.slug);
            info.visibility = if model.listed {
                ModelVisibility::List
            } else {
                ModelVisibility::Hide
            };
            info.priority = if model.listed {
                hanzo::catalog::picker_priority(model.slug)
            } else {
                50 + index as i32
            };
            info
        })
        .collect();
    models.append(&mut response.models);
    response.models = models;
    Ok(response)
}

/// Convert the client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3").
pub fn client_version_to_whole() -> String {
    let version = hanzo::version::get();
    version.split(['-', '+']).next().unwrap_or(version).to_string()
}
