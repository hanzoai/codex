pub mod cache;
pub mod collaboration_mode_presets;
pub(crate) mod config;
pub mod manager;
pub mod model_info;
pub mod model_presets;
pub mod test_support;

pub use codex_protocol::auth::AuthMode;
pub use config::ModelsManagerConfig;

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
    let default = hanzo::DEFAULT_MODEL;
    let served = hanzo::catalog::MODELS
        .iter()
        .map(|model| model.slug)
        .filter(|slug| !slug.contains('/') && *slug != default);
    let mut models: Vec<_> = std::iter::once(default)
        .chain(served)
        .enumerate()
        .map(|(index, slug)| {
            let mut info = model_info::model_info_from_slug(slug);
            info.visibility = ModelVisibility::List;
            info.priority = index as i32;
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
