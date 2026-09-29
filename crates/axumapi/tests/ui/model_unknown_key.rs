use axumapi::prelude::*;

#[derive(Model)]
#[model(tabel = "users")]
struct User {
    #[field(primary_key, auto)]
    id: i64,
}

fn main() {}
