use axumapi::prelude::*;

#[derive(Schema)]
struct Inner {
    a: i32,
}

#[derive(Schema)]
struct Outer {
    #[serde(flatten)]
    inner: Inner,
}

fn main() {}
