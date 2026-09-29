use axumapi::prelude::*;

#[derive(Deserialize, Validate, Schema)]
#[model_config(extra = "sometimes", frozen)]
struct Bad {
    a: String,
}

fn main() {}
