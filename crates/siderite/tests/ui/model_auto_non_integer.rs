use siderite::prelude::*;

#[derive(Model)]
struct User {
    #[field(primary_key, auto)]
    id: String,
}

fn main() {}
