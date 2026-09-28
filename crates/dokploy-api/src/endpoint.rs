use anyhow::Context;
use validator::Validate;

/// An HTTP method declared by the pinned Dokploy contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointMethod {
    Delete,
    Get,
    Head,
    Options,
    Patch,
    Post,
    Put,
    Trace,
}

/// Stable transport metadata for one generated Dokploy operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    operation_id: &'static str,
    operation: &'static str,
    method: EndpointMethod,
}

impl Endpoint {
    /// Creates metadata generated directly from an OpenAPI operation.
    #[must_use]
    pub const fn new(
        operation_id: &'static str,
        operation: &'static str,
        method: EndpointMethod,
    ) -> Self {
        Self {
            operation_id,
            operation,
            method,
        }
    }

    /// Returns the OpenAPI operation identifier.
    #[must_use]
    pub const fn operation_id(self) -> &'static str {
        self.operation_id
    }

    /// Returns the single path segment used on the Dokploy wire protocol.
    #[must_use]
    pub const fn operation(self) -> &'static str {
        self.operation
    }

    /// Returns the HTTP method declared by the pinned contract.
    #[must_use]
    pub const fn method(self) -> EndpointMethod {
        self.method
    }
}

/// A request type carrying constraints emitted by the OpenAPI generator.
pub trait GeneratedRequest {
    /// Runs the generated request constraints.
    fn validate_generated(&self) -> anyhow::Result<()>;
}

impl<T> GeneratedRequest for T
where
    T: Validate,
{
    fn validate_generated(&self) -> anyhow::Result<()> {
        self.validate().context("generated request validation")
    }
}

/// Runs validation emitted for a generated request type.
pub fn validate_request(request: &impl GeneratedRequest) -> anyhow::Result<()> {
    request.validate_generated()
}
