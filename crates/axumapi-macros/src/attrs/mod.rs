//! Attribute model shared by `derive(Schema)` and `derive(Validate)`.
//!
//! Every attribute that influences names, required-ness, defaults or
//! skipping is parsed here exactly once:
//!
//! * `serde`: `#[serde(...)]` (container, field, variant),
//! * `model`: `#[model_config(...)]` and `#[schema(...)]`,
//! * `field`: `#[field(...)]`,
//! * `plan`: field resolution (wire key, required-ness, aliases).

pub mod field;
pub mod model;
pub mod plan;
pub mod rename;
pub mod serde;
