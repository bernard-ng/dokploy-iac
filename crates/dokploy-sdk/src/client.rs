use std::sync::Arc;
use std::time::Duration;

use dokploy_api::{
    APPLICATION_ONE, APPLICATION_SEARCH, ApplicationOneRequest, ApplicationOneRequestQuery,
    ApplicationSearchRequest, ApplicationSearchRequestQuery, DokployApiClient,
    ENVIRONMENT_BY_PROJECT_ID, ENVIRONMENT_ONE, Endpoint, EndpointMethod,
    EnvironmentByProjectIdRequest, EnvironmentByProjectIdRequestQuery, EnvironmentOneRequest,
    EnvironmentOneRequestQuery, POSTGRES_ONE, PROJECT_ALL, PROJECT_ONE, PostgresOneRequest,
    PostgresOneRequestQuery, ProjectAllRequest, ProjectOneRequest, ProjectOneRequestQuery,
    endpoint_by_operation, validate_request,
};
use reqwest::header::{HeaderMap, HeaderValue};
use reqwest::{RequestBuilder, Response, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use url::Url;
use zeroize::Zeroizing;

use crate::error::{BuildError, DokployError, Error};
use crate::imperative::{
    Imperative, ImperativeBody, ImperativeMethod, ImperativeRequest, MultipartField,
};
use crate::models::{
    ApplicationCollection, ApplicationDetails, ApplicationSearchPage, EnvironmentCollection,
    EnvironmentDetails, PostgresDetails, ProjectDetails, ProjectTopology,
};
use crate::services::{Applications, Environments, Postgres, Projects};

const API_KEY_HEADER: &str = "x-api-key";
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const USER_AGENT: &str = concat!("dokploy-iac/", env!("CARGO_PKG_VERSION"));
const APPLICATION_SEARCH_PAGE_SIZE: usize = 100;
const APPLICATION_SEARCH_ITEM_LIMIT: usize = 10_000;

/// A configured client for the Dokploy API.
#[derive(Clone)]
pub struct Dokploy {
    pub(crate) inner: Arc<ClientInner>,
}

pub(crate) struct ClientInner {
    api: DokployApiClient,
}

impl Dokploy {
    /// Starts configuration of a Dokploy client.
    #[must_use]
    pub fn builder() -> DokployBuilder {
        DokployBuilder::default()
    }

    /// Returns the normalized API base URL used for every request.
    ///
    /// The URL contains no credentials, query, or fragment. Callers can parse
    /// it into their own instance-identity type without coupling this SDK to
    /// that domain.
    #[must_use]
    pub fn base_url(&self) -> &Url {
        &self.inner.api.base_url
    }

    /// Returns access to project read operations.
    #[must_use]
    pub fn projects(&self) -> Projects<'_> {
        Projects::new(self)
    }

    /// Returns access to application read operations.
    #[must_use]
    pub fn applications(&self) -> Applications<'_> {
        Applications::new(self)
    }

    /// Returns access to environment read operations.
    #[must_use]
    pub fn environments(&self) -> Environments<'_> {
        Environments::new(self)
    }

    /// Returns access to Postgres read operations.
    #[must_use]
    pub fn postgres(&self) -> Postgres<'_> {
        Postgres::new(self)
    }

    /// Returns broad raw access to operations from the pinned OpenAPI contract.
    #[must_use]
    pub fn imperative(&self) -> Imperative<'_> {
        Imperative::new(self)
    }

    pub(crate) async fn project_all(&self) -> Result<ProjectTopology, Error> {
        let request = ProjectAllRequest {};
        validate_generated_request(PROJECT_ALL, &request)?;

        self.read_json(PROJECT_ALL).await
    }

    pub(crate) async fn project_get(&self, project_id: &str) -> Result<ProjectDetails, Error> {
        let request = ProjectOneRequest {
            query: ProjectOneRequestQuery {
                project_id: project_id.to_owned(),
            },
        };
        validate_generated_request(PROJECT_ONE, &request)?;

        self.read_query_json(PROJECT_ONE, &request.query).await
    }

    pub(crate) async fn application_get(
        &self,
        application_id: &str,
    ) -> Result<ApplicationDetails, Error> {
        let request = ApplicationOneRequest {
            query: ApplicationOneRequestQuery {
                application_id: application_id.to_owned(),
            },
        };
        validate_generated_request(APPLICATION_ONE, &request)?;

        self.read_query_json(APPLICATION_ONE, &request.query).await
    }

    pub(crate) async fn applications_by_environment(
        &self,
        environment_id: &str,
    ) -> Result<ApplicationCollection, Error> {
        if environment_id.is_empty() {
            return Err(invalid_request(
                APPLICATION_SEARCH.operation(),
                "environment ID cannot be empty",
            ));
        }
        let mut applications = Vec::new();
        let mut expected_total = None;

        loop {
            let request = ApplicationSearchRequest {
                query: ApplicationSearchRequestQuery {
                    environment_id: Some(environment_id.to_owned()),
                    limit: Some(APPLICATION_SEARCH_PAGE_SIZE as f64),
                    offset: Some(applications.len() as f64),
                    ..ApplicationSearchRequestQuery::default()
                },
            };
            validate_generated_request(APPLICATION_SEARCH, &request)?;
            let page: ApplicationSearchPage = self
                .read_query_json(APPLICATION_SEARCH, &request.query)
                .await?;

            let expected = *expected_total.get_or_insert(page.total);
            if page.total != expected
                || expected > APPLICATION_SEARCH_ITEM_LIMIT as u64
                || page.items.len() > APPLICATION_SEARCH_PAGE_SIZE
            {
                return Err(Error::UnexpectedResponse {
                    operation: APPLICATION_SEARCH.operation(),
                });
            }
            let expected = usize::try_from(expected).map_err(|_| Error::UnexpectedResponse {
                operation: APPLICATION_SEARCH.operation(),
            })?;
            let page_would_exceed_total = applications
                .len()
                .checked_add(page.items.len())
                .is_none_or(|count| count > expected);
            if page_would_exceed_total || (page.items.is_empty() && applications.len() < expected) {
                return Err(Error::UnexpectedResponse {
                    operation: APPLICATION_SEARCH.operation(),
                });
            }

            applications.extend(page.items);
            if applications.len() == expected {
                return Ok(ApplicationCollection { applications });
            }
        }
    }

    pub(crate) async fn environment_get(
        &self,
        environment_id: &str,
    ) -> Result<EnvironmentDetails, Error> {
        let request = EnvironmentOneRequest {
            query: EnvironmentOneRequestQuery {
                environment_id: environment_id.to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_ONE, &request)?;

        self.read_query_json(ENVIRONMENT_ONE, &request.query).await
    }

    pub(crate) async fn environments_by_project(
        &self,
        project_id: &str,
    ) -> Result<EnvironmentCollection, Error> {
        let request = EnvironmentByProjectIdRequest {
            query: EnvironmentByProjectIdRequestQuery {
                project_id: project_id.to_owned(),
            },
        };
        validate_generated_request(ENVIRONMENT_BY_PROJECT_ID, &request)?;

        self.read_query_json(ENVIRONMENT_BY_PROJECT_ID, &request.query)
            .await
    }

    pub(crate) async fn postgres_get(&self, postgres_id: &str) -> Result<PostgresDetails, Error> {
        let request = PostgresOneRequest {
            query: PostgresOneRequestQuery {
                postgres_id: postgres_id.to_owned(),
            },
        };
        validate_generated_request(POSTGRES_ONE, &request)?;

        self.read_query_json(POSTGRES_ONE, &request.query).await
    }

    async fn read_json<T>(&self, endpoint: Endpoint) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let response = self.send(endpoint, self.request(endpoint)).await?;

        decode_json_response(endpoint, response).await
    }

    async fn read_query_json<T, Q>(&self, endpoint: Endpoint, query: &Q) -> Result<T, Error>
    where
        T: DeserializeOwned,
        Q: Serialize + ?Sized,
    {
        let builder = self.request(endpoint).query(query);
        let response = self.send(endpoint, builder).await?;

        decode_json_response(endpoint, response).await
    }

    pub(crate) async fn execute_imperative(
        &self,
        request: ImperativeRequest,
    ) -> Result<serde_json::Value, Error> {
        let endpoint = endpoint_by_operation(request.operation).ok_or_else(|| {
            invalid_request(
                request.operation,
                "operation is not declared by the pinned Dokploy contract",
            )
        })?;
        validate_imperative_method(&request, endpoint)?;
        let mut builder = self.request(endpoint);

        if !request.query.is_empty() {
            builder = builder.query(&request.query);
        }
        match request.body {
            ImperativeBody::None => {}
            ImperativeBody::Json(body) => {
                builder = builder.json(&body);
            }
            ImperativeBody::Multipart(fields) => {
                let mut form = reqwest::multipart::Form::new();
                for field in fields {
                    form = match field {
                        MultipartField::Text { name, value } => form.text(name, value),
                        MultipartField::File { name, path } => form
                            .file(name, path)
                            .await
                            .map_err(|source| Error::Request {
                                operation: request.operation,
                                source: source.into(),
                            })?,
                    };
                }
                builder = builder.multipart(form);
            }
        }

        let response = self.send(endpoint, builder).await?;

        decode_raw_response(endpoint, response).await
    }

    fn request(&self, endpoint: Endpoint) -> RequestBuilder {
        let url = operation_url(&self.inner.api.base_url, endpoint);

        self.inner
            .api
            .client
            .request(reqwest_method(endpoint.method()), url)
    }

    async fn send(&self, endpoint: Endpoint, builder: RequestBuilder) -> Result<Response, Error> {
        builder
            .send()
            .await
            .map_err(|source| transport_error(endpoint, source))
    }
}

fn validate_generated_request(
    endpoint: Endpoint,
    request: &impl dokploy_api::GeneratedRequest,
) -> Result<(), Error> {
    validate_request(request).map_err(|source| Error::InvalidRequest {
        operation: endpoint.operation(),
        source,
    })
}

fn validate_imperative_method(
    request: &ImperativeRequest,
    endpoint: Endpoint,
) -> Result<(), Error> {
    let matches_contract = matches!(
        (request.method, endpoint.method()),
        (ImperativeMethod::Get, EndpointMethod::Get)
            | (ImperativeMethod::Post, EndpointMethod::Post)
    );
    if matches_contract {
        return Ok(());
    }

    Err(invalid_request(
        request.operation,
        "HTTP method does not match the pinned Dokploy contract",
    ))
}

fn invalid_request(operation: &'static str, message: &'static str) -> Error {
    Error::InvalidRequest {
        operation,
        source: anyhow::anyhow!(message),
    }
}

fn operation_url(base_url: &Url, endpoint: Endpoint) -> Url {
    let mut url = base_url.clone();
    url.path_segments_mut()
        .expect("a validated base URL always supports path segments")
        .push(endpoint.operation());

    url
}

fn reqwest_method(method: EndpointMethod) -> reqwest::Method {
    match method {
        EndpointMethod::Delete => reqwest::Method::DELETE,
        EndpointMethod::Get => reqwest::Method::GET,
        EndpointMethod::Head => reqwest::Method::HEAD,
        EndpointMethod::Options => reqwest::Method::OPTIONS,
        EndpointMethod::Patch => reqwest::Method::PATCH,
        EndpointMethod::Post => reqwest::Method::POST,
        EndpointMethod::Put => reqwest::Method::PUT,
        EndpointMethod::Trace => reqwest::Method::TRACE,
    }
}

fn transport_error(endpoint: Endpoint, source: reqwest::Error) -> Error {
    match endpoint.method() {
        EndpointMethod::Delete
        | EndpointMethod::Patch
        | EndpointMethod::Post
        | EndpointMethod::Put => Error::OutcomeUnknown {
            operation: endpoint.operation(),
            source: source.into(),
        },
        EndpointMethod::Get
        | EndpointMethod::Head
        | EndpointMethod::Options
        | EndpointMethod::Trace => Error::Request {
            operation: endpoint.operation(),
            source: source.into(),
        },
    }
}

async fn decode_json_response<T>(endpoint: Endpoint, response: Response) -> Result<T, Error>
where
    T: DeserializeOwned,
{
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|source| transport_error(endpoint, source))?;

    if status.is_success() {
        return serde_json::from_slice(&bytes).map_err(|source| Error::Decode {
            operation: endpoint.operation(),
            source,
        });
    }

    Err(Error::Api(decode_dokploy_error(status, &bytes)))
}

async fn decode_raw_response(
    endpoint: Endpoint,
    response: Response,
) -> Result<serde_json::Value, Error> {
    let status = response.status();
    let bytes = response
        .bytes()
        .await
        .map_err(|source| transport_error(endpoint, source))?;

    if !status.is_success() {
        return Err(Error::Api(decode_dokploy_error(status, &bytes)));
    }
    if bytes.is_empty() {
        return Ok(serde_json::Value::Null);
    }

    serde_json::from_slice(&bytes).map_err(|source| Error::Decode {
        operation: endpoint.operation(),
        source,
    })
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    issues: Vec<ErrorIssue>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ErrorIssue {
    Detailed { message: String },
    Message(String),
}

fn decode_dokploy_error(status: StatusCode, bytes: &[u8]) -> DokployError {
    let body = serde_json::from_slice::<ErrorEnvelope>(bytes).ok();
    let code = body
        .as_ref()
        .and_then(|body| body.code.clone())
        .filter(|code| !code.trim().is_empty())
        .unwrap_or_else(|| fallback_error_code(status).to_owned());
    let message = body
        .as_ref()
        .and_then(|body| body.message.clone())
        .filter(|message| !message.trim().is_empty())
        .unwrap_or_else(|| {
            status
                .canonical_reason()
                .unwrap_or("Dokploy request failed")
                .to_owned()
        });
    let issues = body
        .map(|body| {
            body.issues
                .into_iter()
                .map(|issue| match issue {
                    ErrorIssue::Detailed { message } | ErrorIssue::Message(message) => message,
                })
                .collect()
        })
        .unwrap_or_default();

    DokployError::new(status.as_u16(), code, message, issues)
}

fn fallback_error_code(status: StatusCode) -> &'static str {
    match status {
        StatusCode::BAD_REQUEST => "BAD_REQUEST",
        StatusCode::UNAUTHORIZED => "UNAUTHORIZED",
        StatusCode::FORBIDDEN => "FORBIDDEN",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "CONFLICT",
        StatusCode::UNPROCESSABLE_ENTITY => "UNPROCESSABLE_ENTITY",
        StatusCode::TOO_MANY_REQUESTS => "TOO_MANY_REQUESTS",
        status if status.is_server_error() => "SERVER_ERROR",
        _ => "DOKPLOY_ERROR",
    }
}

/// Builder for a [`Dokploy`] client.
#[derive(Default)]
pub struct DokployBuilder {
    url: Option<String>,
    api_key: Option<Zeroizing<String>>,
}

impl DokployBuilder {
    /// Sets the Dokploy instance URL.
    #[must_use]
    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets the Dokploy API key.
    #[must_use]
    pub fn api_key(mut self, api_key: impl Into<String>) -> Self {
        self.api_key = Some(Zeroizing::new(api_key.into()));
        self
    }

    /// Validates the configuration and constructs the client.
    pub fn build(self) -> Result<Dokploy, BuildError> {
        let base_url = normalize_base_url(self.url.as_deref().ok_or(BuildError::MissingUrl)?)?;
        let api_key = self.api_key.ok_or(BuildError::MissingApiKey)?;

        if api_key.trim().is_empty() {
            return Err(BuildError::EmptyApiKey);
        }

        let mut header_value =
            HeaderValue::from_str(&api_key).map_err(BuildError::InvalidApiKey)?;
        header_value.set_sensitive(true);

        let mut headers = HeaderMap::new();
        headers.insert(API_KEY_HEADER, header_value);

        let retry_host = base_url
            .host_str()
            .expect("a validated base URL always has a host")
            .to_owned();
        let retry_policy = reqwest::retry::for_host(retry_host)
            .max_retries_per_request(2)
            .classify_fn(|request| {
                let is_safe_read = request.method() == reqwest::Method::GET;
                let is_transient_status = request.status().is_some_and(|status| {
                    matches!(status.as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504)
                });

                if is_safe_read && (request.error().is_some() || is_transient_status) {
                    request.retryable()
                } else {
                    request.success()
                }
            });

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .user_agent(USER_AGENT)
            .timeout(DEFAULT_TIMEOUT)
            .retry(retry_policy)
            .build()
            .map_err(BuildError::HttpClient)?;
        let api = DokployApiClient::with_client(base_url.as_str(), client)
            .map_err(|_| BuildError::InvalidUrl)?;

        Ok(Dokploy {
            inner: Arc::new(ClientInner { api }),
        })
    }
}

fn normalize_base_url(value: &str) -> Result<Url, BuildError> {
    let mut url = Url::parse(value).map_err(|_| BuildError::InvalidUrl)?;

    if !matches!(url.scheme(), "http" | "https")
        || !url.has_host()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(BuildError::InvalidUrl);
    }

    let trimmed_path = url.path().trim_end_matches('/');
    let api_path = if trimmed_path.ends_with("/api") {
        trimmed_path.to_owned()
    } else if trimmed_path.is_empty() {
        "/api".to_owned()
    } else {
        format!("{trimmed_path}/api")
    };
    url.set_path(&api_path);

    Ok(url)
}
