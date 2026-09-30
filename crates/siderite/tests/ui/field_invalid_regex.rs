use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Bad {
    #[field(pattern = "(unclosed")]
    a: String,
}

fn main() {}
