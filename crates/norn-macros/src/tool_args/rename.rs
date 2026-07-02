//! Shared serde `rename_all` rule definitions used by both the struct
//! schema builder (`schema.rs`) and the enum schema builder
//! (`enum_schema.rs`).
//!
//! # Serde parity
//!
//! serde's `rename_all` is a *mechanical, per-character* transform, not a
//! word-splitting one, and it applies a **different** conversion depending on
//! whether the input is an enum variant name (assumed `PascalCase`) or a
//! struct/variant field name (assumed `snake_case`). This module reproduces
//! `serde_derive`'s `RenameRule::apply_to_variant` and `apply_to_field`
//! (serde internals `case.rs`) exactly, so every wire name a generated schema
//! advertises is a name `serde_json` will accept on deserialization — including
//! acronym idents, where the two algorithms diverge:
//!
//! | Rust ident      | rule                | variant → wire   | field → wire     |
//! |-----------------|---------------------|------------------|------------------|
//! | `HTTPRequest`   | `snake_case`        | `h_t_t_p_request`| (n/a as field)   |
//! | `HTTPRequest`   | `camelCase`         | `hTTPRequest`    | (n/a as field)   |
//! | `userID`        | `snake_case`        | (n/a as variant) | `userID`         |
//! | `userID`        | `SCREAMING_SNAKE…`  | (n/a as variant) | `USERID`         |
//!
//! Because the conversions are direction-specific, callers must route variant
//! wire names through [`RenameRule::apply_to_variant`] and field wire names
//! through [`RenameRule::apply_to_field`]; using the wrong one reintroduces the
//! divergence.

/// The eight serde `rename_all` rules. [`RenameRule::from_str`] returns `None`
/// for unknown rule names; the attribute parser turns that into a spanned
/// compile error so a misspelled rule never silently degrades to raw idents.
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

    /// Apply this rule to an **enum variant** name, whose input is assumed to be
    /// `PascalCase` — the exact algorithm of `serde_derive`'s
    /// `RenameRule::apply_to_variant`. Separator rules insert the separator
    /// before *every* uppercase char (so `HTTPRequest` → `h_t_t_p_request`
    /// under `snake_case`), and `camelCase` lowercases only the first character.
    pub(super) fn apply_to_variant(self, variant: &str) -> String {
        match self {
            RenameRule::Pascal => variant.to_owned(),
            RenameRule::Lower => variant.to_ascii_lowercase(),
            RenameRule::Upper => variant.to_ascii_uppercase(),
            RenameRule::Camel => lowercase_first(variant),
            RenameRule::Snake => variant_to_snake(variant),
            RenameRule::ScreamingSnake => variant_to_snake(variant).to_ascii_uppercase(),
            RenameRule::Kebab => variant_to_snake(variant).replace('_', "-"),
            RenameRule::ScreamingKebab => variant_to_snake(variant)
                .to_ascii_uppercase()
                .replace('_', "-"),
        }
    }

    /// Apply this rule to a **field** name, whose input is assumed to be
    /// `snake_case` — the exact algorithm of `serde_derive`'s
    /// `RenameRule::apply_to_field`. `snake_case`/`lowercase` are the identity
    /// (so an already-snake field is unchanged, and a stray `userID` stays
    /// `userID`, matching serde), while `PascalCase`/`camelCase` split on `_`
    /// only.
    pub(super) fn apply_to_field(self, field: &str) -> String {
        match self {
            RenameRule::Lower | RenameRule::Snake => field.to_owned(),
            RenameRule::Upper | RenameRule::ScreamingSnake => field.to_ascii_uppercase(),
            RenameRule::Pascal => field_to_pascal(field),
            RenameRule::Camel => lowercase_first(&field_to_pascal(field)),
            RenameRule::Kebab => field.replace('_', "-"),
            RenameRule::ScreamingKebab => field.to_ascii_uppercase().replace('_', "-"),
        }
    }
}

/// Convert a `PascalCase` variant name to `snake_case` the serde way: insert
/// `_` before every uppercase character (past the first) and ASCII-lowercase
/// each character. `HTTPRequest` becomes `h_t_t_p_request`, not `http_request`.
fn variant_to_snake(variant: &str) -> String {
    let mut snake = String::new();
    for (i, ch) in variant.char_indices() {
        if i > 0 && ch.is_uppercase() {
            snake.push('_');
        }
        snake.push(ch.to_ascii_lowercase());
    }
    snake
}

/// Convert a `snake_case` field name to `PascalCase` the serde way: uppercase
/// the character after each `_` (and the first), dropping the underscores and
/// leaving every other character verbatim. `user_id` → `UserId`, `userID` →
/// `UserID`.
fn field_to_pascal(field: &str) -> String {
    let mut pascal = String::new();
    let mut capitalize = true;
    for ch in field.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            pascal.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            pascal.push(ch);
        }
    }
    pascal
}

/// ASCII-lowercase only the first character, leaving the rest verbatim —
/// serde's `camelCase` step (`s[..1].to_ascii_lowercase() + &s[1..]`), but
/// char-boundary safe so a non-ASCII leading char cannot panic.
fn lowercase_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
    }
}
