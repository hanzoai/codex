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
}

const MILLION: i64 = 1_000_000;

const fn model(slug: &'static str, name: &'static str, vision: bool) -> Model {
    Model {
        slug,
        name,
        window: MILLION,
        vision,
    }
}

pub const MODELS: &[Model] = &[
    model("enso", "Enso", true),
    model("enso-auto", "Enso Auto", true),
    model("enso-flash", "Enso Flash", true),
    model("enso-free", "Enso Free", true),
    model("enso-pro", "Enso Pro", false),
    model("enso-ultra", "Enso Ultra", true),
    model("hanzo/enso", "Enso", true),
    model("hanzo/zen", "Zen", true),
    model("zen-free", "Zen Free", true),
    model("zen-vl", "Zen VL", true),
    model("zen5", "Zen 5", true),
    model("zen5-coder", "Zen 5 Coder", false),
    model("zen5-evo", "Zen 5 Evo", true),
    model("zen5-flash", "Zen 5 Flash", true),
    model("zen5-mini", "Zen 5 Mini", false),
    model("zen5-pro", "Zen 5 Pro", false),
    model("zen5-spark", "Zen 5 Spark", true),
    model("zen6", "Zen 6", true),
    model("zen6-flash", "Zen 6 Flash", true),
];

/// The served chat model named `slug`, if there is one.
pub fn find(slug: &str) -> Option<&'static Model> {
    MODELS.iter().find(|model| model.slug == slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_model_is_known() {
        let model = find(crate::DEFAULT_MODEL).expect("the default model is in the catalog");
        assert_eq!(model.window, 1_000_000);
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
    fn an_unserved_slug_is_unknown() {
        assert_eq!(find("zen-embedding"), None);
        assert_eq!(find("not-a-model"), None);
    }
}
