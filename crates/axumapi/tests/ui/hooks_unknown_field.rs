use axumapi::prelude::*;
use axumapi::validation::FieldError;

#[derive(Deserialize, Validate, Schema)]
struct Model {
    name: String,
}

#[model_hooks]
impl Model {
    #[field_validator("nmae")]
    fn check(_value: &str) -> Result<(), FieldError> {
        Ok(())
    }
}

fn main() {}
