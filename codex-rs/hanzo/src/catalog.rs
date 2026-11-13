//! The chat models api.hanzo.ai serves, as the agent needs to know them.
//!
//! The agent sizes its context, compaction and image input from a model's
//! metadata, and a slug it cannot find runs on a 272K fallback with a warning.
//! These are the chat slugs `GET https://api.hanzo.ai/v1/models` lists; the
//! embedding, rerank, guard and voice models are not chat models and are absent.

/// What the agent needs to know about one served chat model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Model {
    pub slug: &'static str,
    pub name: &'static str,
    /// Context window in tokens.
    pub window: i64,
    /// Accepts images as well as text.
    pub vision: bool,
    /// Listed directly in `/model` (aliases stay false so metadata lookup works without duplicate rows).
    pub listed: bool,
}

const MILLION: i64 = 1_000_000;

const fn model(slug: &'static str, name: &'static str, vision: bool) -> Model {
    Model {
        slug,
        name,
        window: MILLION,
        vision,
        listed: true,
    }
}

const fn alias(slug: &'static str, name: &'static str, vision: bool) -> Model {
    Model {
        slug,
        name,
        window: MILLION,
        vision,
        listed: false,
    }
}

/// Ordered Enso first, then Zen 6 -> Zen 5 -> Zen.
pub const MODELS: &[Model] = &[
    // 1. Hanzo Enso (orchestrated & hybrid)
    model("enso-auto", "Enso Auto", true),
    model("enso", "Enso", true),
    model("enso-pro", "Enso Pro", false),
    model("enso-ultra", "Enso Ultra", true),
    model("enso-max", "Enso Max", true),
    model("enso-flash", "Enso Flash", true),
    model("enso-free", "Enso Free", true),
    model("hybrid/auto", "Hybrid Auto", true),
    // 2. Hanzo Zen 6
    model("zen6", "Zen 6", true),
    model("zen6-max", "Zen 6 Max", true),
    model("zen6-coder", "Zen 6 Coder", true),
    model("zen6-flash", "Zen 6 Flash", true),
    Model {
        slug: "zen6-nano",
        name: "Zen 6 Nano",
        window: 32_768,
        vision: true,
        listed: true,
    },
    // 3. Hanzo Zen 5 & Zen specialized
    model("zen5", "Zen 5", true),
    model("zen5-pro", "Zen 5 Pro", false),
    model("zen5-coder", "Zen 5 Coder", false),
    model("zen5-flash", "Zen 5 Flash", true),
    model("zen5-mini", "Zen 5 Mini", false),
    model("zen5-spark", "Zen 5 Spark", true),
    model("zen5-evo", "Zen 5 Evo", true),
    model("zen-vl", "Zen VL", true),
    model("zen-free", "Zen Free", true),
    // 4. Unlisted aliases & non-coding modalities (kept for metadata/window lookup)
    alias("hanzo/enso", "Enso", true),
    alias("hanzo/hybrid", "Hybrid Auto", true),
    alias("hanzo/zen", "Zen", true),
    alias("hybrid", "Hybrid Auto", true),
    alias("enso-hybrid", "Enso Hybrid", true),
    alias("zen-hybrid", "Zen Hybrid", true),
    alias("zen-ocr", "Zen OCR", true),
    alias("zen-relational", "Zen Relational", false),
    alias("zen-tabular", "Zen Tabular", false),
];

/// Whether `slug` is an on-device local model served by `hanzo-engine` rather than `api.hanzo.ai`.
pub fn is_local_only(slug: &str) -> bool {
    matches!(
        slug.trim().to_ascii_lowercase().as_str(),
        "zen6-nano" | "zenlm/zen6-nano" | "hanzoai/zen6-nano"
    )
}

/// Whether `slug` is a chat/coding model rather than embedding, rerank, guard, audio, 3D, batch, or image/video.
pub fn is_chat_model(slug: &str) -> bool {
    let lower = slug.trim().to_ascii_lowercase();
    if lower.ends_with(":batch") {
        return false;
    }
    ![
        "embed",
        "rerank",
        "moderation",
        "guard",
        "whisper",
        "tts",
        "asr",
        "speech",
        "voice",
        "flux",
        "imagen",
        "image",
        "dall-e",
        "veo",
        "sora",
        "video",
        "music",
        "foley",
        "scribe",
        "director",
        "zen-3d",
        "zen-ocr",
        "zen-relational",
        "zen-tabular",
        "/wan-",
        "/happyhorse-",
    ]
    .iter()
    .any(|blocked| lower.contains(blocked))
}

/// Whether `slug` is a Hanzo Enso / Hybrid orchestrated model.
pub fn is_enso(slug: &str) -> bool {
    let lower = slug.trim().to_ascii_lowercase();
    lower == "enso"
        || lower.starts_with("enso-")
        || lower.starts_with("hanzo/enso")
        || lower == "hybrid"
        || lower.starts_with("hybrid/")
        || lower.starts_with("hanzo/hybrid")
}

/// Whether `slug` is a first-party Hanzo Enso or Zen chat model (shown by default in Hanzo Cloud).
pub fn is_first_party(slug: &str) -> bool {
    let lower = slug.trim().to_ascii_lowercase();
    if !is_chat_model(&lower) {
        return false;
    }
    if find(&lower).is_some() {
        return true;
    }
    is_enso(&lower)
        || lower == "zen"
        || lower.starts_with("zen-")
        || lower.starts_with("zen5")
        || lower.starts_with("zen6")
        || lower.starts_with("hanzo/zen")
        || lower.starts_with("zenlm/")
        || lower.starts_with("hanzoai/")
}

/// Deterministic picker sort priority: Enso (`0..9`) -> Zen (`10..49`) -> Custom/Third-party (`200+`).
pub fn picker_priority(slug: &str) -> i32 {
    let lower = slug.trim().to_ascii_lowercase();
    if let Some(index) = MODELS
        .iter()
        .filter(|m| m.listed)
        .position(|m| m.slug == lower)
    {
        return if is_enso(&lower) {
            index as i32
        } else {
            10 + index as i32
        };
    }
    if is_enso(&lower) {
        return 9;
    }
    if is_first_party(&lower) {
        return 45;
    }
    200
}

/// The served chat model named `slug`, if there is one.
pub fn find(slug: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.slug == slug)
}

/// Short display label for the bottom-right status indicator (e.g. `Enso` for `enso-auto`).
pub fn compact_name<'a>(slug: &str, display_name: &'a str) -> &'a str {
    let trimmed_slug = slug.trim();
    if trimmed_slug.eq_ignore_ascii_case("enso-auto")
        || trimmed_slug.eq_ignore_ascii_case("enso")
        || display_name.trim().eq_ignore_ascii_case("Enso Auto")
    {
        return "Enso";
    }
    if let Some(stripped) = display_name.trim().strip_suffix(" Auto") {
        if !stripped.is_empty() {
            return stripped;
        }
    }
    if let Some(model) = find(trimmed_slug) {
        if let Some(stripped) = model.name.strip_suffix(" Auto") {
            return stripped;
        }
        return model.name;
    }
    display_name.trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_model_is_known() {
        let model = find(crate::DEFAULT_MODEL).expect("the default model is in the catalog");
        assert_eq!(model.window, 1_000_000);
        assert!(model.listed);
    }

    #[test]
    fn every_slug_is_listed_once() {
        for (index, model) in MODELS.iter().enumerate() {
            assert!(
                !MODELS[..index].iter().any(|other| other.slug == model.slug),
                "{} is listed twice",
                model.slug
            );
        }
    }

    #[test]
    fn enso_sorts_before_zen_and_zen_before_third_party() {
        assert!(picker_priority("enso-auto") < picker_priority("enso"));
        assert!(picker_priority("enso-free") < picker_priority("zen6"));
        assert!(picker_priority("hybrid/auto") < picker_priority("zen6"));
        assert!(picker_priority("zen6") < picker_priority("zen6-nano"));
        assert!(picker_priority("zen6-nano") < picker_priority("zen5"));
        assert!(picker_priority("zen-free") < picker_priority("anthropic/claude-sonnet-4-6"));
        assert!(picker_priority("zen-free") < picker_priority("openai/gpt-5"));
    }

    #[test]
    fn first_party_distinguishes_hanzo_from_third_party_and_non_chat() {
        assert!(is_first_party("enso-auto"));
        assert!(is_first_party("zen6-flash"));
        assert!(is_first_party("zen6-nano"));
        assert!(is_first_party("zenlm/zen6-coder"));
        assert!(!is_first_party("anthropic/claude-sonnet-4-6"));
        assert!(!is_first_party("openai/gpt-5"));
        assert!(!is_first_party("deepseek/deepseek-v3"));
        assert!(!is_first_party("zen-embedding"));
        assert!(!is_chat_model("text-embedding-3-large"));
        assert!(!is_chat_model("bge-reranker-v2"));
    }

    #[test]
    fn an_unserved_slug_is_unknown() {
        assert_eq!(find("zen-embedding"), None);
        assert_eq!(find("not-a-model"), None);
    }
}
