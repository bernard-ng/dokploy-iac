use thiserror::Error;

/// A structured error returned by Dokploy.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DokployError {
    status: u16,
    code: String,
    message: String,
    issues: Vec<String>,
}

impl DokployError {
    /// Builds an error as Dokploy would report it; transports other than HTTP, such as the
    /// simulator, use this to answer a request with a rejection.
    #[must_use]
    pub fn new(status: u16, code: String, message: String, issues: Vec<String>) -> Self {
        Self {
            status,
            code,
            message,
            issues,
        }
    }

    /// Returns the HTTP status code.
    #[must_use]
    pub fn status(&self) -> u16 {
        self.status
    }

    /// Returns the stable Dokploy error code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable Dokploy error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns validation issue messages, when supplied by Dokploy.
    #[must_use]
    pub fn issues(&self) -> &[String] {
        &self.issues
    }
}

impl std::fmt::Display for DokployError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "Dokploy returned HTTP {} ({}): {}",
            self.status, self.code, self.message
        )
    }
}

/// A client-construction failure.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("a Dokploy instance URL is required")]
    MissingUrl,
    #[error("a Dokploy API key is required")]
    MissingApiKey,
    #[error("the Dokploy API key cannot be empty")]
    EmptyApiKey,
    #[error(
        "the Dokploy instance URL must be an HTTP(S) URL without credentials, a query, or a fragment"
    )]
    InvalidUrl,
    #[error("the Dokploy API key cannot be used as an HTTP header")]
    InvalidApiKey(#[source] reqwest::header::InvalidHeaderValue),
    #[error("the HTTP client could not be constructed")]
    HttpClient(#[source] reqwest::Error),
}

/// A failure while communicating with Dokploy.
#[derive(Debug, Error)]
pub enum Error {
    #[error("the Dokploy request for `{operation}` is invalid")]
    InvalidRequest {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("the Dokploy request for `{operation}` failed")]
    Request {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error(
        "the outcome of Dokploy mutation `{operation}` is unknown because completion could not be proven"
    )]
    OutcomeUnknown {
        operation: &'static str,
        #[source]
        source: anyhow::Error,
    },
    #[error("{0}")]
    Api(DokployError),
    #[error("Dokploy returned an unrecognized response for `{operation}`")]
    UnexpectedResponse { operation: &'static str },
    #[error("Dokploy returned an invalid response for `{operation}`")]
    Decode {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
}

impl Error {
    /// Returns structured Dokploy error details when the server supplied them.
    #[must_use]
    pub fn dokploy(&self) -> Option<&DokployError> {
        match self {
            Self::Api(error) => Some(error),
            Self::InvalidRequest { .. }
            | Self::Request { .. }
            | Self::OutcomeUnknown { .. }
            | Self::UnexpectedResponse { .. }
            | Self::Decode { .. } => None,
        }
    }
}
