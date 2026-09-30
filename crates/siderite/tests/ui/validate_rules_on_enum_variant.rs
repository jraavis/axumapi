use siderite::Validate;
use serde::Deserialize;

#[derive(Deserialize, Validate)]
enum Shape {
    Circle {
        #[field(gt = 0)]
        radius: f64,
    },
}

fn main() {}
