//! Minimal axumapi application.

use axumapi::prelude::*;

#[derive(Deserialize, Schema)]
struct Greeting {
    shout: Option<bool>,
}

#[derive(Serialize, Deserialize, Schema)]
struct Message {
    message: String,
}

/// Plain-text greeting.
#[get("/")]
async fn index() -> PlainText<&'static str> {
    PlainText("Hello, axumapi!")
}

/// Greet somebody by name.
#[get("/hello/{name}", tag = "greetings")]
async fn hello(Path(name): Path<String>, Query(greeting): Query<Greeting>) -> PlainText<String> {
    let text = format!("Hello, {name}!");
    PlainText(if greeting.shout.unwrap_or(false) {
        text.to_uppercase()
    } else {
        text
    })
}

/// Echo the posted message back.
#[post("/echo", tag = "greetings")]
async fn echo(Json(message): Json<Message>) -> Json<Message> {
    Json(message)
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    App::new()
        .title("Hello World")
        .version("1.0.0")
        .routes(routes![index, hello, echo])
        .run("0.0.0.0:8000")
        .await
}
