use siderite::prelude::*;

#[derive(Deserialize, Validate, Schema)]
struct Bad {
    #[field(default = 5)]
    a: u8,
    #[field(default_factory = String::new)]
    b: String,
}

fn main() {}
