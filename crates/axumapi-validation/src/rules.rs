//! Reusable pure field validators.
//!
//! Each function returns `Ok(())` or a [`FieldError`] with a stable `code`.

use std::fmt::Display;

use regex::Regex;

use crate::error::FieldError;

/// Ensure `value` contains at least `min` Unicode scalar values (characters, not bytes).
pub fn min_length(value: &str, min: usize) -> Result<(), FieldError> {
    let len = value.chars().count();
    if len < min {
        Err(FieldError::new(
            "string_too_short",
            format!("ensure this value has at least {min} characters"),
        ))
    } else {
        Ok(())
    }
}

/// Ensure `value` contains at most `max` Unicode scalar values (characters, not bytes).
pub fn max_length(value: &str, max: usize) -> Result<(), FieldError> {
    let len = value.chars().count();
    if len > max {
        Err(FieldError::new(
            "string_too_long",
            format!("ensure this value has at most {max} characters"),
        ))
    } else {
        Ok(())
    }
}

/// Ensure `value` matches `regex`.
///
/// Matching uses [`Regex::is_match`], so callers who want a full-string match
/// should supply an anchored pattern.
pub fn pattern(value: &str, regex: &Regex) -> Result<(), FieldError> {
    if regex.is_match(value) {
        Ok(())
    } else {
        Err(FieldError::new(
            "pattern_mismatch",
            format!("string does not match pattern `{regex}`"),
        ))
    }
}

/// Pragmatic email check used by [`crate::types::Email`].
///
/// Accepts a value that has exactly one `@`, a non-empty local part, a domain
/// containing a `.`, and no whitespace. This is **not** a full RFC 5322
/// validator.
pub fn email(value: &str) -> Result<(), FieldError> {
    if is_pragmatic_email(value) {
        Ok(())
    } else {
        Err(FieldError::new("invalid_email", "invalid email address"))
    }
}

fn is_pragmatic_email(value: &str) -> bool {
    if value.chars().any(char::is_whitespace) {
        return false;
    }
    let mut parts = value.split('@');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(local), Some(domain), None) => !local.is_empty() && domain.contains('.'),
        _ => false,
    }
}

/// Ensure `value > limit`.
pub fn gt<T>(value: &T, limit: &T) -> Result<(), FieldError>
where
    T: PartialOrd + Display,
{
    if value > limit {
        Ok(())
    } else {
        Err(FieldError::new(
            "greater_than",
            format!("ensure this value is greater than {limit}"),
        ))
    }
}

/// Ensure `value >= limit`.
pub fn ge<T>(value: &T, limit: &T) -> Result<(), FieldError>
where
    T: PartialOrd + Display,
{
    if value >= limit {
        Ok(())
    } else {
        Err(FieldError::new(
            "greater_than_equal",
            format!("ensure this value is greater than or equal to {limit}"),
        ))
    }
}

/// Ensure `value < limit`.
pub fn lt<T>(value: &T, limit: &T) -> Result<(), FieldError>
where
    T: PartialOrd + Display,
{
    if value < limit {
        Ok(())
    } else {
        Err(FieldError::new(
            "less_than",
            format!("ensure this value is less than {limit}"),
        ))
    }
}

/// Ensure `value <= limit`.
pub fn le<T>(value: &T, limit: &T) -> Result<(), FieldError>
where
    T: PartialOrd + Display,
{
    if value <= limit {
        Ok(())
    } else {
        Err(FieldError::new(
            "less_than_equal",
            format!("ensure this value is less than or equal to {limit}"),
        ))
    }
}

/// Ensure `value` is a multiple of `multiple`.
///
/// Returns an error when `multiple` is `0` (the check is undefined and must
/// not panic).
pub fn multiple_of(value: i64, multiple: i64) -> Result<(), FieldError> {
    let is_multiple = match multiple {
        0 => {
            return Err(FieldError::new("multiple_of", "multiple must not be zero"));
        }
        // Every integer is a multiple of ±1; avoid `i64::MIN % -1` overflow.
        1 | -1 => true,
        _ => value % multiple == 0,
    };
    if is_multiple {
        Ok(())
    } else {
        Err(FieldError::new(
            "multiple_of",
            format!("ensure this value is a multiple of {multiple}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn min_length_counts_characters_not_bytes() {
        assert_eq!("é".len(), 2);
        assert_eq!("é".chars().count(), 1);
        assert!(min_length("é", 1).is_ok());
        let err = min_length("é", 2).unwrap_err();
        assert_eq!(err.code, "string_too_short");
        assert!(min_length("éé", 2).is_ok());
        assert!(min_length("", 0).is_ok());
    }

    #[test]
    fn max_length_counts_characters_not_bytes() {
        assert!(max_length("é", 1).is_ok());
        let err = max_length("éé", 1).unwrap_err();
        assert_eq!(err.code, "string_too_long");
        assert!(max_length("😀", 1).is_ok());
        assert_eq!("😀".len(), 4);
    }

    #[test]
    fn pattern_success_and_failure() {
        let re = Regex::new(r"^[a-z]+$").unwrap();
        assert!(pattern("abc", &re).is_ok());
        let err = pattern("Abc", &re).unwrap_err();
        assert_eq!(err.code, "pattern_mismatch");
    }

    #[test]
    fn email_pragmatic_rules() {
        assert!(email("user@example.com").is_ok());
        for invalid in [
            "not-an-email",
            "@example.com",
            "user@",
            "user@localhost",
            "user@@example.com",
            "user@example.com extra",
            "user @example.com",
            "",
        ] {
            let err = email(invalid).unwrap_err();
            assert_eq!(err.code, "invalid_email", "{invalid}");
            assert_eq!(err.message, "invalid email address");
        }
    }

    #[test]
    fn comparisons() {
        assert!(gt(&5, &3).is_ok());
        assert_eq!(gt(&3, &3).unwrap_err().code, "greater_than");
        assert!(ge(&3, &3).is_ok());
        assert_eq!(ge(&2, &3).unwrap_err().code, "greater_than_equal");
        assert!(lt(&2, &3).is_ok());
        assert_eq!(lt(&3, &3).unwrap_err().code, "less_than");
        assert!(le(&3, &3).is_ok());
        assert_eq!(le(&4, &3).unwrap_err().code, "less_than_equal");
        assert!(gt(&1.5, &1.0).is_ok());
    }

    #[test]
    fn multiple_of_rules() {
        assert!(multiple_of(10, 5).is_ok());
        assert!(multiple_of(0, 5).is_ok());
        assert!(multiple_of(-6, 3).is_ok());
        assert_eq!(multiple_of(10, 3).unwrap_err().code, "multiple_of");
        assert_eq!(multiple_of(10, 0).unwrap_err().code, "multiple_of");
        assert!(multiple_of(i64::MIN, -1).is_ok());
        assert!(multiple_of(7, 1).is_ok());
    }
}
