//! Typed OpenAPI 3.1 structures (the subset axumapi generates).

use axumapi_validation::SchemaObject;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// OpenAPI version emitted by this crate.
pub const OPENAPI_VERSION: &str = "3.1.0";

/// Root document.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenApi {
    /// Always [`OPENAPI_VERSION`].
    pub openapi: &'static str,
    /// API metadata.
    pub info: Info,
    /// Operations grouped by path template.
    pub paths: BTreeMap<String, PathItem>,
    /// Reusable components.
    #[serde(skip_serializing_if = "Components::is_empty")]
    pub components: Components,
}

impl OpenApi {
    /// Serialize to a JSON value.
    pub fn to_value(&self) -> Value {
        // Serialization of these plain structs cannot fail.
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// `info` object.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Info {
    /// API title.
    pub title: String,
    /// API version (not the OpenAPI version).
    pub version: String,
    /// Longer description (CommonMark).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// `components` object.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Components {
    /// Named schemas.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub schemas: BTreeMap<String, SchemaObject>,
    /// Security schemes registered by security extractors.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub security_schemes: BTreeMap<String, Value>,
}

impl Components {
    /// Whether nothing would be emitted.
    pub fn is_empty(&self) -> bool {
        self.schemas.is_empty() && self.security_schemes.is_empty()
    }
}

/// HTTP methods representable in a path item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[allow(missing_docs)]
pub enum HttpMethod {
    Get,
    Put,
    Post,
    Delete,
    Options,
    Head,
    Patch,
    Trace,
}

impl HttpMethod {
    /// Lower-case name used as the path item key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Put => "put",
            Self::Post => "post",
            Self::Delete => "delete",
            Self::Options => "options",
            Self::Head => "head",
            Self::Patch => "patch",
            Self::Trace => "trace",
        }
    }
}

/// Operations available on one path.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PathItem(pub BTreeMap<&'static str, Operation>);

/// A single API operation.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Operation {
    /// Grouping tags.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// One-line summary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Longer description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Unique operation id.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
    /// Parameters (path, query, header, cookie).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub parameters: Vec<Parameter>,
    /// Request body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_body: Option<RequestBody>,
    /// Responses keyed by status code (`"200"`) or `"default"`.
    pub responses: BTreeMap<String, Response>,
    /// Deprecated flag.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
    /// Security requirements.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub security: Vec<BTreeMap<String, Vec<String>>>,
}

impl Operation {
    /// Add a parameter unless one with the same name and location exists.
    ///
    /// Path parameters may be pushed with an empty `name`; the builder names
    /// them positionally from the path template.
    pub fn add_parameter(&mut self, parameter: Parameter) {
        let duplicate = !parameter.name.is_empty()
            && self
                .parameters
                .iter()
                .any(|p| p.name == parameter.name && p.location == parameter.location);
        if !duplicate {
            self.parameters.push(parameter);
        }
    }

    /// Set the request body to `schema` with the given media type.
    pub fn set_request_body(&mut self, media_type: &str, schema: SchemaObject, required: bool) {
        self.request_body = Some(RequestBody {
            description: None,
            content: BTreeMap::from([(
                media_type.to_owned(),
                MediaType {
                    schema: Some(schema),
                },
            )]),
            required,
        });
    }

    /// Add (or replace) a response.
    pub fn add_response(
        &mut self,
        status: impl Into<String>,
        description: impl Into<String>,
        content: Option<(&str, SchemaObject)>,
    ) {
        let content = content
            .map(|(mime, schema)| {
                BTreeMap::from([(
                    mime.to_owned(),
                    MediaType {
                        schema: Some(schema),
                    },
                )])
            })
            .unwrap_or_default();
        self.responses.insert(
            status.into(),
            Response {
                description: description.into(),
                content,
            },
        );
    }

    /// Move the response documented under `from` to `to` (status override).
    pub fn remap_response(&mut self, from: &str, to: &str) {
        if let Some(r) = self.responses.remove(from) {
            self.responses.insert(to.to_owned(), r);
        }
    }
}

/// Parameter location.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[allow(missing_docs)]
pub enum ParameterLocation {
    Path,
    Query,
    Header,
    Cookie,
}

/// Operation parameter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Parameter {
    /// Parameter name.
    pub name: String,
    /// Where the parameter is read from.
    #[serde(rename = "in")]
    pub location: ParameterLocation,
    /// Required flag (always `true` for path parameters).
    pub required: bool,
    /// Parameter schema.
    pub schema: SchemaObject,
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

impl Parameter {
    /// New parameter.
    pub fn new(
        name: impl Into<String>,
        location: ParameterLocation,
        required: bool,
        schema: SchemaObject,
    ) -> Self {
        Self {
            name: name.into(),
            location,
            required,
            schema,
            description: None,
        }
    }
}

/// Request body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RequestBody {
    /// Description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Content by media type.
    pub content: BTreeMap<String, MediaType>,
    /// Required flag.
    pub required: bool,
}

/// Media type entry.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MediaType {
    /// Payload schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schema: Option<SchemaObject>,
}

/// Response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Response {
    /// Required description.
    pub description: String,
    /// Content by media type.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub content: BTreeMap<String, MediaType>,
}
