use axumapi::prelude::*;
use axumapi::validation::FieldError;

#[derive(Deserialize, Validate, Schema)]
struct Model {
    name: String,
}

#[model_hooks]
impl Model {
    #[field_validator("name", mode = "during")]
    fn check(_value: &str) -> Result<(), FieldError> {
        Ok(())
    }

    #[model_validator(mode = "later")]
    fn whole(&self) -> Result<(), FieldError> {
        Ok(())
    }
}

fn main() {}
