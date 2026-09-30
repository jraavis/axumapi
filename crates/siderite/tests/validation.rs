//! `derive(Validate)`, `derive(Schema)` dumping and `#[model_hooks]` end to
//! end: through HTTP, differentially against Serde, and in the OpenAPI
//! document.
#![allow(clippy::unwrap_used)]
#![allow(dead_code)]

use serde_json::{Value, json};
use siderite::JsonDump;
use siderite::prelude::*;
use siderite::validation::model::prepare_agrees_with_serde;
use siderite::validation::{DumpOptions, FieldError, FieldSet, ValidationContext, parse_value};
use siderite_testkit::{TestClient, TestResponse};

// ---------------------------------------------------------------------
// Models
// ---------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Address {
    #[field(pattern = r"^\d{5}$")]
    zip: String,
    city: String,
}

/// The reference model of `siderite_validation::model`, now derived.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "camelCase")]
#[model_config(extra = "forbid", str_strip_whitespace, populate_by_name)]
struct Signup {
    #[field(min_length = 2, validation_alias = "login")]
    user_name: String,
    #[field(ge = 18)]
    age: Option<u8>,
    tags: Vec<String>,
    address: Address,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Lax {
    n: i32,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(strict = true)]
struct Strict {
    n: i32,
    lenient: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct FieldStrict {
    #[field(strict)]
    n: i32,
    m: i32,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(str_strip_whitespace, str_to_lower)]
struct Lower {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(str_to_upper)]
struct Upper {
    name: String,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Basket {
    items: Vec<Line>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Line {
    #[field(ge = 1, le = 10)]
    qty: u32,
    #[field(min_length = 1, max_length = 3)]
    code: String,
}

fn ten() -> u32 {
    10
}

#[derive(Debug, Deserialize, Validate, Schema)]
struct Search {
    q: String,
    #[serde(default = "ten")]
    #[field(default = 10, le = 100)]
    limit: u32,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    active: bool,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "lowercase")]
enum Color {
    Red,
    #[serde(rename = "dark-green", alias = "green")]
    Green,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Palette {
    main: Color,
    accent: Option<Color>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Wrapper(#[field(min_length = 2)] String);

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Holder {
    wrapped: Wrapper,
}

fn positive_even(value: &i64) -> Result<(), FieldError> {
    if *value > 0 && value % 2 == 0 {
        Ok(())
    } else {
        Err(FieldError::new(
            "not_positive_even",
            "must be positive and even",
        ))
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Reusable {
    #[field(validator = positive_even)]
    a: i64,
    #[field(validator = positive_even)]
    b: i64,
}

// ---- hooks: ordering ------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Ordered {
    a: String,
    b: String,
    #[serde(default)]
    csv: Vec<String>,
}

#[model_hooks]
impl Ordered {
    #[model_validator(mode = "before")]
    fn poisoned(input: &mut Value) -> Result<(), FieldError> {
        if input.get("poison").is_some() {
            return Err(FieldError::new("poisoned", "poison key present"));
        }
        Ok(())
    }

    #[field_validator("csv", mode = "before")]
    fn split_csv(value: &mut Value) -> Result<(), FieldError> {
        if let Some(text) = value.as_str() {
            *value = Value::Array(text.split(',').map(|s| json!(s.trim())).collect());
        }
        Ok(())
    }

    #[field_validator("a")]
    fn a_long_enough(value: &str) -> Result<(), FieldError> {
        if value.len() < 3 {
            return Err(FieldError::new("a_short", "a is too short"));
        }
        Ok(())
    }

    #[field_validator("a", "b")]
    fn not_x(value: &str) -> Result<(), FieldError> {
        if value == "x" {
            return Err(FieldError::new("is_x", "must not be x"));
        }
        Ok(())
    }

    #[model_validator(mode = "after")]
    fn a_differs_from_b(&self) -> Result<(), FieldError> {
        if self.a == self.b {
            return Err(FieldError::new("same", "a and b must differ"));
        }
        Ok(())
    }

    #[model_validator]
    fn located(&self) -> Result<(), FieldError> {
        if self.csv.len() > 2 {
            return Err(FieldError::new("long", "too many").at("csv"));
        }
        Ok(())
    }
}

// ---- generics -------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Item {
    #[field(ge = 1)]
    qty: u32,
}

/// Generic models must opt in to hooks explicitly.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[model_config(hooks)]
struct Page<T> {
    items: Vec<T>,
    total: u32,
}

#[model_hooks]
impl<T> Page<T> {
    #[model_validator]
    fn total_matches(&self) -> Result<(), FieldError> {
        if usize::try_from(self.total).is_ok_and(|total| total == self.items.len()) {
            Ok(())
        } else {
            Err(FieldError::new(
                "total_mismatch",
                "total != number of items",
            ))
        }
    }

    #[computed_field]
    fn count(&self) -> u64 {
        u64::try_from(self.items.len()).unwrap_or(u64::MAX)
    }
}

// ---- serialization --------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, Validate, Schema)]
#[serde(rename_all = "camelCase")]
struct User {
    first_name: String,
    last_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nickname: Option<String>,
    #[field(exclude)]
    password: String,
    #[serde(default)]
    #[field(default = 1)]
    level: u8,
    #[serde(default = "default_theme")]
    #[field(default_factory = default_theme)]
    theme: String,
    #[serde(default)]
    bio: Option<String>,
}

fn default_theme() -> String {
    "light".to_owned()
}

#[model_hooks]
impl User {
    /// Full display name.
    #[computed_field(alias = "fullName")]
    fn full_name(&self) -> String {
        format!("{} {}", self.first_name, self.last_name)
    }

    #[computed_field]
    fn initials(&self) -> String {
        let first = self.first_name.chars().next().unwrap_or('?');
        let last = self.last_name.chars().next().unwrap_or('?');
        format!("{first}{last}")
    }

    #[field_serializer("last_name")]
    fn shout(value: &str) -> String {
        value.to_uppercase()
    }

    #[field_serializer("level")]
    fn level_as_value(value: &u8) -> Value {
        json!({ "value": value })
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Team {
    lead: User,
    members: Vec<User>,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(default)]
struct Config {
    name: String,
    retries: u8,
    verbose: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            name: "svc".to_owned(),
            retries: 3,
            verbose: false,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Enveloped {
    id: u32,
}

#[model_hooks]
impl Enveloped {
    #[model_serializer]
    fn envelope(&self, value: Value) -> Value {
        json!({ "data": value, "version": 1 })
    }
}

// ---------------------------------------------------------------------
// Routes
// ---------------------------------------------------------------------

#[post("/signup")]
async fn signup(Json(s): Json<Signup>) -> Json<Signup> {
    Json(s)
}

#[post("/lax")]
async fn lax(Json(v): Json<Lax>) -> Json<Lax> {
    Json(v)
}

#[post("/strict")]
async fn strict(Json(v): Json<Strict>) -> Json<Strict> {
    Json(v)
}

#[post("/field_strict")]
async fn field_strict(Json(v): Json<FieldStrict>) -> Json<FieldStrict> {
    Json(v)
}

#[post("/lower")]
async fn lower(Json(v): Json<Lower>) -> Json<Lower> {
    Json(v)
}

#[post("/upper")]
async fn upper(Json(v): Json<Upper>) -> Json<Upper> {
    Json(v)
}

#[post("/basket")]
async fn basket(Json(v): Json<Basket>) -> Json<Basket> {
    Json(v)
}

#[get("/search")]
async fn search(Query(q): Query<Search>) -> Json<Value> {
    Json(json!({ "q": q.q, "limit": q.limit, "tags": q.tags, "active": q.active }))
}

#[post("/palette")]
async fn palette(Json(v): Json<Palette>) -> Json<Palette> {
    Json(v)
}

#[post("/holder")]
async fn holder(Json(v): Json<Holder>) -> Json<Holder> {
    Json(v)
}

#[post("/reusable")]
async fn reusable(Json(v): Json<Reusable>) -> Json<Reusable> {
    Json(v)
}

#[post("/ordered")]
async fn ordered(Json(v): Json<Ordered>) -> Json<Ordered> {
    Json(v)
}

#[post("/pages")]
async fn pages(Json(v): Json<Page<Item>>) -> Json<Page<Item>> {
    Json(v)
}

fn sample_user() -> User {
    User {
        first_name: "Ada".into(),
        last_name: "Lovelace".into(),
        nickname: None,
        password: "hunter2".into(),
        level: 1,
        theme: "light".into(),
        bio: None,
    }
}

#[get("/user")]
async fn user() -> Json<User> {
    Json(sample_user())
}

#[get("/users")]
async fn users() -> Json<Vec<User>> {
    Json(vec![sample_user(), sample_user()])
}

#[get("/team")]
async fn team() -> Json<Team> {
    Json(Team {
        lead: sample_user(),
        members: vec![sample_user()],
    })
}

#[get("/user/slim")]
async fn user_slim() -> JsonDump<User> {
    JsonDump(
        sample_user(),
        DumpOptions::new()
            .exclude_none()
            .exclude(FieldSet::of(["initials"])),
    )
}

#[get("/user/only")]
async fn user_only() -> JsonDump<User> {
    JsonDump(
        sample_user(),
        DumpOptions::new().include(FieldSet::of(["firstName", "fullName"])),
    )
}

#[get("/user/defaults")]
async fn user_defaults() -> JsonDump<User> {
    JsonDump(sample_user(), DumpOptions::new().exclude_defaults())
}

#[get("/config")]
async fn config() -> JsonDump<Config> {
    JsonDump(Config::default(), DumpOptions::new().exclude_defaults())
}

#[get("/enveloped")]
async fn enveloped() -> Json<Enveloped> {
    Json(Enveloped { id: 4 })
}

fn app() -> App {
    App::new().title("validation").version("1").routes(routes![
        signup,
        lax,
        strict,
        field_strict,
        lower,
        upper,
        basket,
        search,
        palette,
        holder,
        reusable,
        ordered,
        pages,
        user,
        users,
        team,
        user_slim,
        user_only,
        user_defaults,
        config,
        enveloped,
    ])
}

fn client() -> TestClient {
    TestClient::new(app())
}

/// `(joined location, code)` of every error in a 422 body.
fn errors(res: &TestResponse) -> Vec<(String, String)> {
    assert_eq!(res.status, 422, "{}", res.text());
    let body = res.json::<Value>().unwrap();
    body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            let location: Vec<String> = e["location"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.to_string().trim_matches('"').to_owned())
                .collect();
            (location.join("."), e["code"].as_str().unwrap().to_owned())
        })
        .collect()
}

fn pair(location: &str, code: &str) -> (String, String) {
    (location.to_owned(), code.to_owned())
}

fn valid_signup() -> Value {
    json!({
        "userName": "bob",
        "age": 30,
        "tags": ["a"],
        "address": {"zip": "12345", "city": "X"},
    })
}

// ---------------------------------------------------------------------
// HTTP: validation
// ---------------------------------------------------------------------

#[tokio::test]
async fn every_error_is_reported_with_body_locations() {
    let res = client()
        .post_json("/signup", &json!({"login": "b", "age": 10, "extra": 1}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.login", "too_short"),
            pair("body.age", "greater_than_equal"),
            pair("body.tags", "missing"),
            pair("body.address", "missing"),
            pair("body.extra", "extra_forbidden"),
        ]
    );
}

#[tokio::test]
async fn valid_signup_round_trips_and_aliases_apply() {
    let mut body = valid_signup();
    body["login"] = json!("  alice ");
    body.as_object_mut().unwrap().remove("userName");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(res.status, 200, "{}", res.text());
    // Stripped by `str_strip_whitespace`, serialized under the serde key.
    assert_eq!(res.json::<Value>().unwrap()["userName"], "alice");
}

#[tokio::test]
async fn populate_by_name_accepts_the_rust_field_name() {
    let mut body = valid_signup();
    body.as_object_mut().unwrap().remove("userName");
    body["user_name"] = json!("carol");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(res.status, 200, "{}", res.text());
    assert_eq!(res.json::<Value>().unwrap()["userName"], "carol");
}

#[tokio::test]
async fn missing_aliased_field_is_reported_under_the_alias() {
    let mut body = valid_signup();
    body.as_object_mut().unwrap().remove("userName");
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(errors(&res), vec![pair("body.login", "missing")]);
}

#[tokio::test]
async fn nested_model_errors_carry_the_full_path() {
    let mut body = valid_signup();
    body["address"] = json!({"zip": "abc", "city": 5});
    let res = client().post_json("/signup", &body).await.unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.address.zip", "pattern_mismatch"),
            pair("body.address.city", "string_type"),
        ]
    );
}

#[tokio::test]
async fn lists_of_models_use_index_locations() {
    let res = client()
        .post_json(
            "/basket",
            &json!({"items": [
                {"qty": 1, "code": "ok"},
                {"qty": 99, "code": ""},
                {"qty": "3", "code": "abcd"},
            ]}),
        )
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.items.1.qty", "less_than_equal"),
            pair("body.items.1.code", "too_short"),
            pair("body.items.2.code", "too_long"),
        ]
    );
}

#[tokio::test]
async fn strict_and_lax_models_differ_on_coercion() {
    let c = client();
    let lax = c.post_json("/lax", &json!({"n": "5"})).await.unwrap();
    assert_eq!(lax.json::<Value>().unwrap(), json!({"n": 5}));

    let strict = c.post_json("/strict", &json!({"n": "5"})).await.unwrap();
    assert_eq!(errors(&strict), vec![pair("body.n", "int_type")]);

    // A strict field inside a lax model: only that field is strict.
    let field = c
        .post_json("/field_strict", &json!({"n": "5", "m": "6"}))
        .await
        .unwrap();
    assert_eq!(errors(&field), vec![pair("body.n", "int_type")]);
}

#[tokio::test]
async fn string_transforms_apply_before_checks() {
    let c = client();
    let res = c
        .post_json("/lower", &json!({"name": "  HeLLo "}))
        .await
        .unwrap();
    assert_eq!(res.json::<Value>().unwrap(), json!({"name": "hello"}));
    let res = c.post_json("/upper", &json!({"name": "hi"})).await.unwrap();
    assert_eq!(res.json::<Value>().unwrap(), json!({"name": "HI"}));
}

#[tokio::test]
async fn query_strings_coerce_leniently_and_report_query_locations() {
    let c = client();
    let ok = c
        .get("/search?q=rust&limit=25&tags=a&tags=b&active=yes")
        .await
        .unwrap();
    assert_eq!(
        ok.json::<Value>().unwrap(),
        json!({"q": "rust", "limit": 25, "tags": ["a", "b"], "active": true})
    );
    let defaults = c.get("/search?q=x").await.unwrap();
    assert_eq!(defaults.json::<Value>().unwrap()["tags"], json!([]));
    let bad = c.get("/search?limit=1000&active=maybe").await.unwrap();
    assert_eq!(
        errors(&bad),
        vec![
            pair("query.q", "missing"),
            pair("query.limit", "less_than_equal"),
            pair("query.active", "bool_parsing"),
        ]
    );
}

#[tokio::test]
async fn unit_enums_are_checked_against_their_serde_names() {
    let c = client();
    let ok = c
        .post_json("/palette", &json!({"main": "red", "accent": "green"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    let bad = c
        .post_json("/palette", &json!({"main": "Red", "accent": 5}))
        .await
        .unwrap();
    assert_eq!(
        errors(&bad),
        vec![pair("body.main", "enum"), pair("body.accent", "enum")]
    );
    let null = c
        .post_json("/palette", &json!({"main": "dark-green", "accent": null}))
        .await
        .unwrap();
    assert_eq!(null.status, 200);
}

#[tokio::test]
async fn newtypes_delegate_and_apply_their_constraints() {
    let c = client();
    let bad = c
        .post_json("/holder", &json!({"wrapped": "x"}))
        .await
        .unwrap();
    assert_eq!(errors(&bad), vec![pair("body.wrapped", "too_short")]);
    let ok = c
        .post_json("/holder", &json!({"wrapped": "xy"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
}

// ---------------------------------------------------------------------
// HTTP: hooks
// ---------------------------------------------------------------------

#[tokio::test]
async fn reusable_field_validators_run_after_deserialization() {
    let c = client();
    let res = c
        .post_json("/reusable", &json!({"a": 3, "b": -2}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.a", "not_positive_even"),
            pair("body.b", "not_positive_even"),
        ]
    );
    let ok = c
        .post_json("/reusable", &json!({"a": 2, "b": 4}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200);
}

#[tokio::test]
async fn hooks_run_in_declaration_order() {
    let c = client();
    // Field validators (declaration order), then model validators.
    let res = c
        .post_json("/ordered", &json!({"a": "x", "b": "x", "csv": "1,2,3"}))
        .await
        .unwrap();
    assert_eq!(
        errors(&res),
        vec![
            pair("body.a", "a_short"),
            pair("body.a", "is_x"),
            pair("body.b", "is_x"),
            pair("body", "same"),
            pair("body.csv", "long"),
        ]
    );
}

#[tokio::test]
async fn before_validators_see_raw_input() {
    let c = client();
    // The before-validator turns the string into a list before `Vec::prepare`.
    let ok = c
        .post_json("/ordered", &json!({"a": "abc", "b": "def", "csv": "1, 2"}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    assert_eq!(ok.json::<Value>().unwrap()["csv"], json!(["1", "2"]));

    // The model before-validator runs first and, like every prepare-phase
    // error, prevents the after phase.
    let poisoned = c
        .post_json("/ordered", &json!({"a": "x", "b": "x", "poison": 1}))
        .await
        .unwrap();
    assert_eq!(errors(&poisoned), vec![pair("body", "poisoned")]);
}

#[tokio::test]
async fn generic_models_validate_nested_items_and_run_hooks() {
    let c = client();
    let bad = c
        .post_json(
            "/pages",
            &json!({"items": [{"qty": 1}, {"qty": 0}], "total": 2}),
        )
        .await
        .unwrap();
    assert_eq!(
        errors(&bad),
        vec![pair("body.items.1.qty", "greater_than_equal")]
    );

    let mismatch = c
        .post_json("/pages", &json!({"items": [{"qty": 1}], "total": 5}))
        .await
        .unwrap();
    assert_eq!(errors(&mismatch), vec![pair("body", "total_mismatch")]);

    let ok = c
        .post_json("/pages", &json!({"items": [{"qty": 1}], "total": 1}))
        .await
        .unwrap();
    assert_eq!(ok.status, 200, "{}", ok.text());
    // Computed fields work on generic models that opt in to hooks.
    assert_eq!(ok.json::<Value>().unwrap()["count"], 1);
}

// ---------------------------------------------------------------------
// HTTP: serialization
// ---------------------------------------------------------------------

fn expected_user() -> Value {
    json!({
        "firstName": "Ada",
        "lastName": "LOVELACE",
        "level": {"value": 1},
        "theme": "light",
        "bio": null,
        "fullName": "Ada Lovelace",
        "initials": "AL",
    })
}

#[tokio::test]
async fn responses_include_computed_fields_and_serializers() {
    let res = client().get("/user").await.unwrap();
    assert_eq!(res.json::<Value>().unwrap(), expected_user());
}

#[tokio::test]
async fn nested_models_keep_computed_fields() {
    let c = client();
    let list = c.get("/users").await.unwrap().json::<Value>().unwrap();
    assert_eq!(list, json!([expected_user(), expected_user()]));
    let team = c.get("/team").await.unwrap().json::<Value>().unwrap();
    assert_eq!(team["lead"], expected_user());
    assert_eq!(team["members"][0], expected_user());
}

#[tokio::test]
async fn json_dump_options_apply() {
    let c = client();
    let slim = c.get("/user/slim").await.unwrap().json::<Value>().unwrap();
    let mut want = expected_user();
    want.as_object_mut().unwrap().remove("initials");
    want.as_object_mut().unwrap().remove("bio");
    assert_eq!(slim, want);

    let only = c.get("/user/only").await.unwrap().json::<Value>().unwrap();
    assert_eq!(
        only,
        json!({"firstName": "Ada", "fullName": "Ada Lovelace"})
    );
}

#[tokio::test]
async fn exclude_defaults_drops_fields_equal_to_their_default() {
    let c = client();
    let user = c
        .get("/user/defaults")
        .await
        .unwrap()
        .json::<Value>()
        .unwrap();
    // `level` (1) and `theme` ("light") equal their defaults; `bio` has a
    // serde default (`None`) too.
    assert!(user.get("theme").is_none(), "{user}");
    assert!(user.get("level").is_none(), "{user}");
    assert!(user.get("bio").is_none(), "{user}");
    assert_eq!(user["firstName"], "Ada");

    let cfg = c.get("/config").await.unwrap().json::<Value>().unwrap();
    assert_eq!(cfg, json!({}));
}

#[tokio::test]
async fn field_exclude_hides_the_field_from_dumps_only() {
    let user = client()
        .get("/user")
        .await
        .unwrap()
        .json::<Value>()
        .unwrap();
    assert!(user.get("password").is_none());
    // Plain Serde still sees it; `exclude` is a Dump concept.
    assert_eq!(
        serde_json::to_value(sample_user()).unwrap()["password"],
        "hunter2"
    );
    // Input still accepts it.
    let parsed: User = parse_value(
        json!({"firstName": "A", "lastName": "B", "password": "p"}),
        ValidationContext::new(),
    )
    .unwrap();
    assert_eq!(parsed.password, "p");
}

#[tokio::test]
async fn model_serializer_wraps_the_output() {
    let res = client().get("/enveloped").await.unwrap();
    assert_eq!(
        res.json::<Value>().unwrap(),
        json!({"data": {"id": 4}, "version": 1})
    );
}

// ---------------------------------------------------------------------
// Differential: prepare vs Serde
// ---------------------------------------------------------------------

fn common_inputs() -> Vec<Value> {
    vec![
        json!({}),
        json!(null),
        json!([]),
        json!(5),
        json!("text"),
        json!({"extra": 1}),
        json!({"n": 1}),
        json!({"n": "1"}),
        json!({"n": 1.5}),
        json!({"n": null}),
        json!({"n": true}),
        json!({"n": [1]}),
        json!({"n": {"x": 1}}),
        json!({"n": 3000000000_i64}),
        json!({"n": -3000000000_i64}),
        json!({"n": "abc"}),
        json!({"n": 2.0}),
    ]
}

fn inputs_for(specific: Vec<Value>) -> Vec<Value> {
    let mut all = common_inputs();
    all.extend(specific);
    all
}

#[test]
fn signup_prepare_agrees_with_serde() {
    let good_address = json!({"zip": "12345", "city": "c"});
    let inputs = inputs_for(vec![
        json!({}),
        json!({"userName": "ab", "tags": ["x"], "address": good_address}),
        json!({"userName": 5, "tags": "x", "address": good_address}),
        json!({"userName": "ab", "tags": [1], "address": good_address}),
        json!({"login": "ab", "userName": "cd", "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": 300, "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": null, "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": "19", "tags": [], "address": good_address}),
        json!({"userName": "ab", "age": -1, "tags": [], "address": good_address}),
        json!({"user_name": "ab", "tags": [], "address": good_address}),
        json!({"userName": "ab", "tags": [], "address": null}),
        json!({"userName": "ab", "tags": [], "address": {"zip": 1, "city": "c"}}),
        json!({"userName": "ab", "tags": [], "address": {"zip": "1", "city": null}}),
        json!({"userName": "ab", "tags": [], "address": [], "extra": true}),
        json!({"userName": "  ab  ", "tags": ["a", null], "address": good_address}),
    ]);
    assert!(inputs.len() >= 15);
    prepare_agrees_with_serde::<Signup>(&inputs).unwrap();
}

#[test]
fn line_and_basket_prepare_agrees_with_serde() {
    let inputs = inputs_for(vec![
        json!({"qty": 1, "code": "a"}),
        json!({"qty": "1", "code": "a"}),
        json!({"qty": 0, "code": "a"}),
        json!({"qty": 11, "code": "a"}),
        json!({"qty": -1, "code": "a"}),
        json!({"qty": 1.0, "code": "a"}),
        json!({"qty": 1.5, "code": "a"}),
        json!({"qty": 1, "code": ""}),
        json!({"qty": 1, "code": "abcd"}),
        json!({"qty": 1, "code": 5}),
        json!({"qty": 1}),
        json!({"code": "a"}),
        json!({"qty": null, "code": null}),
        json!({"qty": 4294967296_u64, "code": "a"}),
        json!({"qty": 1, "code": "a", "more": {"deep": [1, 2]}}),
    ]);
    prepare_agrees_with_serde::<Line>(&inputs).unwrap();
    let baskets = inputs_for(vec![
        json!({"items": []}),
        json!({"items": [{"qty": 1, "code": "a"}]}),
        json!({"items": [{"qty": 1, "code": "a"}, {"qty": "x"}]}),
        json!({"items": {}}),
        json!({"items": null}),
        json!({"items": [null]}),
        json!({"items": [[]]}),
        json!({"items": "one"}),
        json!({"items": [{"qty": 2, "code": "b"}, {"qty": 2, "code": "b"}]}),
        json!({"items": [1, 2, 3]}),
        json!({"items": [{}]}),
        json!({"item": []}),
        json!({"items": [{"qty": 1, "code": "a"}], "x": 1}),
    ]);
    prepare_agrees_with_serde::<Basket>(&baskets).unwrap();
}

#[test]
fn search_and_palette_prepare_agrees_with_serde() {
    let inputs = inputs_for(vec![
        json!({"q": "x"}),
        json!({"q": "x", "limit": 5}),
        json!({"q": "x", "limit": "5"}),
        json!({"q": "x", "limit": -5}),
        json!({"q": "x", "limit": 500}),
        json!({"q": "x", "tags": ["a"]}),
        json!({"q": "x", "tags": "a"}),
        json!({"q": "x", "tags": null}),
        json!({"q": "x", "active": "yes"}),
        json!({"q": "x", "active": 2}),
        json!({"q": "x", "active": 1}),
        json!({"q": 1}),
        json!({"limit": 1}),
        json!({"q": "x", "limit": null}),
        json!({"q": "x", "active": null}),
    ]);
    prepare_agrees_with_serde::<Search>(&inputs).unwrap();
    let palettes = inputs_for(vec![
        json!({"main": "red"}),
        json!({"main": "green"}),
        json!({"main": "dark-green"}),
        json!({"main": "blue"}),
        json!({"main": "Red"}),
        json!({"main": 1}),
        json!({"main": null}),
        json!({"main": "red", "accent": null}),
        json!({"main": "red", "accent": "nope"}),
        json!({"main": "red", "accent": []}),
        json!({"accent": "red"}),
        json!({"main": ["red"]}),
        json!({"main": "red", "extra": 1}),
    ]);
    prepare_agrees_with_serde::<Palette>(&palettes).unwrap();
}

// ---------------------------------------------------------------------
// OpenAPI
// ---------------------------------------------------------------------

fn doc() -> Value {
    app().openapi().unwrap().to_value()
}

#[test]
fn computed_fields_are_read_only_properties() {
    let doc = doc();
    let user = &doc["components"]["schemas"]["User"];
    let props = &user["properties"];
    assert_eq!(props["fullName"]["readOnly"], true);
    assert_eq!(props["fullName"]["type"], "string");
    assert_eq!(props["fullName"]["description"], "Full display name.");
    assert_eq!(props["initials"]["readOnly"], true);
    // Computed fields are never required inputs.
    let required: Vec<&str> = user["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!required.contains(&"fullName"));
    assert!(required.contains(&"firstName"));
    // Field constraints and defaults.
    assert_eq!(props["level"]["default"], 1);
}

#[test]
fn generic_computed_fields_appear_in_the_schema() {
    let mut registry = siderite::validation::SchemaRegistry::new();
    let inline = <Page<Item> as Schema>::schema(&mut registry).into_value();
    assert_eq!(inline["properties"]["count"]["readOnly"], true);
}

#[test]
fn extra_forbid_is_documented_and_the_document_is_valid_openapi_31() {
    let doc = doc();
    assert_eq!(
        doc["components"]["schemas"]["Signup"]["additionalProperties"],
        false
    );
    let meta: Value = serde_json::from_str(include_str!(
        "../../siderite-openapi/tests/fixtures/openapi-3.1-schema.json"
    ))
    .unwrap();
    let validator = jsonschema::validator_for(&meta).unwrap();
    let problems: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(problems.is_empty(), "{problems:#?}\n{doc:#}");
}

// ---------------------------------------------------------------------
// Key names agree between schema, prepare and dump
// ---------------------------------------------------------------------

#[test]
fn schema_prepare_and_dump_use_the_same_keys() {
    let schema = <Signup as Schema>::schema(&mut SchemaRegistry::new()).into_value();
    let mut keys: Vec<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(keys, ["address", "age", "tags", "userName"]);

    let parsed: Signup = parse_value(valid_signup(), ValidationContext::new()).unwrap();
    let dumped = siderite::validation::Dump::dump(&parsed, &DumpOptions::new()).unwrap();
    let mut dumped_keys: Vec<&str> = dumped
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    dumped_keys.sort_unstable();
    assert_eq!(dumped_keys, keys);
}

// ---------------------------------------------------------------------
// Constraint keys, config and edge cases
// ---------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[serde(deny_unknown_fields)]
struct Numbers {
    #[field(gt = 0, lt = 10)]
    open: i32,
    #[field(ge = -1.5, le = 2.5)]
    closed: f64,
    #[field(multiple_of = 5)]
    step: i64,
    #[field(multiple_of = 0.5)]
    half: f64,
    #[field(email, max_length = 20)]
    mail: String,
    #[field(url)]
    site: String,
    #[field(regex = "^[a-z]+$", max_digits = 5, decimal_places = 2)]
    slug: String,
    #[field(min_length = 1, max_length = 2)]
    list: Vec<u8>,
}

fn numbers() -> Value {
    json!({"open": 5, "closed": 0.5, "step": 10, "half": 1.5,
           "mail": "a@b.co", "site": "https://example.com/a", "slug": "abc", "list": [1]})
}

fn codes_of<T: serde::de::DeserializeOwned + Validate>(input: Value) -> Vec<(String, String)> {
    match parse_value::<T>(input, ValidationContext::new()) {
        Ok(_) => Vec::new(),
        Err(e) => e
            .errors
            .into_iter()
            .map(|e| {
                let loc: Vec<String> = e.location.iter().map(ToString::to_string).collect();
                (loc.join("."), e.code.into_owned())
            })
            .collect(),
    }
}

#[test]
fn numeric_string_and_collection_constraints() {
    assert!(codes_of::<Numbers>(numbers()).is_empty());
    let mut bad = numbers();
    bad["open"] = json!(10);
    bad["closed"] = json!(-2);
    bad["step"] = json!(7);
    bad["half"] = json!(1.2);
    bad["mail"] = json!("nope");
    bad["site"] = json!("not a url");
    bad["slug"] = json!("ABC");
    bad["list"] = json!([1, 2, 3]);
    assert_eq!(
        codes_of::<Numbers>(bad),
        vec![
            pair("open", "less_than"),
            pair("closed", "greater_than_equal"),
            pair("step", "multiple_of"),
            pair("half", "multiple_of"),
            pair("mail", "invalid_email"),
            pair("site", "url_parsing"),
            pair("slug", "pattern_mismatch"),
            pair("list", "too_long"),
        ]
    );
}

#[test]
fn deny_unknown_fields_means_extra_forbid() {
    let mut input = numbers();
    input["surprise"] = json!(1);
    assert_eq!(
        codes_of::<Numbers>(input),
        vec![pair("surprise", "extra_forbidden")]
    );
    let schema = <Numbers as Schema>::schema(&mut SchemaRegistry::new()).into_value();
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"]["slug"]["pattern"], "^[a-z]+$");
    assert_eq!(schema["properties"]["slug"]["x-max-digits"], 5);
    assert_eq!(schema["properties"]["half"]["multipleOf"], 0.5);
    assert_eq!(schema["properties"]["mail"]["format"], "email");
}

#[test]
fn two_names_for_one_field_are_a_duplicate() {
    // `populate_by_name`: the key, the validation alias and the Rust name all
    // reach `user_name`; the first one present wins, the rest are rejected.
    let mut input = valid_signup();
    input["login"] = json!("eve");
    input["user_name"] = json!("amy");
    assert_eq!(
        codes_of::<Signup>(input),
        vec![
            pair("login", "duplicate_field"),
            pair("user_name", "duplicate_field"),
        ]
    );
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(extra = "forbid")]
struct ServerStamped {
    name: String,
    #[serde(skip_deserializing)]
    stamp: u8,
}

#[test]
fn a_skip_deserializing_key_is_extra_under_forbid() {
    // Like Serde's `deny_unknown_fields`: the field is not an input, so its
    // key is an unknown one.
    assert!(codes_of::<ServerStamped>(json!({"name": "a"})).is_empty());
    assert_eq!(
        codes_of::<ServerStamped>(json!({"name": "a", "stamp": 1})),
        vec![pair("stamp", "extra_forbidden")]
    );
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(extra = "allow")]
struct Permissive {
    a: u8,
}

#[derive(Debug, Deserialize, Validate, Schema)]
#[model_config(hooks)]
struct Direct {
    a: u8,
}

#[model_hooks]
impl Direct {
    #[model_validator]
    fn nonzero(&self) -> Result<(), FieldError> {
        if self.a == 0 {
            Err(FieldError::new("zero", "a must not be zero"))
        } else {
            Ok(())
        }
    }
}

#[test]
fn extra_allow_and_direct_hooks_on_concrete_types() {
    let ok: Permissive = parse_value(json!({"a": 1, "x": 2}), ValidationContext::new()).unwrap();
    assert_eq!(ok.a, 1);
    assert_eq!(codes_of::<Direct>(json!({"a": 0})), vec![pair("", "zero")]);
    assert!(codes_of::<Direct>(json!({"a": 3})).is_empty());
}

/// Input-only types may derive `Schema` without `Serialize`, or opt out of
/// `Dump` explicitly.
#[derive(Debug, Deserialize, Validate, Schema)]
struct InputOnly {
    a: u8,
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
#[schema(no_dump)]
struct OptedOut {
    a: u8,
}

fn assert_dump<T: siderite::validation::Dump>() {}

#[test]
fn dump_applies_exactly_to_serializable_types() {
    assert_dump::<Signup>();
    assert_dump::<Page<Item>>();
    assert_dump::<Wrapper>();
    assert_dump::<Color>();
}

#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Nested {
    inner: Option<Wrapper>,
    list: Vec<Wrapper>,
}

#[test]
fn newtype_dump_delegates_and_nested_options_hold() {
    let n = Nested {
        inner: None,
        list: vec![Wrapper("ab".into())],
    };
    let dumped = siderite::validation::Dump::dump(&n, &DumpOptions::new().exclude_none()).unwrap();
    assert_eq!(dumped, json!({"list": ["ab"]}));
}

/// Generic models without `hooks` still validate their items.
#[derive(Debug, Serialize, Deserialize, Validate, Schema)]
struct Bag<T> {
    items: Vec<T>,
}

#[test]
fn generic_models_without_hooks_validate_items() {
    let got = codes_of::<Bag<Item>>(json!({"items": [{"qty": 0}, {"qty": 2}]}));
    assert_eq!(got, vec![pair("items.0.qty", "greater_than_equal")]);
    assert_dump::<Bag<Item>>();
}
