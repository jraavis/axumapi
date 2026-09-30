use siderite::Validate;
use serde::Deserialize;

#[derive(Deserialize, Validate)]
struct Pair(#[field(min_length = 1)] String, String);

fn main() {}
