//! Security extractors: credential parsing, challenges and OpenAPI output.
#![allow(clippy::unwrap_used)]

use ::http::request::Parts;
use axumapi_core::http::{Method, StatusCode, header};
use axumapi_core::security::check_scopes;
use axumapi_core::*;
use axumapi_testkit::{TestClient, TestResponse};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

struct HeaderKey;
impl ApiKeySpec for HeaderKey {
    const NAME: &'static str = "X-API-Key";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Header;
    const SCHEME: &'static str = "HeaderKey";
}

struct QueryKey;
impl ApiKeySpec for QueryKey {
    const NAME: &'static str = "api_key";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Query;
    const SCHEME: &'static str = "QueryKey";
}

struct CookieKey;
impl ApiKeySpec for CookieKey {
    const NAME: &'static str = "session";
    const LOCATION: ApiKeyLocation = ApiKeyLocation::Cookie;
    const SCHEME: &'static str = "CookieKey";
}

struct Oauth;
impl OAuth2Spec for Oauth {
    const TOKEN_URL: &'static str = "/token";
    const SCOPES: &'static [(&'static str, &'static str)] =
        &[("read", "Read items"), ("admin", "Administer")];
    const SCHEME: &'static str = "OAuth2";
}

/// Principal whose granted scopes are encoded in the token: `user:read,write`.
#[derive(Debug)]
struct CurrentUser {
    name: String,
    scopes: Vec<String>,
}

impl Authenticate for CurrentUser {
    type Credentials = OAuth2PasswordBearer<Oauth>;

    async fn authenticate(
        credentials: Self::Credentials,
        required: &[&'static str],
        _parts: &Parts,
    ) -> Result<Self, ApiError> {
        let (name, scopes) = credentials
            .token
            .split_once(':')
            .ok_or_else(|| ApiError::new(StatusCode::UNAUTHORIZED, "bad token"))?;
        let scopes: Vec<String> = scopes.split(',').map(str::to_owned).collect();
        check_scopes(required, scopes.iter().map(String::as_str))?;
        Ok(Self {
            name: name.to_owned(),
            scopes,
        })
    }
}

scopes!(ReadScopes = ["read"]);
scopes!(AdminScopes = ["read", "admin"]);

async fn bearer(HttpBearer { token }: HttpBearer) -> String {
    token
}
async fn optional_bearer(auth: Option<HttpBearer>) -> String {
    auth.map_or_else(|| "anonymous".into(), |b| b.token)
}
async fn basic(HttpBasic { username, password }: HttpBasic) -> String {
    format!("{username}/{password}")
}
async fn header_key(key: ApiKey<HeaderKey>) -> String {
    key.key
}
async fn query_key(key: ApiKey<QueryKey>) -> String {
    key.key
}
async fn cookie_key(key: ApiKey<CookieKey>) -> String {
    key.key
}
async fn oauth_token(t: OAuth2PasswordBearer<Oauth>) -> String {
    t.token
}
async fn login(form: OAuth2PasswordRequestForm) -> Json<Value> {
    Json(json!({
        "username": form.username,
        "password": form.password,
        "scopes": form.scopes,
        "client_id": form.client_id,
    }))
}
async fn open_secured(Security(user, _): Security<CurrentUser>) -> String {
    user.name
}
async fn read_secured(Security(user, _): Security<CurrentUser, ReadScopes>) -> String {
    format!("{}:{}", user.name, user.scopes.join("+"))
}
async fn admin_secured(Security(user, _): Security<CurrentUser, AdminScopes>) -> String {
    user.name
}
async fn both(_a: HttpBearer, _b: ApiKey<HeaderKey>) -> &'static str {
    "both"
}
async fn key_and_optional_bearer(_k: ApiKey<HeaderKey>, _b: Option<HttpBearer>) -> &'static str {
    "ok"
}
async fn double(_a: HttpBearer, _b: HttpBearer) -> &'static str {
    "double"
}

fn app() -> App {
    App::new()
        .route("/bearer", get(bearer))
        .route("/optional", get(optional_bearer))
        .route("/basic", get(basic))
        .route("/key/header", get(header_key))
        .route("/key/query", get(query_key))
        .route("/key/cookie", get(cookie_key))
        .route("/oauth", get(oauth_token))
        .route("/token", post(login))
        .route("/secured", get(open_secured))
        .route("/secured/read", get(read_secured))
        .route("/secured/admin", get(admin_secured))
        .route("/double", get(double))
        .route("/both", get(both))
        .route("/key-optional-bearer", get(key_and_optional_bearer))
}

async fn call(client: &TestClient, path: &str, headers: &[(&str, &str)]) -> TestResponse {
    let mut req = ::http::Request::builder().method(Method::GET).uri(path);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    client.send(req.body(Body::empty()).unwrap()).await.unwrap()
}

fn challenge(response: &TestResponse) -> Option<&str> {
    response
        .headers
        .get(header::WWW_AUTHENTICATE)
        .and_then(|v| v.to_str().ok())
}

fn basic_header(credentials: &str) -> String {
    format!("Basic {}", STANDARD.encode(credentials))
}

#[tokio::test]
async fn bearer_extracts_token() {
    let client = TestClient::new(app());
    let ok = call(&client, "/bearer", &[("authorization", "Bearer abc.def")]).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.text(), "abc.def");
    // The scheme name is case-insensitive.
    let lower = call(&client, "/bearer", &[("authorization", "bearer  xyz")]).await;
    assert_eq!(lower.text(), "xyz");
}

#[tokio::test]
async fn bearer_rejects_missing_and_malformed_credentials() {
    let client = TestClient::new(app());
    for headers in [
        vec![],
        vec![("authorization", "Bearer")],
        vec![("authorization", "Bearer ")],
        vec![("authorization", "Bearer a b")],
        vec![("authorization", "Basic abc")],
        vec![("authorization", "abc")],
    ] {
        let response = call(&client, "/bearer", &headers).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{headers:?}");
        assert_eq!(challenge(&response), Some("Bearer"), "{headers:?}");
        assert_eq!(response.content_type(), Some("application/problem+json"));
    }
}

#[tokio::test]
async fn optional_bearer_is_none_without_credentials() {
    let client = TestClient::new(app());
    assert_eq!(call(&client, "/optional", &[]).await.text(), "anonymous");
    let with = call(&client, "/optional", &[("authorization", "Bearer t1")]).await;
    assert_eq!(with.text(), "t1");
}

#[tokio::test]
async fn basic_decodes_credentials() {
    let client = TestClient::new(app());
    let auth = basic_header("ann:pa:ss");
    let ok = call(&client, "/basic", &[("authorization", &auth)]).await;
    assert_eq!(ok.status, StatusCode::OK);
    assert_eq!(ok.text(), "ann/pa:ss");
}

#[tokio::test]
async fn basic_rejects_missing_and_malformed_credentials() {
    let client = TestClient::new(app());
    let no_colon = basic_header("nocolon");
    let bad_base64 = "Basic !!!".to_owned();
    for headers in [
        vec![],
        vec![("authorization", "Bearer abc")],
        vec![("authorization", bad_base64.as_str())],
        vec![("authorization", no_colon.as_str())],
    ] {
        let response = call(&client, "/basic", &headers).await;
        assert_eq!(response.status, StatusCode::UNAUTHORIZED, "{headers:?}");
        assert_eq!(challenge(&response), Some(r#"Basic realm="api""#));
    }
}

#[tokio::test]
async fn api_keys_are_read_from_their_location() {
    let client = TestClient::new(app());
    let header = call(&client, "/key/header", &[("x-api-key", "k1")]).await;
    assert_eq!(header.text(), "k1");
    let query = call(&client, "/key/query?other=1&api_key=k2", &[]).await;
    assert_eq!(query.text(), "k2");
    let cookie = call(&client, "/key/cookie", &[("cookie", "a=b; session=k3")]).await;
    assert_eq!(cookie.text(), "k3");
}

#[tokio::test]
async fn missing_or_empty_api_keys_are_unauthorized_without_challenge() {
    let client = TestClient::new(app());
    for (path, headers) in [
        ("/key/header", vec![]),
        ("/key/header", vec![("x-api-key", "")]),
        ("/key/query", vec![]),
        ("/key/query?api_key=", vec![]),
        ("/key/query?other=1", vec![]),
        ("/key/cookie", vec![]),
        ("/key/cookie", vec![("cookie", "other=1")]),
        ("/key/cookie", vec![("cookie", "session=")]),
    ] {
        let response = call(&client, path, &headers).await;
        assert_eq!(
            response.status,
            StatusCode::UNAUTHORIZED,
            "{path} {headers:?}"
        );
        assert_eq!(challenge(&response), None);
    }
}

#[tokio::test]
async fn oauth2_bearer_extracts_or_challenges() {
    let client = TestClient::new(app());
    let ok = call(&client, "/oauth", &[("authorization", "Bearer tok")]).await;
    assert_eq!(ok.text(), "tok");
    let missing = call(&client, "/oauth", &[]).await;
    assert_eq!(missing.status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&missing), Some("Bearer"));
}

#[tokio::test]
async fn password_form_parses_fields() {
    let client = TestClient::new(app());
    let body = "grant_type=password&username=ann&password=p%40ss&scope=read+admin&client_id=web";
    let response = client
        .post_raw(
            "/token",
            "application/x-www-form-urlencoded",
            body.as_bytes().to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(
        response.json::<Value>().unwrap(),
        json!({
            "username": "ann",
            "password": "p@ss",
            "scopes": ["read", "admin"],
            "client_id": "web",
        })
    );
}

#[tokio::test]
async fn password_form_validates_input() {
    let client = TestClient::new(app());
    let missing = client
        .post_raw(
            "/token",
            "application/x-www-form-urlencoded",
            b"username=ann".to_vec(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status, StatusCode::UNPROCESSABLE_ENTITY);
    let problem: Value = missing.json().unwrap();
    assert_eq!(problem["errors"][0]["code"], "missing");

    let wrong_type = client
        .post_json("/token", &json!({"username": "a", "password": "b"}))
        .await
        .unwrap();
    assert_eq!(wrong_type.status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn security_enforces_scopes() {
    let client = TestClient::new(app());
    async fn as_user(client: &TestClient, path: &str, token: &str) -> TestResponse {
        let auth = format!("Bearer {token}");
        call(client, path, &[("authorization", auth.as_str())]).await
    }

    let open = as_user(&client, "/secured", "ann:none").await;
    assert_eq!(open.status, StatusCode::OK);
    assert_eq!(open.text(), "ann");

    let read = as_user(&client, "/secured/read", "ann:read,write").await;
    assert_eq!(read.text(), "ann:read+write");

    let denied = as_user(&client, "/secured/admin", "ann:read").await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    let problem: Value = denied.json().unwrap();
    assert_eq!(problem["detail"], "Missing required scopes: admin.");

    let allowed = as_user(&client, "/secured/admin", "root:read,admin").await;
    assert_eq!(allowed.text(), "root");

    let anonymous = call(&client, "/secured/read", &[]).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    assert_eq!(challenge(&anonymous), Some("Bearer"));
    let invalid = as_user(&client, "/secured/read", "no-colon").await;
    assert_eq!(invalid.status, StatusCode::UNAUTHORIZED);
}

fn doc() -> Value {
    app().openapi().unwrap().to_value()
}

#[test]
fn security_schemes_are_registered() {
    let schemes = &doc()["components"]["securitySchemes"];
    assert_eq!(
        schemes["HTTPBearer"],
        json!({"type": "http", "scheme": "bearer"})
    );
    assert_eq!(
        schemes["HTTPBasic"],
        json!({"type": "http", "scheme": "basic"})
    );
    assert_eq!(
        schemes["HeaderKey"],
        json!({"type": "apiKey", "in": "header", "name": "X-API-Key"})
    );
    assert_eq!(
        schemes["QueryKey"],
        json!({"type": "apiKey", "in": "query", "name": "api_key"})
    );
    assert_eq!(
        schemes["CookieKey"],
        json!({"type": "apiKey", "in": "cookie", "name": "session"})
    );
    assert_eq!(
        schemes["OAuth2"],
        json!({
            "type": "oauth2",
            "flows": {"password": {
                "tokenUrl": "/token",
                "scopes": {"read": "Read items", "admin": "Administer"},
            }},
        })
    );
}

#[test]
fn operations_carry_security_requirements() {
    let doc = doc();
    let security = |path: &str| doc["paths"][path]["get"]["security"].clone();
    assert_eq!(security("/bearer"), json!([{"HTTPBearer": []}]));
    assert_eq!(security("/basic"), json!([{"HTTPBasic": []}]));
    assert_eq!(security("/key/header"), json!([{"HeaderKey": []}]));
    assert_eq!(security("/key/query"), json!([{"QueryKey": []}]));
    assert_eq!(security("/key/cookie"), json!([{"CookieKey": []}]));
    assert_eq!(security("/oauth"), json!([{"OAuth2": []}]));
    assert_eq!(security("/secured"), json!([{"OAuth2": []}]));
    assert_eq!(security("/secured/read"), json!([{"OAuth2": ["read"]}]));
    assert_eq!(
        security("/secured/admin"),
        json!([{"OAuth2": ["read", "admin"]}])
    );
    assert!(doc["paths"]["/token"]["post"].get("security").is_none());
}

#[test]
fn duplicate_requirements_are_not_repeated() {
    assert_eq!(
        doc()["paths"]["/double"]["get"]["security"],
        json!([{"HTTPBearer": []}])
    );
}

#[test]
fn schemes_of_one_handler_are_all_required() {
    assert_eq!(
        doc()["paths"]["/both"]["get"]["security"],
        json!([{"HTTPBearer": [], "HeaderKey": []}])
    );
}

#[test]
fn optional_schemes_are_documented_as_alternatives() {
    let doc = doc();
    assert_eq!(
        doc["paths"]["/optional"]["get"]["security"],
        json!([{}, {"HTTPBearer": []}])
    );
    assert_eq!(
        doc["paths"]["/key-optional-bearer"]["get"]["security"],
        json!([{"HeaderKey": []}, {"HeaderKey": [], "HTTPBearer": []}])
    );
}

#[test]
fn password_form_is_documented_as_a_form_body() {
    let body = &doc()["paths"]["/token"]["post"]["requestBody"];
    assert_eq!(body["required"], true);
    let schema = &body["content"]["application/x-www-form-urlencoded"]["schema"];
    assert_eq!(schema["required"], json!(["username", "password"]));
    assert!(schema["properties"]["client_secret"].is_object());
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
    assert!(errors.is_empty(), "{errors:#?}");
}

#[test]
fn check_scopes_reports_every_missing_scope() {
    assert!(check_scopes(&[], []).is_ok());
    let err = check_scopes(&["a", "b", "c"], ["b"]).unwrap_err();
    assert_eq!(err.status(), StatusCode::FORBIDDEN);
    assert_eq!(err.detail(), Some("Missing required scopes: a, c."));
}

#[test]
fn scopes_macro_declares_marker_types() {
    assert_eq!(<ReadScopes as Scopes>::SCOPES, ["read"]);
    assert_eq!(<AdminScopes as Scopes>::SCOPES, ["read", "admin"]);
    assert!(<NoScopes as Scopes>::SCOPES.is_empty());
}
