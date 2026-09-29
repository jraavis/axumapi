use axumapi::prelude::*;

#[derive(Model)]
struct Author {
    #[field(primary_key, auto)]
    id: i64,
}

#[derive(Model)]
struct Book {
    #[field(primary_key, auto)]
    id: i64,
    #[field(on_delete = "set_null")]
    author: ForeignKey<Author>,
}

fn main() {}
