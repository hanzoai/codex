//! The reasoning efforts a Hanzo model offers: one table, in the order the /model picker
//! shows them, so a name changes here and nowhere else.
//!
//! [`DEFAULT`] (`auto`) sends no effort, which leaves it to api.hanzo.ai's router: medium or
//! lower, more only when the request needs it. Every other name is sent as written. A setting
//! stored under a name the table retired is moved to its successor ([`successor`]), and an
//! upstream name the table spells differently is shown by the table's name ([`shown`]).

/// The effort Dev starts with and the picker selects when nothing is set.
pub const DEFAULT: &str = "auto";

/// Every effort, with what the picker says about it, in picker order.
pub const EFFORTS: &[(&str, &str)] = &[
    (
        DEFAULT,
        "The router decides: medium or lower unless the request needs more",
    ),
    ("instant", "No reasoning, the fastest answer"),
    ("low", "Light reasoning for quick, simple work"),
    ("medium", "Balanced reasoning for most work"),
    ("high", "Deep reasoning for hard problems"),
    ("max", "The most reasoning the model can give"),
];

/// Retired names a stored setting is moved from, and the name it becomes.
const RETIRED: &[(&str, &str)] = &[("none", "instant"), ("minimal", "instant")];

/// Upstream names the table spells differently.
const SPELLED: &[(&str, &str)] = &[("xhigh", "max")];

/// Whether an effort goes on the wire. [`DEFAULT`] does not: the router decides.
pub fn sent(name: &str) -> bool {
    name != DEFAULT
}

/// The name a stored setting `name` is moved to, when the table retired `name`.
pub fn successor(name: &str) -> Option<&'static str> {
    RETIRED
        .iter()
        .find(|(old, _)| *old == name)
        .map(|(_, new)| *new)
}

/// How the picker names `name`.
pub fn shown(name: &str) -> &str {
    SPELLED
        .iter()
        .find(|(upstream, _)| *upstream == name)
        .map_or(name, |(_, ours)| ours)
}

/// 5-step Unicode circle effort scale:
/// - `○` (U+25CB, White Circle) — 0% / Empty / Instant / Flash
/// - `◔` (U+25D4, Circle with Upper Right Quadrant Black) — 25% / Low effort
/// - `◑` (U+25D1, Circle with Right Half Black) — 50% / Medium effort
/// - `◕` (U+25D5, Circle with All But Upper Left Quadrant Black) — 75% / High effort
/// - `●` (U+25CF, Black Circle) — 100% / Full / Auto effort (default)
pub fn symbol(effort: Option<&str>) -> &'static str {
    match effort.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("instant" | "none" | "minimal") => "○",
        Some("low") => "◔",
        Some("medium") => "◑",
        Some("high") => "◕",
        Some("auto" | "max" | "xhigh" | "ultra" | "default") | None => "●",
        Some(_) => "●",
    }
}

/// Returns the effort symbol for `model` and `effort`, defaulting `-flash` models
/// to `○` (instant/fastest) when effort is unset or `auto`.
pub fn symbol_for(model: &str, effort: Option<&str>) -> &'static str {
    let lower_model = model.trim().to_ascii_lowercase();
    let is_flash = lower_model.ends_with("-flash") || lower_model.contains("-flash-");
    match effort.map(|s| s.trim().to_ascii_lowercase()).as_deref() {
        Some("instant" | "none" | "minimal") => "○",
        Some("low") => "◔",
        Some("medium") => "◑",
        Some("high") => "◕",
        Some("max" | "xhigh" | "ultra") => "●",
        Some("auto" | "default") | None => {
            if is_flash {
                "○"
            } else {
                "●"
            }
        }
        Some(_) => "●",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_the_default_and_is_not_sent() {
        assert_eq!(EFFORTS[0].0, DEFAULT);
        assert!(!sent("auto"));
        for (name, _) in &EFFORTS[1..] {
            assert!(sent(name), "{name} goes on the wire");
        }
    }

    #[test]
    fn the_picker_order_is_the_table() {
        let names: Vec<_> = EFFORTS.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["auto", "instant", "low", "medium", "high", "max"]);
    }

    #[test]
    fn retired_names_move_and_upstream_names_show_as_ours() {
        assert_eq!(successor("none"), Some("instant"));
        assert_eq!(successor("minimal"), Some("instant"));
        assert_eq!(successor("medium"), None);
        assert_eq!(successor("auto"), None);
        assert_eq!(shown("xhigh"), "max");
        assert_eq!(shown("high"), "high");
    }
}
