//! The seam between the engine and Dokploy.
//!
//! The engine speaks in the terms specs use: an operation id such as `registry.all`, a
//! query, and a JSON body. A [`Transport`] sends that and returns the JSON response. The
//! real implementation is [`Dokploy`] over HTTP; tests and the simulator provide others, so
//! the engine runs against an in-memory Dokploy without a socket (ADR 0003, ADR 0015).

use std::collections::BTreeMap;
use std::future::Future;
use std::sync::Arc;

use dokploy_api::{EndpointMethod, endpoint_by_operation};
use serde_json::Value;
use url::Url;

use crate::{Dokploy, Error, ImperativeRequest};

/// One request to one operation of the pinned Dokploy contract.
///
/// The operation's HTTP method comes from the contract, never from the caller, so a spec
/// cannot ask for a mutation to be sent as a read. `Debug` shows the names of the query
/// parameters and whether there is a body, never the values.
#[derive(Clone, PartialEq)]
pub struct OperationRequest {
    operation: String,
    query: BTreeMap<String, Value>,
    body: Option<Value>,
}

impl OperationRequest {
    /// Starts a request for an operation id such as `registry.all`.
    #[must_use]
    pub fn new(operation: impl Into<String>) -> Self {
        Self {
            operation: operation.into(),
            query: BTreeMap::new(),
            body: None,
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
        self.body = Some(body);
        self
    }

    /// The operation id.
    #[must_use]
    pub fn operation(&self) -> &str {
        &self.operation
    }

    /// The query parameters, by wire name.
    #[must_use]
    pub fn query_parameters(&self) -> &BTreeMap<String, Value> {
        &self.query
    }

    /// The JSON body, when there is one.
    #[must_use]
    pub fn json_body(&self) -> Option<&Value> {
        self.body.as_ref()
    }
}

impl std::fmt::Debug for OperationRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OperationRequest")
            .field("operation", &self.operation)
            .field("query_names", &self.query.keys().collect::<Vec<_>>())
            .field("has_body", &self.body.is_some())
            .finish()
    }
}

/// Something that can send an [`OperationRequest`] to a Dokploy.
///
/// A mutation is never retried by an implementation, and one whose completion cannot be
/// proven reports [`Error::OutcomeUnknown`] (invariant 16). Errors are sanitised: they never
/// carry a credential or a request body.
pub trait Transport: Send + Sync {
    /// The base URL of the Dokploy instance, which identifies it for state.
    fn base_url(&self) -> &Url;

    /// Sends one request and returns its unmodified JSON response.
    fn call(&self, request: OperationRequest) -> impl Future<Output = Result<Value, Error>> + Send;
}

impl Transport for Dokploy {
    fn base_url(&self) -> &Url {
        Dokploy::base_url(self)
    }

    async fn call(&self, request: OperationRequest) -> Result<Value, Error> {
        let Some(endpoint) = endpoint_by_operation(&request.operation) else {
            return Err(Error::InvalidRequest {
                operation: "unknown",
                source: anyhow::anyhow!("operation is not declared by the pinned Dokploy contract"),
            });
        };
        let mut raw = match endpoint.method() {
            EndpointMethod::Get => ImperativeRequest::get(endpoint.operation()),
            EndpointMethod::Post => ImperativeRequest::post(endpoint.operation()),
            _ => {
                return Err(Error::InvalidRequest {
                    operation: endpoint.operation(),
                    source: anyhow::anyhow!("only GET and POST operations are supported"),
                });
            }
        };
        for (name, value) in request.query {
            raw = raw.query(name, value);
        }
        if let Some(body) = request.body {
            raw = raw.body(body);
        }

        self.execute_imperative(raw).await
    }
}

impl<T: Transport + ?Sized> Transport for &T {
    fn base_url(&self) -> &Url {
        (**self).base_url()
    }

    fn call(&self, request: OperationRequest) -> impl Future<Output = Result<Value, Error>> + Send {
        (**self).call(request)
    }
}

impl<T: Transport + ?Sized> Transport for Arc<T> {
    fn base_url(&self) -> &Url {
        (**self).base_url()
    }

    fn call(&self, request: OperationRequest) -> impl Future<Output = Result<Value, Error>> + Send {
        (**self).call(request)
    }
}
