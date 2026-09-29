use axumapi::prelude::*;

#[derive(Model)]
struct User {
    #[field(primary_key, auto)]
    id: i64,
    #[field(primary_key)]
    other: i64,
}

fn main() {}
