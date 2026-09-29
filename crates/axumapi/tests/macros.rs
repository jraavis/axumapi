//! Route attributes, `routes![]` and `derive(Schema)` end to end.
#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use axumapi::http::StatusCode;
use axumapi::prelude::*;
use axumapi_testkit::TestClient;
use serde_json::{Value, json};

/// A user account.
#[derive(Serialize, Deserialize, Schema)]
#[serde(rename_all = "camelCase")]
struct UserOut {
    id: i64,
    /// Shown in the UI.
    #[field(min_length = 1, max_length = 50)]
    display_name: String,
    #[serde(rename = "e-mail")]
    #[field(email)]
    email: Option<String>,
    #[field(description = "Access level")]
    role: Role,
    #[field(ge = 0, le = 150, examples(30, 40))]
    age: i32,
    #[serde(default)]
    nickname: String,
    #[serde(skip)]
    internal: u8,
    #[serde(skip_serializing, skip_deserializing)]
    hidden_both: u8,
    #[serde(skip_deserializing)]
    server_only: u8,
    #[field(alias = "site", url, title = "Website", default_factory = String::new)]
    website: Option<String>,
}

#[derive(Serialize, Deserialize, Schema)]
struct UserIn {
    name: String,
}

#[derive(Serialize, Deserialize, Schema)]
#[serde(rename_all = "snake_case")]
enum Role {
    Admin,
    #[serde(rename = "regular-user")]
    RegularUser,
}

/// Create a user.
///
/// Creates and returns the user.
/// Second line.
#[post("/users", status = 201, response_model = UserOut, tag = "users", tags("a", "b"))]
async fn create_user(Json(input): Json<UserIn>) -> Json<UserIn> {
    Json(input)
}

/// List users
#[get("/users")]
async fn list_users() -> Json<Vec<UserIn>> {
    Json(vec![])
}

#[get(
    "/users/{id}",
    summary = "Get one",
    operation_id = "fetch_user",
    deprecated
)]
async fn get_user(Path(id): Path<i64>) -> Json<i64> {
    Json(id)
}

#[get("/secret", hidden)]
async fn secret() -> PlainText<&'static str> {
    PlainText("s")
}

#[delete("/users/{id}", status = 204)]
async fn remove_user(Path(_id): Path<i64>) -> NoContent {
    NoContent
}

mod admin {
    use axumapi::prelude::*;

    /// Ping.
    #[get("/admin/ping")]
    pub async fn ping() -> PlainText<&'static str> {
        PlainText("pong")
    }
}

fn app() -> App {
    App::new().title("t").version("1").routes(routes![
        create_user,
        list_users,
        get_user,
        secret,
        remove_user,
        admin::ping,
    ])
}

fn doc() -> Value {
    app().openapi().unwrap().to_value()
}

#[tokio::test]
async fn routes_are_served_and_status_is_overridden() {
    let client = TestClient::new(app());
    let created = client
        .post_json("/users", &json!({ "name": "ada" }))
        .await
        .unwrap();
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(client.get("/users").await.unwrap().status, StatusCode::OK);
    assert_eq!(client.get("/users/7").await.unwrap().text(), "7");
    assert_eq!(client.get("/admin/ping").await.unwrap().text(), "pong");
    assert_eq!(client.get("/secret").await.unwrap().text(), "s");
}

#[tokio::test]
async fn annotated_fns_stay_callable() {
    assert_eq!(secret().await.0, "s");
    assert_eq!(get_user(Path(3)).await.0, 3);
    let _ = __axumapi_route_secret();
}

#[test]
fn operation_metadata_comes_from_attributes_and_docs() {
    let doc = doc();
    let post = &doc["paths"]["/users"]["post"];
    assert_eq!(post["summary"], "Create a user.");
    assert_eq!(
        post["description"],
        "Creates and returns the user.\nSecond line."
    );
    assert_eq!(post["operationId"], "create_user");
    assert_eq!(post["tags"], json!(["users", "a", "b"]));
    assert_eq!(
        post["responses"]["201"]["content"]["application/json"]["schema"]["$ref"],
        "#/components/schemas/UserOut"
    );
    let list = &doc["paths"]["/users"]["get"];
    assert_eq!(list["summary"], "List users");
    assert!(list.get("description").is_none());
    let get = &doc["paths"]["/users/{id}"]["get"];
    assert_eq!(get["summary"], "Get one");
    assert_eq!(get["operationId"], "fetch_user");
    assert_eq!(get["deprecated"], true);
    assert!(doc["paths"].get("/secret").is_none());
    assert!(doc["paths"]["/users/{id}"]["delete"]["responses"]["204"].is_object());
    assert_eq!(doc["paths"]["/admin/ping"]["get"]["summary"], "Ping.");
}

#[test]
fn derived_struct_schema() {
    let doc = doc();
    let user = &doc["components"]["schemas"]["UserOut"];
    assert_eq!(user["type"], "object");
    assert_eq!(user["description"], "A user account.");
    let props = &user["properties"];
    let mut names: Vec<&str> = props
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "age",
            "displayName",
            "e-mail",
            "id",
            "nickname",
            "role",
            "serverOnly",
            "site"
        ]
    );
    assert_eq!(props["displayName"]["minLength"], 1);
    assert_eq!(props["displayName"]["maxLength"], 50);
    assert_eq!(props["displayName"]["description"], "Shown in the UI.");
    assert_eq!(props["e-mail"]["format"], "email");
    assert_eq!(props["age"]["minimum"], 0);
    assert_eq!(props["age"]["maximum"], 150);
    assert_eq!(props["age"]["examples"], json!([30, 40]));
    assert_eq!(props["site"]["title"], "Website");
    assert_eq!(props["site"]["format"], "uri");
    assert_eq!(
        props["role"],
        json!({ "allOf": [{ "$ref": "#/components/schemas/Role" }], "description": "Access level" })
    );
    let mut required: Vec<&str> = user["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    required.sort_unstable();
    assert_eq!(required, ["age", "displayName", "id", "role"]);
    assert_eq!(
        doc["components"]["schemas"]["Role"],
        json!({ "type": "string", "enum": ["admin", "regular-user"] })
    );
}

#[test]
fn document_is_valid_openapi_31() {
    let meta: Value = serde_json::from_str(include_str!(
        "../../axumapi-openapi/tests/fixtures/openapi-3.1-schema.json"
    ))
    .unwrap();
    let doc = doc();
    let validator = jsonschema::validator_for(&meta).unwrap();
    let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

fn schema_of<T: Schema + 'static>() -> Value {
    T::schema(&mut SchemaRegistry::new()).into_value()
}

#[derive(Schema)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    a: String,
    b: Option<i32>,
}

/// Wrapper.
#[derive(Schema)]
struct Id(i64);

#[derive(Schema)]
struct Nothing;

#[derive(Schema)]
struct Pair(String, i32);

#[derive(Schema)]
struct Page<T> {
    items: Vec<T>,
}

#[derive(Schema)]
#[schema(name = "UserPage")]
struct NamedPage<T> {
    items: Vec<T>,
}

#[derive(Schema)]
#[schema(inline)]
struct Inline {
    x: bool,
}

#[test]
fn struct_shapes_and_names() {
    assert_eq!(
        schema_of::<Settings>(),
        json!({
            "type": "object",
            "properties": {
                "a": { "type": "string" },
                "b": { "anyOf": [{ "type": "integer", "format": "int32" }, { "type": "null" }] }
            },
            "additionalProperties": false
        })
    );
    assert_eq!(
        schema_of::<Id>(),
        json!({ "type": "integer", "format": "int64", "description": "Wrapper." })
    );
    assert_eq!(<Id as Schema>::schema_name(), Some("Id"));
    assert_eq!(schema_of::<Nothing>(), json!({ "type": "null" }));
    assert_eq!(
        schema_of::<Pair>()["prefixItems"].as_array().unwrap().len(),
        2
    );
    assert_eq!(<Page<Id> as Schema>::schema_name(), None);
    assert_eq!(<NamedPage<Id> as Schema>::schema_name(), Some("UserPage"));
    assert_eq!(<Inline as Schema>::schema_name(), None);
    assert_eq!(
        schema_of::<Page<Id>>()["properties"]["items"]["items"]["$ref"],
        "#/components/schemas/Id"
    );
}

#[derive(Schema)]
enum Shape {
    Empty,
    Circle(f64),
    Rect { w: f64, h: f64 },
}

#[derive(Schema)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Event {
    Ping,
    Message { text: String },
    Wrapped(Settings),
}

#[derive(Schema)]
#[serde(tag = "t", content = "c")]
enum Adjacent {
    A,
    B(String),
}

#[derive(Schema)]
#[serde(untagged)]
enum Either {
    Num(i64),
    Text(String),
}

#[test]
fn enum_shapes() {
    let shape = schema_of::<Shape>();
    let one_of = shape["oneOf"].as_array().unwrap();
    assert_eq!(one_of[0], json!({ "type": "string", "const": "Empty" }));
    assert_eq!(
        one_of[1],
        json!({
            "type": "object",
            "properties": { "Circle": { "type": "number", "format": "double" } },
            "required": ["Circle"],
            "additionalProperties": false
        })
    );
    assert_eq!(
        one_of[2]["properties"]["Rect"]["required"],
        json!(["w", "h"])
    );

    let event = schema_of::<Event>();
    assert_eq!(
        event["oneOf"][0],
        json!({
            "type": "object",
            "properties": { "type": { "type": "string", "const": "ping" } },
            "required": ["type"]
        })
    );
    assert_eq!(event["oneOf"][1]["required"], json!(["type", "text"]));
    assert_eq!(event["oneOf"][2]["allOf"].as_array().unwrap().len(), 2);

    let adjacent = schema_of::<Adjacent>();
    assert_eq!(adjacent["oneOf"][1]["required"], json!(["t", "c"]));
    assert_eq!(adjacent["oneOf"][0]["required"], json!(["t"]));

    assert_eq!(
        schema_of::<Either>(),
        json!({ "oneOf": [
            { "type": "integer", "format": "int64" },
            { "type": "string" }
        ]})
    );
}

#[derive(Schema)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE")]
struct Cased {
    first_name: String,
}

#[derive(Schema)]
#[serde(rename_all_fields = "camelCase")]
enum Fielded {
    One { some_field: u8 },
}

#[test]
fn casing() {
    assert!(
        schema_of::<Cased>()["properties"]
            .get("FIRST-NAME")
            .is_some()
    );
    assert!(
        schema_of::<Fielded>()["oneOf"][0]["properties"]["One"]["properties"]
            .get("someField")
            .is_some()
    );
}
