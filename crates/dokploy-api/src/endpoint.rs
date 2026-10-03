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

/// One query parameter or JSON body property an operation accepts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestField {
    pub(crate) name: &'static str,
    pub(crate) required: bool,
}

impl RequestField {
    /// The name on the wire.
    #[must_use]
    pub const fn name(self) -> &'static str {
        self.name
    }

    /// Whether the contract requires it.
    #[must_use]
    pub const fn required(self) -> bool {
        self.required
    }
}

/// What the contract says about the JSON body of an operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BodyShape {
    /// The operation takes no body.
    None,
    /// The body is an object with exactly these properties.
    Object(&'static [RequestField]),
    /// The body is not a plain object; the contract names no fields.
    Unconstrained,
}

/// The query parameters and body properties one operation accepts, from the pinned contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RequestContract {
    pub(crate) query: &'static [RequestField],
    pub(crate) body: BodyShape,
}

impl RequestContract {
    /// The query parameters.
    #[must_use]
    pub const fn query(&self) -> &'static [RequestField] {
        self.query
    }

    /// The body shape.
    #[must_use]
    pub const fn body(&self) -> BodyShape {
        self.body
    }

    /// The body property with this wire name, when the body is an object.
    #[must_use]
    pub fn body_field(&self, name: &str) -> Option<RequestField> {
        match self.body {
            BodyShape::Object(fields) => fields.iter().copied().find(|field| field.name == name),
            BodyShape::None | BodyShape::Unconstrained => None,
        }
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
