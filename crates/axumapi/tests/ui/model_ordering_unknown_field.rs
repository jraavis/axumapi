use axumapi::prelude::*;

#[derive(Model)]
#[model(ordering = ["-nmae"])]
struct User {
    #[field(primary_key, auto)]
    id: i64,
    name: String,
}

fn main() {}
