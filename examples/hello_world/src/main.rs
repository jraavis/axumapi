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

// `#[derive(Schema)]` replaces these impls once the macros land.
impl Schema for Greeting {
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object").with(
            "properties",
            serde_json::json!({ "shout": r.subschema::<Option<bool>>() }),
        )
    }
}

impl Schema for Message {
    fn schema_name() -> Option<&'static str> {
        Some("Message")
    }
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object")
            .with(
                "properties",
                serde_json::json!({ "message": r.subschema::<String>() }),
            )
            .with("required", serde_json::json!(["message"]))
    }
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
