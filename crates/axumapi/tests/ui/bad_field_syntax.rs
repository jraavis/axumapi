use axumapi::prelude::*;

#[derive(Schema)]
struct Bad {
    #[field(min_length = "three", ge = abc)]
    a: String,
}

fn main() {}
