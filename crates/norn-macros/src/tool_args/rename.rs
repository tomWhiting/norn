//! Shared serde `rename_all` rule definitions used by both the struct
//! schema builder (`schema.rs`) and the enum schema builder
//! (`enum_schema.rs`).

/// The eight serde `rename_all` rules. Unknown rule names silently fall back to
/// the raw ident — serde would itself error at deserialize time if the rule
/// name were misspelled, so we do not duplicate that diagnostic here.
#[derive(Clone, Copy)]
pub(super) enum RenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl RenameRule {
    pub(super) fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "lowercase" => RenameRule::Lower,
            "UPPERCASE" => RenameRule::Upper,
            "PascalCase" => RenameRule::Pascal,
            "camelCase" => RenameRule::Camel,
            "snake_case" => RenameRule::Snake,
            "SCREAMING_SNAKE_CASE" => RenameRule::ScreamingSnake,
            "kebab-case" => RenameRule::Kebab,
            "SCREAMING-KEBAB-CASE" => RenameRule::ScreamingKebab,
            _ => return None,
        })
    }

    pub(super) fn apply(self, ident: &str) -> String {
        let words = split_ident(ident);
        match self {
            RenameRule::Lower => words.concat().to_lowercase(),
            RenameRule::Upper => words.concat().to_uppercase(),
            RenameRule::Snake => words
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join("_"),
            RenameRule::ScreamingSnake => words
                .iter()
                .map(|w| w.to_uppercase())
                .collect::<Vec<_>>()
                .join("_"),
            RenameRule::Kebab => words
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<Vec<_>>()
                .join("-"),
            RenameRule::ScreamingKebab => words
                .iter()
                .map(|w| w.to_uppercase())
                .collect::<Vec<_>>()
                .join("-"),
            RenameRule::Pascal => words.iter().map(|w| capitalize(w)).collect(),
            RenameRule::Camel => {
                let mut iter = words.iter();
                let head = iter.next().map(|w| w.to_lowercase()).unwrap_or_default();
                let tail: String = iter.map(|w| capitalize(w)).collect();
                head + &tail
            }
        }
    }
}

/// Splits an identifier into words by underscores (`snake_case`) and uppercase
/// boundaries (`PascalCase`/`camelCase`). Consecutive uppercase runs stay
/// together until a lowercase appears, so `HTTPRequest` yields
/// `["HTTP", "Request"]` and `my_field` yields `["my", "field"]` — matching
/// serde's heuristic.
fn split_ident(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if c.is_uppercase() && !current.is_empty() {
            let prev_lower = chars
                .get(i.wrapping_sub(1))
                .is_some_and(|ch| ch.is_lowercase());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if prev_lower || next_lower {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Returns `s` with the first character upper-cased, leaving the rest of the
/// word verbatim. Used by Pascal and camelCase rendering.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
    }
}
