use siderite::prelude::*;

#[get("/users/{id")]
async fn a() {}

#[get("/users/id}")]
async fn b() {}

#[get("/users/{id}/{id}")]
async fn c() {}

#[get("/users/{1bad}")]
async fn d() {}

fn main() {}
