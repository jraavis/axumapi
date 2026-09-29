//! Minimal axumapi application.

use axumapi::prelude::*;

#[derive(Deserialize)]
struct Greeting {
    shout: Option<bool>,
}

#[derive(Serialize, Deserialize)]
struct Message {
    message: String,
}

async fn index() -> PlainText<&'static str> {
    PlainText("Hello, axumapi!")
}

async fn hello(Path(name): Path<String>, Query(greeting): Query<Greeting>) -> PlainText<String> {
    let text = format!("Hello, {name}!");
    PlainText(if greeting.shout.unwrap_or(false) {
        text.to_uppercase()
    } else {
        text
    })
}

async fn echo(Json(message): Json<Message>) -> Json<Message> {
    Json(message)
}

#[tokio::main]
async fn main() -> Result<(), ServerError> {
    App::new()
        .title("Hello World")
        .version("1.0.0")
        .route("/", get(index))
        .route("/hello/{name}", get(hello))
        .route("/echo", post(echo))
        .run("0.0.0.0:8000")
        .await
}
