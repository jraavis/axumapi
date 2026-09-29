//! The [`Validate`] trait: types that can check their own invariants.

use crate::error::{LocationItem, ValidationError, ValidationResult};

/// A value that can validate its own invariants.
pub trait Validate {
    /// Check this value, returning every violation that is found.
    fn validate(&self) -> ValidationResult<()>;
}

impl<T: Validate> Validate for Option<T> {
    fn validate(&self) -> ValidationResult<()> {
        match self {
            Some(inner) => inner.validate(),
            None => Ok(()),
        }
    }
}

impl<T: Validate> Validate for Vec<T> {
    fn validate(&self) -> ValidationResult<()> {
        let mut errors = ValidationError::new();
        for (index, item) in self.iter().enumerate() {
            if let Err(item_errors) = item.validate() {
                errors.merge(item_errors.prefixed(LocationItem::index(index)));
            }
        }
        errors.into_result()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FieldError;

    struct MinLen(String);

    impl Validate for MinLen {
        fn validate(&self) -> ValidationResult<()> {
            if self.0.chars().count() >= 3 {
                Ok(())
            } else {
                Err(FieldError::new("string_too_short", "too short").into())
            }
        }
    }

    #[test]
    fn option_none_is_ok() {
        let value: Option<MinLen> = None;
        assert!(value.validate().is_ok());
    }

    #[test]
    fn option_some_forwards_inner() {
        assert!(Some(MinLen("abc".into())).validate().is_ok());
        assert!(Some(MinLen("ab".into())).validate().is_err());
    }

    #[test]
    fn vec_empty_is_ok() {
        let value: Vec<MinLen> = Vec::new();
        assert!(value.validate().is_ok());
    }

    #[test]
    fn vec_collects_every_error() {
        let value = vec![
            MinLen("ok!".into()),
            MinLen("no".into()),
            MinLen("abc".into()),
            MinLen("x".into()),
        ];
        let error = value.validate().unwrap_err();
        assert_eq!(error.errors.len(), 2);
        assert_eq!(error.errors[0].location, vec![LocationItem::index(1)]);
        assert_eq!(error.errors[1].location, vec![LocationItem::index(3)]);
    }

    #[test]
    fn vec_all_valid_is_ok() {
        let value = vec![MinLen("abc".into()), MinLen("abcd".into())];
        assert!(value.validate().is_ok());
    }
}
