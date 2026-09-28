use std::collections::BTreeMap;
use std::path::PathBuf;

use serde_json::Value;

use crate::{Dokploy, Error};

/// HTTP methods exposed by the pinned Dokploy OpenAPI contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImperativeMethod {
    Get,
    Post,
}

/// A raw operation request assembled by generated command definitions.
///
/// Query and body values remain JSON so incomplete OpenAPI response schemas do
/// not leak generated placeholder types into the public SDK interface.
pub struct ImperativeRequest {
    pub(crate) operation: &'static str,
    pub(crate) method: ImperativeMethod,
    pub(crate) query: BTreeMap<String, Value>,
    pub(crate) body: ImperativeBody,
}

pub(crate) enum ImperativeBody {
    None,
    Json(Value),
    Multipart(Vec<MultipartField>),
}

pub(crate) enum MultipartField {
    Text { name: String, value: String },
    File { name: String, path: PathBuf },
}

impl std::fmt::Debug for ImperativeRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let body_kind = match self.body {
            ImperativeBody::None => "none",
            ImperativeBody::Json(_) => "json",
            ImperativeBody::Multipart(_) => "multipart",
        };

        formatter
            .debug_struct("ImperativeRequest")
            .field("operation", &self.operation)
            .field("method", &self.method)
            .field("query_names", &self.query.keys().collect::<Vec<_>>())
            .field("body_kind", &body_kind)
            .finish_non_exhaustive()
    }
}

impl ImperativeRequest {
    /// Starts a raw GET request for a pinned operation.
    #[must_use]
    pub fn get(operation: &'static str) -> Self {
        Self::new(operation, ImperativeMethod::Get)
    }

    /// Starts a raw POST request for a pinned operation.
    #[must_use]
    pub fn post(operation: &'static str) -> Self {
        Self::new(operation, ImperativeMethod::Post)
    }

    fn new(operation: &'static str, method: ImperativeMethod) -> Self {
        Self {
            operation,
            method,
            query: BTreeMap::new(),
            body: ImperativeBody::None,
        }
    }

    /// Adds one query parameter using its wire name.
    #[must_use]
    pub fn query(mut self, name: impl Into<String>, value: impl Into<Value>) -> Self {
        self.query.insert(name.into(), value.into());
        self
    }

    /// Sets the JSON request body.
    #[must_use]
    pub fn body(mut self, body: Value) -> Self {
        self.body = ImperativeBody::Json(body);
        self
    }

    /// Adds a text field to a multipart request body.
    #[must_use]
    pub fn multipart_text(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.multipart_fields().push(MultipartField::Text {
            name: name.into(),
            value: value.into(),
        });
        self
    }

    /// Adds a file field to a multipart request body.
    #[must_use]
    pub fn multipart_file(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.multipart_fields().push(MultipartField::File {
            name: name.into(),
            path: path.into(),
        });
        self
    }

    fn multipart_fields(&mut self) -> &mut Vec<MultipartField> {
        if matches!(self.body, ImperativeBody::None) {
            self.body = ImperativeBody::Multipart(Vec::new());
        }
        match &mut self.body {
            ImperativeBody::Multipart(fields) => fields,
            ImperativeBody::None | ImperativeBody::Json(_) => {
                panic!("multipart fields cannot be combined with a JSON body")
            }
        }
    }
}

/// Broad raw access to the pinned Dokploy API contract.
pub struct Imperative<'a> {
    client: &'a Dokploy,
}

impl<'a> Imperative<'a> {
    pub(crate) fn new(client: &'a Dokploy) -> Self {
        Self { client }
    }

    /// Executes one operation and returns its unmodified JSON response.
    ///
    /// POST operations are never retried. GET operations are eligible for the
    /// bounded transient retry policy configured by [`Dokploy`].
    pub async fn execute(&self, request: ImperativeRequest) -> Result<Value, Error> {
        self.client.execute_imperative(request).await
    }
}
