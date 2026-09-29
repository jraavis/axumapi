//! Validates generated documents against the official OpenAPI 3.1 JSON Schema.
#![allow(clippy::unwrap_used)]

use axumapi_openapi::{
    DocumentBuilder, HttpMethod, Operation, Parameter, ParameterLocation, Schema, SchemaObject,
    SchemaRegistry,
};
use serde_json::{Value, json};

struct User;
impl Schema for User {
    fn schema_name() -> Option<&'static str> {
        Some("User")
    }
    fn schema(r: &mut SchemaRegistry) -> SchemaObject {
        SchemaObject::of_type("object")
            .with(
                "properties",
                json!({ "name": r.subschema::<String>(), "friends": r.subschema::<Vec<User>>() }),
            )
            .with("required", json!(["name"]))
    }
}

fn meta_schema() -> Value {
    serde_json::from_str(include_str!("fixtures/openapi-3.1-schema.json")).unwrap()
}

#[test]
fn generated_document_is_valid_openapi_31() {
    let mut b = DocumentBuilder::new("Example", "1.0.0").description("demo");
    let mut op = Operation {
        summary: Some("Get user".into()),
        tags: vec!["users".into()],
        ..Default::default()
    };
    op.add_parameter(Parameter::new(
        "",
        ParameterLocation::Path,
        true,
        SchemaObject::of_type("integer"),
    ));
    op.add_parameter(Parameter::new(
        "verbose",
        ParameterLocation::Query,
        false,
        SchemaObject::of_type("boolean"),
    ));
    let user = b.registry().subschema::<User>();
    op.add_response("200", "OK", Some(("application/json", user.clone())));
    b.add_operation("/users/{id}", HttpMethod::Get, op);

    let mut create = Operation {
        operation_id: Some("create_user".into()),
        ..Default::default()
    };
    create.set_request_body("application/json", user.clone(), true);
    create.add_response("201", "Created", Some(("application/json", user)));
    b.add_operation("/users", HttpMethod::Post, create);
    b.add_operation("/health", HttpMethod::Get, Operation::default());

    let doc = b.build().unwrap().to_value();
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(doc["components"]["schemas"].as_object().unwrap().len(), 1);

    let validator = jsonschema::validator_for(&meta_schema()).unwrap();
    let errors: Vec<String> = validator.iter_errors(&doc).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

#[test]
fn meta_schema_rejects_invalid_documents() {
    let validator = jsonschema::validator_for(&meta_schema()).unwrap();
    assert!(!validator.is_valid(&json!({"openapi": "3.1.0"})));
}
