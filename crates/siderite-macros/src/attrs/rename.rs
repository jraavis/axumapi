//! serde-compatible renaming rules.

/// A `#[serde(rename_all = "...")]` rule.
#[derive(Clone, Copy, Debug)]
pub enum RenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

/// Accepted `rename_all` spellings, for diagnostics.
pub const EXPECTED: &str = "lowercase, UPPERCASE, PascalCase, camelCase, snake_case, \
                            SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE";

impl RenameRule {
    /// Parse a rule name.
    pub fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "lowercase" => Self::Lower,
            "UPPERCASE" => Self::Upper,
            "PascalCase" => Self::Pascal,
            "camelCase" => Self::Camel,
            "snake_case" => Self::Snake,
            "SCREAMING_SNAKE_CASE" => Self::ScreamingSnake,
            "kebab-case" => Self::Kebab,
            "SCREAMING-KEBAB-CASE" => Self::ScreamingKebab,
            _ => return None,
        })
    }

    /// Rename a struct field (written in `snake_case`).
    pub fn apply_to_field(self, field: &str) -> String {
        match self {
            Self::Lower | Self::Snake => field.to_owned(),
            Self::Upper | Self::ScreamingSnake => field.to_ascii_uppercase(),
            Self::Pascal => pascal_from_snake(field),
            Self::Camel => lower_first(&pascal_from_snake(field)),
            Self::Kebab => field.replace('_', "-"),
            Self::ScreamingKebab => field.to_ascii_uppercase().replace('_', "-"),
        }
    }

    /// Rename an enum variant (written in `PascalCase`).
    pub fn apply_to_variant(self, variant: &str) -> String {
        match self {
            Self::Pascal => variant.to_owned(),
            Self::Lower => variant.to_ascii_lowercase(),
            Self::Upper => variant.to_ascii_uppercase(),
            Self::Camel => lower_first(variant),
            Self::Snake => snake_from_pascal(variant),
            Self::ScreamingSnake => snake_from_pascal(variant).to_ascii_uppercase(),
            Self::Kebab => snake_from_pascal(variant).replace('_', "-"),
            Self::ScreamingKebab => snake_from_pascal(variant)
                .to_ascii_uppercase()
                .replace('_', "-"),
        }
    }
}

fn pascal_from_snake(field: &str) -> String {
    let mut out = String::with_capacity(field.len());
    let mut capitalize = true;
    for c in field.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            out.push(c.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    out
}

fn lower_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
        None => String::new(),
    }
}

pub(crate) fn snake_from_pascal(variant: &str) -> String {
    let mut out = String::with_capacity(variant.len() + 4);
    for (i, c) in variant.char_indices() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::RenameRule;

    #[test]
    fn fields() {
        let cases = [
            ("lowercase", "first_name", "first_name"),
            ("UPPERCASE", "first_name", "FIRST_NAME"),
            ("PascalCase", "first_name", "FirstName"),
            ("camelCase", "first_name", "firstName"),
            ("snake_case", "first_name", "first_name"),
            ("SCREAMING_SNAKE_CASE", "first_name", "FIRST_NAME"),
            ("kebab-case", "first_name", "first-name"),
            ("SCREAMING-KEBAB-CASE", "first_name", "FIRST-NAME"),
        ];
        for (rule, input, want) in cases {
            let rule = RenameRule::parse(rule).unwrap_or(RenameRule::Lower);
            assert_eq!(rule.apply_to_field(input), want);
        }
    }

    #[test]
    fn variants() {
        let cases = [
            ("lowercase", "FirstName", "firstname"),
            ("UPPERCASE", "FirstName", "FIRSTNAME"),
            ("PascalCase", "FirstName", "FirstName"),
            ("camelCase", "FirstName", "firstName"),
            ("snake_case", "FirstName", "first_name"),
            ("SCREAMING_SNAKE_CASE", "FirstName", "FIRST_NAME"),
            ("kebab-case", "FirstName", "first-name"),
            ("SCREAMING-KEBAB-CASE", "FirstName", "FIRST-NAME"),
        ];
        for (rule, input, want) in cases {
            let rule = RenameRule::parse(rule).unwrap_or(RenameRule::Lower);
            assert_eq!(rule.apply_to_variant(input), want);
        }
    }
}
