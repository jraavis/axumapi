//! Reusable pure field validators.
//!
//! Each function returns `Ok(())` or a [`FieldError`] with a stable `code`.

use std::fmt::Display;
use std::net::IpAddr;
use std::str::FromStr;

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

/// Parse `value` as a URL.
///
/// Accepts any scheme the [`url`] crate accepts (including `http`, `https`,
/// `ftp`, `file`, …). Relative references without a scheme are rejected.
pub(crate) fn parse_url(value: &str) -> Result<url::Url, FieldError> {
    url::Url::parse(value).map_err(|_| FieldError::new("url_parsing", "input is not a valid URL"))
}

/// Ensure `value` is a valid absolute URL.
pub fn url(value: &str) -> Result<(), FieldError> {
    parse_url(value).map(|_| ())
}

/// Parse `value` as a UUID in hyphenated (`8-4-4-4-12`) or simple (32 hex
/// digits) form. Matching is case-insensitive.
///
/// URN (`urn:uuid:…`) and braced (`{…}`) forms accepted by the `uuid` crate
/// are **rejected** so the public contract stays hyphenated/simple only.
pub(crate) fn parse_uuid(value: &str) -> Result<uuid::Uuid, FieldError> {
    let err = || FieldError::new("uuid_parsing", "input is not a valid UUID");
    match value.len() {
        32 | 36 => uuid::Uuid::try_parse(value).map_err(|_| err()),
        _ => Err(err()),
    }
}

/// Ensure `value` is a hyphenated or simple UUID string.
pub fn uuid(value: &str) -> Result<(), FieldError> {
    parse_uuid(value).map(|_| ())
}

/// Parse `value` as an IPv4 or IPv6 address.
pub(crate) fn parse_ip(value: &str) -> Result<IpAddr, FieldError> {
    IpAddr::from_str(value)
        .map_err(|_| FieldError::new("ip_any_parsing", "input is not a valid IP address"))
}

/// Ensure `value` is an IPv4 or IPv6 address.
pub fn ip(value: &str) -> Result<(), FieldError> {
    parse_ip(value).map(|_| ())
}

/// Parse `value` as an IPv4 address.
pub(crate) fn parse_ipv4(value: &str) -> Result<std::net::Ipv4Addr, FieldError> {
    std::net::Ipv4Addr::from_str(value)
        .map_err(|_| FieldError::new("ip_v4_parsing", "input is not a valid IPv4 address"))
}

/// Parse `value` as an IPv6 address.
pub(crate) fn parse_ipv6(value: &str) -> Result<std::net::Ipv6Addr, FieldError> {
    std::net::Ipv6Addr::from_str(value)
        .map_err(|_| FieldError::new("ip_v6_parsing", "input is not a valid IPv6 address"))
}

fn decimal_digit_counts(value: rust_decimal::Decimal) -> (u32, u32) {
    let scale = value.scale();
    let mantissa = value.mantissa().unsigned_abs();
    let coefficient_digits = if mantissa == 0 {
        1
    } else {
        mantissa.ilog10() + 1
    };
    let decimals = scale;
    let digits = coefficient_digits.max(scale);
    (decimals, digits)
}

/// Ensure `value` has at most `max_digits` significant digits and at most
/// `max_places` digits after the decimal point.
///
/// Trailing zeros are ignored, matching Pydantic: `1.20` satisfies
/// `max_places = 1`. Unlike Pydantic, integer digits are **not** additionally
/// capped at `max_digits - max_places`, so `Decimal<28, 28>` remains usable
/// for values with an integer part.
pub fn decimal_digits(
    value: rust_decimal::Decimal,
    max_digits: u32,
    max_places: u32,
) -> Result<(), FieldError> {
    let (decimals, digits) = decimal_digit_counts(value);
    let (norm_decimals, norm_digits) = decimal_digit_counts(value.normalize());
    if decimals > max_places && norm_decimals > max_places {
        return Err(FieldError::new(
            "decimal_max_places",
            format!("ensure this value has at most {max_places} decimal places"),
        ));
    }
    if digits > max_digits && norm_digits > max_digits {
        return Err(FieldError::new(
            "decimal_max_digits",
            format!("ensure this value has at most {max_digits} digits in total"),
        ));
    }
    Ok(())
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

    #[test]
    fn url_accepts_any_scheme() {
        assert!(url("https://example.com/path").is_ok());
        assert!(url("ftp://files.example.com").is_ok());
        assert_eq!(url("not a url").unwrap_err().code, "url_parsing");
        assert_eq!(url("example.com").unwrap_err().code, "url_parsing");
    }

    #[test]
    fn uuid_hyphenated_and_simple() {
        assert!(uuid("550e8400-e29b-41d4-a716-446655440000").is_ok());
        assert!(uuid("550E8400-E29B-41D4-A716-446655440000").is_ok());
        assert!(uuid("550e8400e29b41d4a716446655440000").is_ok());
        assert_eq!(uuid("not-a-uuid").unwrap_err().code, "uuid_parsing");
        assert_eq!(
            uuid("urn:uuid:550e8400-e29b-41d4-a716-446655440000")
                .unwrap_err()
                .code,
            "uuid_parsing"
        );
    }

    #[test]
    fn ip_v4_and_v6() {
        assert!(ip("127.0.0.1").is_ok());
        assert!(ip("::1").is_ok());
        assert_eq!(ip("not-an-ip").unwrap_err().code, "ip_any_parsing");
        assert_eq!(parse_ipv4("::1").unwrap_err().code, "ip_v4_parsing");
        assert_eq!(parse_ipv6("127.0.0.1").unwrap_err().code, "ip_v6_parsing");
    }

    #[test]
    fn decimal_digits_trailing_zeros_and_limits() {
        use rust_decimal::Decimal;
        use std::str::FromStr;

        let one_point_two_zero = Decimal::from_str("1.20").unwrap();
        assert!(decimal_digits(one_point_two_zero, 5, 1).is_ok());
        let too_many_places = Decimal::from_str("1.234").unwrap();
        assert_eq!(
            decimal_digits(too_many_places, 5, 2).unwrap_err().code,
            "decimal_max_places"
        );
        let too_many_digits = Decimal::from_str("12345.6").unwrap();
        assert_eq!(
            decimal_digits(too_many_digits, 5, 2).unwrap_err().code,
            "decimal_max_digits"
        );
        let integer_part = Decimal::from_str("123").unwrap();
        assert!(decimal_digits(integer_part, 28, 28).is_ok());
    }
}
