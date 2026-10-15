//! Finding the model a user means by `/model <name>`.

/// What a typed name came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Pick {
    One(String),
    /// More than one model fits; the user names one of these.
    Many(Vec<String>),
    None,
}

/// Match `wanted` against the models the session offers and the ones Hanzo
/// serves: the exact name, then `<name>-auto`, then the only model whose name
/// contains it. Case does not matter.
pub fn pick(wanted: &str, offered: &[String]) -> Pick {
    let wanted = wanted.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        return Pick::None;
    }
    let mut names: Vec<String> = offered.to_vec();
    for served in crate::catalog::MODELS {
        if !names.iter().any(|name| name == served.slug) {
            names.push(served.slug.to_string());
        }
    }
    let lower = |name: &String| name.to_ascii_lowercase();
    let exact = |target: &str| names.iter().find(|name| lower(name) == target).cloned();
    if let Some(name) = exact(&wanted).or_else(|| exact(&format!("{wanted}-auto"))) {
        return Pick::One(name);
    }
    let mut near: Vec<String> = names
        .iter()
        .filter(|name| lower(name).contains(&wanted))
        .cloned()
        .collect();
    match near.len() {
        0 => Pick::None,
        1 => Pick::One(near.remove(0)),
        _ => Pick::Many(near),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered() -> Vec<String> {
        ["gpt-6-astra", "gpt-6-sol"].map(String::from).to_vec()
    }

    #[test]
    fn a_served_name_is_taken_as_it_is() {
        assert_eq!(pick("enso", &offered()), Pick::One("enso".into()));
        assert_eq!(pick("ENSO-PRO", &offered()), Pick::One("enso-pro".into()));
    }

    #[test]
    fn a_family_name_finds_its_auto_model() {
        assert_eq!(pick("zen6", &[]), Pick::One("zen6".into()));
        assert_eq!(pick("gpt-6-astra", &offered()), Pick::One("gpt-6-astra".into()));
    }

    #[test]
    fn the_only_model_containing_the_name_is_the_one() {
        assert_eq!(pick("astra", &offered()), Pick::One("gpt-6-astra".into()));
    }

    #[test]
    fn a_name_that_fits_several_asks_which() {
        let Pick::Many(names) = pick("flash", &offered()) else {
            panic!("flash fits several");
        };
        assert!(names.contains(&"enso-flash".to_string()));
        assert!(names.contains(&"zen5-flash".to_string()));
    }

    #[test]
    fn a_name_that_fits_nothing_is_not_guessed() {
        assert_eq!(pick("nonesuch", &offered()), Pick::None);
        assert_eq!(pick("  ", &offered()), Pick::None);
    }
}
