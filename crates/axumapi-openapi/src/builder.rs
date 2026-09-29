//! Assembles an [`OpenApi`] document from described operations.

use crate::model::{
    Components, HttpMethod, Info, OPENAPI_VERSION, OpenApi, Operation, Parameter,
    ParameterLocation, PathItem,
};
use axumapi_validation::{SchemaConflict, SchemaObject, SchemaRegistry};
use std::collections::BTreeMap;
use thiserror::Error;

/// Errors produced while building a document.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum OpenApiError {
    /// The same method was registered twice on one path.
    #[error("duplicate operation {method} {path}")]
    DuplicateOperation {
        /// Path template.
        path: String,
        /// Lower-case method.
        method: &'static str,
    },
    /// Two operations share an `operationId`.
    #[error("duplicate operationId `{0}`")]
    DuplicateOperationId(String),
    /// Two Rust types claimed one schema name.
    #[error("schema name conflicts: {0:?}")]
    SchemaConflicts(Vec<SchemaConflict>),
}

/// Collects operations and produces a document.
///
/// Operations are described against the builder's shared [`SchemaRegistry`]
/// so every named schema appears exactly once in `components.schemas`.
#[derive(Debug)]
pub struct DocumentBuilder {
    info: Info,
    registry: SchemaRegistry,
    paths: BTreeMap<String, PathItem>,
    errors: Vec<OpenApiError>,
}

impl DocumentBuilder {
    /// Start a document.
    pub fn new(title: impl Into<String>, version: impl Into<String>) -> Self {
        Self {
            info: Info {
                title: title.into(),
                version: version.into(),
                description: None,
            },
            registry: SchemaRegistry::new(),
            paths: BTreeMap::new(),
            errors: Vec::new(),
        }
    }

    /// Set the API description.
    #[must_use]
    pub fn description(mut self, description: impl Into<String>) -> Self {
        self.info.description = Some(description.into());
        self
    }

    /// Registry to describe operations against.
    pub fn registry(&mut self) -> &mut SchemaRegistry {
        &mut self.registry
    }

    /// Add an operation at `path` (axumapi `{param}` template syntax).
    ///
    /// Unnamed path parameters are named positionally from the template, and
    /// template parameters with no documented schema get a `string` schema so
    /// the document stays valid.
    pub fn add_operation(&mut self, path: &str, method: HttpMethod, mut op: Operation) {
        name_path_parameters(path, &mut op);
        let item = self.paths.entry(path.to_owned()).or_default();
        if item.0.contains_key(method.as_str()) {
            self.errors.push(OpenApiError::DuplicateOperation {
                path: path.to_owned(),
                method: method.as_str(),
            });
            return;
        }
        if op.responses.is_empty() {
            op.add_response("default", "Response", None);
        }
        item.0.insert(method.as_str(), op);
    }

    /// Finish the document.
    ///
    /// # Errors
    /// Returns the first structural error found (duplicate operations or
    /// operation ids, schema name conflicts).
    pub fn build(mut self) -> Result<OpenApi, OpenApiError> {
        let mut ids = std::collections::BTreeSet::new();
        for op in self.paths.values().flat_map(|p| p.0.values()) {
            if let Some(id) = &op.operation_id
                && !ids.insert(id.clone())
            {
                self.errors
                    .push(OpenApiError::DuplicateOperationId(id.clone()));
            }
        }
        if let Some(err) = self.errors.into_iter().next() {
            return Err(err);
        }
        let security_schemes = self.registry.take_security_schemes();
        let schemas = self
            .registry
            .into_components()
            .map_err(OpenApiError::SchemaConflicts)?;
        Ok(OpenApi {
            openapi: OPENAPI_VERSION,
            info: self.info,
            paths: self.paths,
            components: Components {
                schemas,
                security_schemes,
            },
        })
    }
}

/// Parameter names in a `{param}` path template, in order.
pub(crate) fn template_params(path: &str) -> Vec<&str> {
    path.split('/')
        .filter_map(|seg| seg.strip_prefix('{')?.strip_suffix('}'))
        .map(|name| name.trim_start_matches('*'))
        .collect()
}

fn name_path_parameters(path: &str, op: &mut Operation) {
    let names = template_params(path);
    let mut unnamed = names
        .iter()
        .filter(|n| {
            !op.parameters
                .iter()
                .any(|p| p.location == ParameterLocation::Path && p.name == **n)
        })
        .copied()
        .collect::<Vec<_>>()
        .into_iter();
    for p in op
        .parameters
        .iter_mut()
        .filter(|p| p.location == ParameterLocation::Path && p.name.is_empty())
    {
        if let Some(name) = unnamed.next() {
            p.name = name.to_owned();
        }
    }
    // Drop placeholders that did not match a template segment.
    op.parameters
        .retain(|p| !(p.location == ParameterLocation::Path && p.name.is_empty()));
    for name in names {
        let documented = op
            .parameters
            .iter()
            .any(|p| p.location == ParameterLocation::Path && p.name == name);
        if !documented {
            op.parameters.push(Parameter::new(
                name,
                ParameterLocation::Path,
                true,
                SchemaObject::of_type("string"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parsing() {
        assert_eq!(template_params("/a/{id}/b/{*rest}"), vec!["id", "rest"]);
        assert!(template_params("/plain").is_empty());
    }

    #[test]
    fn unnamed_path_params_are_named_positionally() {
        let mut op = Operation::default();
        op.add_parameter(Parameter::new(
            "",
            ParameterLocation::Path,
            true,
            SchemaObject::of_type("integer"),
        ));
        let mut b = DocumentBuilder::new("t", "1");
        b.add_operation("/orgs/{org}/users/{id}", HttpMethod::Get, op);
        let doc = b.build().unwrap();
        let params = &doc.paths["/orgs/{org}/users/{id}"].0["get"].parameters;
        assert_eq!(params[0].name, "org");
        assert_eq!(params[0].schema, SchemaObject::of_type("integer"));
        assert_eq!(params[1].name, "id");
        assert_eq!(params[1].schema, SchemaObject::of_type("string"));
    }

    #[test]
    fn duplicates_are_errors() {
        let mut b = DocumentBuilder::new("t", "1");
        b.add_operation("/x", HttpMethod::Get, Operation::default());
        b.add_operation("/x", HttpMethod::Get, Operation::default());
        assert!(matches!(
            b.build(),
            Err(OpenApiError::DuplicateOperation { .. })
        ));

        let mut b = DocumentBuilder::new("t", "1");
        let op = Operation {
            operation_id: Some("a".into()),
            ..Operation::default()
        };
        b.add_operation("/x", HttpMethod::Get, op.clone());
        b.add_operation("/y", HttpMethod::Get, op);
        assert_eq!(
            b.build(),
            Err(OpenApiError::DuplicateOperationId("a".into()))
        );
    }
}
