use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Bad {
    #[field(min_lenght = 3)]
    a: String,
}

fn main() {}
