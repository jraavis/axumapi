use axumapi::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Bad {
    #[field(serialization_alias = "other")]
    a: String,
    #[field(alias = "renamed")]
    b: String,
}

fn main() {}
