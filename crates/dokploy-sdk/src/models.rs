use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer, ser::SerializeStruct};
use std::fmt;
use std::num::{NonZeroU16, NonZeroU32};
use zeroize::{Zeroize, Zeroizing};

/// A mutation value that distinguishes omission from an explicit JSON null.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Nullable<T> {
    /// Explicitly clear the remote field.
    Null,
    /// Set the remote field to a concrete value.
    Value(T),
}

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates an identifier from the opaque value returned by Dokploy.
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

identifier!(ApplicationId);
identifier!(ComposeId);
identifier!(EnvironmentId);
identifier!(LibSqlId);
identifier!(MariaDbId);
identifier!(MongoId);
identifier!(MountId);
identifier!(MySqlId);
identifier!(PortId);
identifier!(PostgresId);
identifier!(ProjectId);
identifier!(RedirectId);
identifier!(RedisId);
identifier!(DomainId);
identifier!(SecurityId);
identifier!(ScheduleId);
identifier!(BackupId);
identifier!(ServerId);
identifier!(RegistryId);
identifier!(DestinationId);

trait IdentifierValue {
    fn identifier_value(&self) -> &str;
}

impl IdentifierValue for ServerId {
    fn identifier_value(&self) -> &str {
        self.as_str()
    }
}

impl IdentifierValue for RegistryId {
    fn identifier_value(&self) -> &str {
        self.as_str()
    }
}

const LIBSQL_DEFAULT_IMAGE: &str = "ghcr.io/tursodatabase/libsql-server:v0.24.32";

/// Explicit physical placement selected when creating a service.
///
/// Leaving placement unset on a create input omits `serverId` and leaves the
/// field unmanaged. Selecting [`Self::Local`] sends an explicit JSON null,
/// while [`Self::Server`] sends one resolved external server identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerPlacement {
    /// Place the service on Dokploy's local server.
    Local,
    /// Place the service on one specific external Dokploy server.
    Server(ServerId),
}

impl ServerPlacement {
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Local => true,
            Self::Server(server_id) => !server_id.as_str().is_empty(),
        }
    }

    pub(crate) fn matches_response(&self, field: &ResponseField<ServerId>) -> bool {
        match (self, field) {
            (Self::Local, ResponseField::Null) => true,
            (Self::Server(expected), ResponseField::Value(actual)) => expected == actual,
            _ => false,
        }
    }
}

/// Minimal non-sensitive server identity returned by `server.all`.
///
/// Command configuration, metrics tokens, connection data, and nested runtime
/// records are deliberately not represented. Unknown response fields are
/// ignored so the selector boundary tolerates unrelated upstream additions.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ServerSummary {
    pub server_id: ServerId,
    pub name: String,
    pub server_type: String,
}

/// The complete bounded server selector collection.
#[derive(Clone, Debug, PartialEq)]
pub struct ServerCollection {
    pub(crate) servers: Vec<ServerSummary>,
}

impl ServerCollection {
    /// Returns every server selector candidate in upstream order.
    #[must_use]
    pub fn servers(&self) -> &[ServerSummary] {
        &self.servers
    }
}

/// Minimal non-sensitive registry identity returned by `registry.all`.
///
/// Credentials and other registry connection settings are deliberately not
/// represented.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RegistrySummary {
    pub registry_id: RegistryId,
    pub registry_name: String,
}

/// The complete bounded registry selector collection.
#[derive(Clone, Debug, PartialEq)]
pub struct RegistryCollection {
    pub(crate) registries: Vec<RegistrySummary>,
}

impl RegistryCollection {
    /// Returns every registry selector candidate in upstream order.
    #[must_use]
    pub fn registries(&self) -> &[RegistrySummary] {
        &self.registries
    }
}

/// Minimal non-sensitive backup-destination identity returned by
/// `destination.all`.
///
/// Provider configuration, access keys, secret keys, and bucket details are
/// deliberately not represented.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DestinationSummary {
    pub destination_id: DestinationId,
    pub name: String,
}

/// The complete bounded backup-destination selector collection.
#[derive(Clone, Debug, PartialEq)]
pub struct DestinationCollection {
    pub(crate) destinations: Vec<DestinationSummary>,
}

impl DestinationCollection {
    /// Returns every backup-destination selector candidate in upstream order.
    #[must_use]
    pub fn destinations(&self) -> &[DestinationSummary] {
        &self.destinations
    }
}

/// Runtime mode used by one Dokploy Compose record.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ComposeType {
    /// Manage the document through Docker Compose.
    #[default]
    DockerCompose,
    /// Manage the document as a Docker Swarm stack.
    Stack,
}

impl ComposeType {
    fn as_str(self) -> &'static str {
        match self {
            Self::DockerCompose => "docker-compose",
            Self::Stack => "stack",
        }
    }
}

/// Whether deleting a Compose record should also delete its Docker volumes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposeVolumePolicy {
    /// Keep volumes after deleting the Compose record.
    Preserve,
    /// Delete volumes with the Compose record.
    Delete,
}

impl ComposeVolumePolicy {
    pub(crate) const fn delete_volumes(self) -> bool {
        matches!(self, Self::Delete)
    }
}

/// Inputs required to create one raw Dokploy Compose record.
///
/// The Compose document can contain credentials and is therefore retained in
/// zeroizing memory and omitted from debug output.
pub struct CreateCompose {
    name: String,
    environment_id: EnvironmentId,
    compose_file: Zeroizing<String>,
    description: Option<String>,
    compose_type: ComposeType,
    app_name: Option<String>,
    server_placement: Option<ServerPlacement>,
}

impl CreateCompose {
    /// Creates raw Compose input with Docker Compose as its runtime mode.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        compose_file: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            compose_file,
            description: None,
            compose_type: ComposeType::DockerCompose,
            app_name: None,
            server_placement: None,
        }
    }

    /// Sets an initial description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Sets the stable application name used by Dokploy.
    #[must_use]
    pub fn with_app_name(mut self, app_name: impl Into<String>) -> Self {
        self.app_name = Some(app_name.into());
        self
    }

    /// Selects the Compose runtime mode.
    #[must_use]
    pub const fn with_compose_type(mut self, compose_type: ComposeType) -> Self {
        self.compose_type = compose_type;
        self
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    /// Associates the Compose record with a specific external Dokploy server.
    #[must_use]
    pub fn with_server(self, server_id: ServerId) -> Self {
        self.with_server_placement(ServerPlacement::Server(server_id))
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) const fn environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.compose_file.is_empty()
            && self
                .app_name
                .as_ref()
                .is_none_or(|value| valid_application_name(value))
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateCompose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateCompose")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("compose_file", &"[REDACTED]")
            .field("description", &self.description)
            .field("compose_type", &self.compose_type)
            .field("app_name", &self.app_name)
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateCompose {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateCompose",
            5 + usize::from(self.description.is_some())
                + usize::from(self.app_name.is_some())
                + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        if let Some(description) = &self.description {
            body.serialize_field("description", description)?;
        }
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("composeType", self.compose_type.as_str())?;
        if let Some(app_name) = &self.app_name {
            body.serialize_field("appName", app_name)?;
        }
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.serialize_field("composeFile", self.compose_file.as_str())?;
        body.serialize_field("sourceType", "raw")?;
        body.end()
    }
}

/// Physical identity returned by Dokploy when a Compose record is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedCompose {
    compose_id: ComposeId,
}

impl CreatedCompose {
    pub(crate) fn new(compose_id: ComposeId) -> Self {
        Self { compose_id }
    }

    /// Returns the new Compose identity.
    #[must_use]
    pub const fn compose_id(&self) -> &ComposeId {
        &self.compose_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComposeCreateResponse {
    pub(crate) compose_id: ComposeId,
    pub(crate) environment_id: EnvironmentId,
    pub(crate) name: String,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// Owned Compose fields written by one update.
pub struct UpdateCompose {
    compose_id: ComposeId,
    name: Option<String>,
    description: Option<Nullable<String>>,
    compose_file: Option<Zeroizing<String>>,
}

impl UpdateCompose {
    /// Starts a Compose update with no fields selected.
    #[must_use]
    pub fn new(compose_id: ComposeId) -> Self {
        Self {
            compose_id,
            name: None,
            description: None,
            compose_file: None,
        }
    }

    /// Selects the human-readable name.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Selects a concrete description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(Nullable::Value(description.into()));
        self
    }

    /// Explicitly clears the description.
    #[must_use]
    pub fn clear_description(mut self) -> Self {
        self.description = Some(Nullable::Null);
        self
    }

    /// Replaces the opaque Compose document.
    #[must_use]
    pub fn with_compose_file(mut self, compose_file: Zeroizing<String>) -> Self {
        self.compose_file = Some(compose_file);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.compose_id.as_str().is_empty()
            && (self.name.as_ref().is_some_and(|value| !value.is_empty())
                || self.description.is_some()
                || self
                    .compose_file
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateCompose {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateCompose")
            .field("compose_id", &self.compose_id)
            .field("name", &self.name)
            .field("description", &self.description)
            .field(
                "compose_file",
                &self.compose_file.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Serialize for UpdateCompose {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateCompose",
            1 + usize::from(self.name.is_some())
                + usize::from(self.description.is_some())
                + usize::from(self.compose_file.is_some()),
        )?;
        body.serialize_field("composeId", self.compose_id.as_str())?;
        if let Some(name) = &self.name {
            body.serialize_field("name", name)?;
        }
        if let Some(description) = &self.description {
            body.serialize_field("description", description)?;
        }
        if let Some(compose_file) = &self.compose_file {
            body.serialize_field("composeFile", compose_file.as_str())?;
        }
        body.end()
    }
}

/// The network scope used when Dokploy publishes an application port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PublishMode {
    /// Publish through Docker Swarm's routing mesh.
    Ingress,
    /// Publish directly on the node that runs the task.
    Host,
}

/// The transport protocol carried by a published application port.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PortProtocol {
    /// Transmission Control Protocol.
    Tcp,
    /// User Datagram Protocol.
    Udp,
}

/// Complete inputs required to publish one application port.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreatePort {
    application_id: ApplicationId,
    published_port: NonZeroU16,
    target_port: NonZeroU16,
    publish_mode: PublishMode,
    protocol: PortProtocol,
}

impl CreatePort {
    /// Creates one explicit port publication for an application.
    #[must_use]
    pub const fn new(
        application_id: ApplicationId,
        published_port: NonZeroU16,
        target_port: NonZeroU16,
        publish_mode: PublishMode,
        protocol: PortProtocol,
    ) -> Self {
        Self {
            application_id,
            published_port,
            target_port,
            publish_mode,
            protocol,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.application_id.as_str().is_empty()
    }

    pub(crate) fn matches(&self, details: &PortDetails) -> bool {
        !details.port_id.as_str().is_empty()
            && details.application_id == self.application_id
            && details.published_port == self.published_port
            && details.target_port == self.target_port
            && details.publish_mode == self.publish_mode
            && details.protocol == self.protocol
    }
}

/// Physical identity returned by Dokploy when a port is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedPort {
    port_id: PortId,
}

impl CreatedPort {
    pub(crate) fn new(port_id: PortId) -> Self {
        Self { port_id }
    }

    /// Returns the new Port identity.
    #[must_use]
    pub const fn port_id(&self) -> &PortId {
        &self.port_id
    }
}

/// Complete owned fields required by Dokploy's port update operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePort {
    port_id: PortId,
    published_port: NonZeroU16,
    target_port: NonZeroU16,
    publish_mode: PublishMode,
    protocol: PortProtocol,
}

impl UpdatePort {
    /// Replaces all mutable fields of one application port.
    #[must_use]
    pub const fn new(
        port_id: PortId,
        published_port: NonZeroU16,
        target_port: NonZeroU16,
        publish_mode: PublishMode,
        protocol: PortProtocol,
    ) -> Self {
        Self {
            port_id,
            published_port,
            target_port,
            publish_mode,
            protocol,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.port_id.as_str().is_empty()
    }
}

/// Complete inputs required to create one application Redirect.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateRedirect {
    application_id: ApplicationId,
    regex: String,
    replacement: String,
    permanent: bool,
}

impl CreateRedirect {
    /// Creates one Redirect attached to an application.
    #[must_use]
    pub fn new(
        application_id: ApplicationId,
        regex: impl Into<String>,
        replacement: impl Into<String>,
        permanent: bool,
    ) -> Self {
        Self {
            application_id,
            regex: regex.into(),
            replacement: replacement.into(),
            permanent,
        }
    }

    pub(crate) fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    pub(crate) fn regex(&self) -> &str {
        &self.regex
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.application_id.as_str().is_empty()
            && !self.regex.is_empty()
            && !self.replacement.is_empty()
    }

    pub(crate) fn matches(&self, details: &RedirectDetails) -> bool {
        !details.redirect_id.as_str().is_empty()
            && details.application_id == self.application_id
            && details.regex == self.regex
            && details.replacement == self.replacement
            && details.permanent == self.permanent
    }
}

/// Physical identity discovered after Dokploy creates a Redirect.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedRedirect {
    redirect_id: RedirectId,
}

impl CreatedRedirect {
    pub(crate) fn new(redirect_id: RedirectId) -> Self {
        Self { redirect_id }
    }

    /// Returns the new Redirect identity.
    #[must_use]
    pub const fn redirect_id(&self) -> &RedirectId {
        &self.redirect_id
    }
}

/// Complete mutable fields required by Dokploy's Redirect update operation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateRedirect {
    redirect_id: RedirectId,
    regex: String,
    replacement: String,
    permanent: bool,
}

impl UpdateRedirect {
    /// Replaces every mutable field of one Redirect without changing its parent.
    #[must_use]
    pub fn new(
        redirect_id: RedirectId,
        regex: impl Into<String>,
        replacement: impl Into<String>,
        permanent: bool,
    ) -> Self {
        Self {
            redirect_id,
            regex: regex.into(),
            replacement: replacement.into(),
            permanent,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.redirect_id.as_str().is_empty()
            && !self.regex.is_empty()
            && !self.replacement.is_empty()
    }

    pub(crate) const fn redirect_id(&self) -> &RedirectId {
        &self.redirect_id
    }

    pub(crate) fn regex(&self) -> &str {
        &self.regex
    }

    pub(crate) fn matches(&self, details: &RedirectDetails) -> bool {
        details.redirect_id == self.redirect_id
            && details.regex == self.regex
            && details.replacement == self.replacement
            && details.permanent == self.permanent
    }
}

/// Complete inputs required to create one application basic-auth entry.
pub struct CreateSecurity {
    application_id: ApplicationId,
    username: String,
    password: Zeroizing<String>,
}

impl CreateSecurity {
    /// Creates one basic-auth entry attached to an application.
    #[must_use]
    pub fn new(
        application_id: ApplicationId,
        username: impl Into<String>,
        password: Zeroizing<String>,
    ) -> Self {
        Self {
            application_id,
            username: username.into(),
            password,
        }
    }

    pub(crate) fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.application_id.as_str().is_empty()
            && !self.username.is_empty()
            && !self.password.is_empty()
    }

    pub(crate) fn matches(&self, details: &SecurityDetails) -> bool {
        !details.security_id.as_str().is_empty()
            && details.application_id == self.application_id
            && details.username == self.username
            && details.password_present
    }
}

impl fmt::Debug for CreateSecurity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateSecurity")
            .field("application_id", &self.application_id)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for CreateSecurity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreateSecurity", 3)?;
        body.serialize_field("applicationId", self.application_id.as_str())?;
        body.serialize_field("username", &self.username)?;
        body.serialize_field("password", self.password.as_str())?;
        body.end()
    }
}

/// Physical identity discovered after Dokploy creates a basic-auth entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedSecurity {
    security_id: SecurityId,
}

impl CreatedSecurity {
    pub(crate) fn new(security_id: SecurityId) -> Self {
        Self { security_id }
    }

    /// Returns the new Security identity.
    #[must_use]
    pub const fn security_id(&self) -> &SecurityId {
        &self.security_id
    }
}

/// Complete username and password required by Dokploy's Security update.
pub struct UpdateSecurity {
    security_id: SecurityId,
    username: String,
    password: Zeroizing<String>,
}

impl UpdateSecurity {
    /// Replaces the username and password without changing the application.
    #[must_use]
    pub fn new(
        security_id: SecurityId,
        username: impl Into<String>,
        password: Zeroizing<String>,
    ) -> Self {
        Self {
            security_id,
            username: username.into(),
            password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.security_id.as_str().is_empty()
            && !self.username.is_empty()
            && !self.password.is_empty()
    }

    pub(crate) const fn security_id(&self) -> &SecurityId {
        &self.security_id
    }

    pub(crate) fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn matches(&self, proof: &SecurityProofDetails) -> bool {
        let details = proof.details();

        details.security_id == self.security_id
            && details.username == self.username
            && proof
                .password
                .as_ref()
                .is_some_and(|password| password.as_str() == self.password.as_str())
    }
}

impl fmt::Debug for UpdateSecurity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateSecurity")
            .field("security_id", &self.security_id)
            .field("username", &self.username)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for UpdateSecurity {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("UpdateSecurity", 3)?;
        body.serialize_field("securityId", self.security_id.as_str())?;
        body.serialize_field("username", &self.username)?;
        body.serialize_field("password", self.password.as_str())?;
        body.end()
    }
}

/// The shell used to execute one Schedule command.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellType {
    /// Execute with Bash.
    Bash,
    /// Execute with POSIX `sh`.
    Sh,
}

/// The exact undeployed service target that owns a Schedule.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ScheduleTarget {
    /// Run against one application.
    Application(ApplicationId),
    /// Run against one named service in a Compose record.
    Compose {
        /// The owning Compose identity.
        compose_id: ComposeId,
        /// The service key inside the Compose document.
        service_name: String,
    },
}

impl ScheduleTarget {
    pub(crate) fn is_valid(&self) -> bool {
        match self {
            Self::Application(application_id) => !application_id.as_str().is_empty(),
            Self::Compose {
                compose_id,
                service_name,
            } => !compose_id.as_str().is_empty() && !service_name.is_empty(),
        }
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Application(application_id) => application_id.as_str(),
            Self::Compose { compose_id, .. } => compose_id.as_str(),
        }
    }
}

/// Complete inputs required to create one Schedule.
pub struct CreateSchedule {
    target: ScheduleTarget,
    name: String,
    description: Option<String>,
    cron_expression: String,
    shell_type: ShellType,
    command: Zeroizing<String>,
    script: Option<Zeroizing<String>>,
    enabled: bool,
    timezone: Option<String>,
}

impl CreateSchedule {
    /// Creates one disabled or enabled Schedule for an explicit supported target.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: ScheduleTarget,
        name: impl Into<String>,
        description: Option<String>,
        cron_expression: impl Into<String>,
        shell_type: ShellType,
        command: Zeroizing<String>,
        script: Option<Zeroizing<String>>,
        enabled: bool,
        timezone: Option<String>,
    ) -> Self {
        Self {
            target,
            name: name.into(),
            description,
            cron_expression: cron_expression.into(),
            shell_type,
            command,
            script,
            enabled,
            timezone,
        }
    }

    pub(crate) const fn target(&self) -> &ScheduleTarget {
        &self.target
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.target.is_valid()
            && !self.name.is_empty()
            && !self.cron_expression.is_empty()
            && !self.command.is_empty()
            && self.script.as_ref().is_none_or(|script| !script.is_empty())
            && self
                .timezone
                .as_ref()
                .is_none_or(|timezone| !timezone.is_empty())
    }

    pub(crate) fn matches(&self, proof: &ScheduleProofDetails) -> bool {
        let details = proof.details();
        details.is_valid()
            && details.target == self.target
            && details.name == self.name
            && details.description == self.description
            && details.cron_expression == self.cron_expression
            && details.shell_type == self.shell_type
            && details.command_present
            && details.script_present == self.script.is_some()
            && details.enabled == self.enabled
            && details.timezone == self.timezone
            && proof.command.as_str() == self.command.as_str()
            && proof.script.as_ref().map(|script| script.as_str())
                == self.script.as_ref().map(|script| script.as_str())
    }
}

impl fmt::Debug for CreateSchedule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateSchedule")
            .field("target", &self.target)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("cron_expression", &self.cron_expression)
            .field("shell_type", &self.shell_type)
            .field("command", &"[REDACTED]")
            .field("script", &self.script.as_ref().map(|_| "[REDACTED]"))
            .field("enabled", &self.enabled)
            .field("timezone", &self.timezone)
            .finish()
    }
}

impl Serialize for CreateSchedule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let field_count = match self.target {
            ScheduleTarget::Application(_) => 10,
            ScheduleTarget::Compose { .. } => 11,
        };
        let mut body = serializer.serialize_struct("CreateSchedule", field_count)?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("description", &self.description)?;
        body.serialize_field("cronExpression", &self.cron_expression)?;
        body.serialize_field("shellType", &self.shell_type)?;
        body.serialize_field("command", self.command.as_str())?;
        body.serialize_field(
            "script",
            &self.script.as_ref().map(|script| script.as_str()),
        )?;
        body.serialize_field("enabled", &self.enabled)?;
        body.serialize_field("timezone", &self.timezone)?;
        match &self.target {
            ScheduleTarget::Application(application_id) => {
                body.serialize_field("scheduleType", "application")?;
                body.serialize_field("applicationId", application_id.as_str())?;
            }
            ScheduleTarget::Compose {
                compose_id,
                service_name,
            } => {
                body.serialize_field("scheduleType", "compose")?;
                body.serialize_field("composeId", compose_id.as_str())?;
                body.serialize_field("serviceName", service_name)?;
            }
        }
        body.end()
    }
}

/// Physical identity returned after Dokploy creates a Schedule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedSchedule {
    schedule_id: ScheduleId,
}

impl CreatedSchedule {
    pub(crate) fn new(schedule_id: ScheduleId) -> Self {
        Self { schedule_id }
    }

    /// Returns the new Schedule identity.
    #[must_use]
    pub const fn schedule_id(&self) -> &ScheduleId {
        &self.schedule_id
    }
}

/// Every safely mutable field required by Dokploy's Schedule update.
pub struct UpdateSchedule {
    schedule_id: ScheduleId,
    target: ScheduleTarget,
    name: String,
    description: Option<String>,
    cron_expression: String,
    shell_type: ShellType,
    command: Zeroizing<String>,
    script: Option<Zeroizing<String>>,
    enabled: bool,
    timezone: Option<String>,
}

impl UpdateSchedule {
    /// Replaces every mutable field without changing the target or service.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        schedule_id: ScheduleId,
        target: ScheduleTarget,
        name: impl Into<String>,
        description: Option<String>,
        cron_expression: impl Into<String>,
        shell_type: ShellType,
        command: Zeroizing<String>,
        script: Option<Zeroizing<String>>,
        enabled: bool,
        timezone: Option<String>,
    ) -> Self {
        Self {
            schedule_id,
            target,
            name: name.into(),
            description,
            cron_expression: cron_expression.into(),
            shell_type,
            command,
            script,
            enabled,
            timezone,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.schedule_id.as_str().is_empty()
            && self.target.is_valid()
            && !self.name.is_empty()
            && !self.cron_expression.is_empty()
            && !self.command.is_empty()
            && self.script.as_ref().is_none_or(|script| !script.is_empty())
            && self
                .timezone
                .as_ref()
                .is_none_or(|timezone| !timezone.is_empty())
    }

    pub(crate) const fn schedule_id(&self) -> &ScheduleId {
        &self.schedule_id
    }

    pub(crate) const fn target(&self) -> &ScheduleTarget {
        &self.target
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn matches(&self, proof: &ScheduleProofDetails) -> bool {
        let details = proof.details();
        details.is_valid()
            && details.schedule_id == self.schedule_id
            && details.target == self.target
            && details.name == self.name
            && details.description == self.description
            && details.cron_expression == self.cron_expression
            && details.shell_type == self.shell_type
            && details.command_present
            && details.script_present == self.script.is_some()
            && details.enabled == self.enabled
            && details.timezone == self.timezone
            && proof.command.as_str() == self.command.as_str()
            && proof.script.as_ref().map(|script| script.as_str())
                == self.script.as_ref().map(|script| script.as_str())
    }
}

impl fmt::Debug for UpdateSchedule {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateSchedule")
            .field("schedule_id", &self.schedule_id)
            .field("target", &self.target)
            .field("name", &self.name)
            .field("description", &self.description)
            .field("cron_expression", &self.cron_expression)
            .field("shell_type", &self.shell_type)
            .field("command", &"[REDACTED]")
            .field("script", &self.script.as_ref().map(|_| "[REDACTED]"))
            .field("enabled", &self.enabled)
            .field("timezone", &self.timezone)
            .finish()
    }
}

impl Serialize for UpdateSchedule {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("UpdateSchedule", 9)?;
        body.serialize_field("scheduleId", self.schedule_id.as_str())?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("description", &self.description)?;
        body.serialize_field("cronExpression", &self.cron_expression)?;
        body.serialize_field("shellType", &self.shell_type)?;
        body.serialize_field("command", self.command.as_str())?;
        body.serialize_field(
            "script",
            &self.script.as_ref().map(|script| script.as_str()),
        )?;
        body.serialize_field("enabled", &self.enabled)?;
        body.serialize_field("timezone", &self.timezone)?;
        body.end()
    }
}

/// The supported database target that owns a Backup.
///
/// Compose and web-server backups remain unavailable until their metadata and
/// privilege boundaries have a separate secret-safe design.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum BackupTarget {
    /// A PostgreSQL database.
    Postgres(PostgresId),
    /// A MySQL database.
    MySql(MySqlId),
    /// A MariaDB database.
    MariaDb(MariaDbId),
    /// A MongoDB database.
    Mongo(MongoId),
    /// A LibSQL database.
    LibSql(LibSqlId),
}

impl BackupTarget {
    pub(crate) fn is_valid(&self) -> bool {
        !self.id().is_empty()
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Postgres(id) => id.as_str(),
            Self::MySql(id) => id.as_str(),
            Self::MariaDb(id) => id.as_str(),
            Self::Mongo(id) => id.as_str(),
            Self::LibSql(id) => id.as_str(),
        }
    }

    pub(crate) const fn database_type(&self) -> &'static str {
        match self {
            Self::Postgres(_) => "postgres",
            Self::MySql(_) => "mysql",
            Self::MariaDb(_) => "mariadb",
            Self::Mongo(_) => "mongo",
            Self::LibSql(_) => "libsql",
        }
    }
}

/// Complete safe inputs required to create one database Backup.
#[derive(Clone, Debug)]
pub struct CreateBackup {
    target: BackupTarget,
    destination_id: DestinationId,
    schedule: String,
    enabled: bool,
    prefix: String,
    database: String,
    keep_latest_count: Option<NonZeroU32>,
    include_encryption_key: bool,
}

impl CreateBackup {
    /// Creates the complete desired Backup payload for a supported database.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        target: BackupTarget,
        destination_id: DestinationId,
        schedule: impl Into<String>,
        enabled: bool,
        prefix: impl Into<String>,
        database: impl Into<String>,
        keep_latest_count: Option<NonZeroU32>,
        include_encryption_key: bool,
    ) -> Self {
        Self {
            target,
            destination_id,
            schedule: schedule.into(),
            enabled,
            prefix: prefix.into(),
            database: database.into(),
            keep_latest_count,
            include_encryption_key,
        }
    }

    pub(crate) const fn target(&self) -> &BackupTarget {
        &self.target
    }

    pub(crate) const fn destination_id(&self) -> &DestinationId {
        &self.destination_id
    }

    pub(crate) fn is_valid(&self) -> bool {
        self.target.is_valid()
            && !self.destination_id.as_str().is_empty()
            && !self.schedule.is_empty()
            && !self.prefix.is_empty()
            && !self.database.is_empty()
    }

    pub(crate) fn matches(&self, details: &BackupDetails) -> bool {
        details.is_valid()
            && details.target == self.target
            && details.destination_id == self.destination_id
            && details.schedule == self.schedule
            && details.enabled == Some(self.enabled)
            && details.prefix == self.prefix
            && details.database == self.database
            && details.keep_latest_count == self.keep_latest_count
            && details.include_encryption_key == self.include_encryption_key
    }

    pub(crate) fn collision_key(&self) -> BackupCollisionKey {
        BackupCollisionKey::new(
            self.target.clone(),
            self.destination_id.clone(),
            &self.prefix,
            self.database.clone(),
        )
    }
}

impl Serialize for CreateBackup {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreateBackup", 12)?;
        body.serialize_field("schedule", &self.schedule)?;
        body.serialize_field("enabled", &self.enabled)?;
        body.serialize_field("prefix", &self.prefix)?;
        body.serialize_field("destinationId", self.destination_id.as_str())?;
        body.serialize_field(
            "keepLatestCount",
            &self.keep_latest_count.map(NonZeroU32::get),
        )?;
        body.serialize_field("database", &self.database)?;
        body.serialize_field("databaseType", self.target.database_type())?;
        body.serialize_field("backupType", "database")?;
        body.serialize_field("serviceName", &Option::<String>::None)?;
        body.serialize_field("includeEncryptionKey", &self.include_encryption_key)?;
        body.serialize_field("metadata", &Option::<()>::None)?;
        match &self.target {
            BackupTarget::Postgres(id) => body.serialize_field("postgresId", id.as_str())?,
            BackupTarget::MySql(id) => body.serialize_field("mysqlId", id.as_str())?,
            BackupTarget::MariaDb(id) => body.serialize_field("mariadbId", id.as_str())?,
            BackupTarget::Mongo(id) => body.serialize_field("mongoId", id.as_str())?,
            BackupTarget::LibSql(id) => body.serialize_field("libsqlId", id.as_str())?,
        }
        body.end()
    }
}

/// Physical identity discovered after Dokploy creates one Backup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedBackup {
    backup_id: BackupId,
}

impl CreatedBackup {
    pub(crate) const fn new(backup_id: BackupId) -> Self {
        Self { backup_id }
    }

    /// Returns the created Backup identity.
    #[must_use]
    pub const fn backup_id(&self) -> &BackupId {
        &self.backup_id
    }
}

/// Every mutable database Backup field plus the immutable target proof.
#[derive(Clone, Debug)]
pub struct UpdateBackup {
    backup_id: BackupId,
    target: BackupTarget,
    destination_id: DestinationId,
    schedule: String,
    enabled: bool,
    prefix: String,
    database: String,
    keep_latest_count: Option<NonZeroU32>,
    include_encryption_key: bool,
}

impl UpdateBackup {
    /// Replaces every mutable field without changing the Backup target.
    #[must_use]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        backup_id: BackupId,
        target: BackupTarget,
        destination_id: DestinationId,
        schedule: impl Into<String>,
        enabled: bool,
        prefix: impl Into<String>,
        database: impl Into<String>,
        keep_latest_count: Option<NonZeroU32>,
        include_encryption_key: bool,
    ) -> Self {
        Self {
            backup_id,
            target,
            destination_id,
            schedule: schedule.into(),
            enabled,
            prefix: prefix.into(),
            database: database.into(),
            keep_latest_count,
            include_encryption_key,
        }
    }

    pub(crate) const fn backup_id(&self) -> &BackupId {
        &self.backup_id
    }

    pub(crate) const fn target(&self) -> &BackupTarget {
        &self.target
    }

    pub(crate) const fn destination_id(&self) -> &DestinationId {
        &self.destination_id
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.backup_id.as_str().is_empty()
            && self.target.is_valid()
            && !self.destination_id.as_str().is_empty()
            && !self.schedule.is_empty()
            && !self.prefix.is_empty()
            && !self.database.is_empty()
    }

    pub(crate) fn matches(&self, details: &BackupDetails) -> bool {
        details.backup_id == self.backup_id
            && details.target == self.target
            && details.destination_id == self.destination_id
            && details.schedule == self.schedule
            && details.enabled == Some(self.enabled)
            && details.prefix == self.prefix
            && details.database == self.database
            && details.keep_latest_count == self.keep_latest_count
            && details.include_encryption_key == self.include_encryption_key
    }

    pub(crate) fn collision_key(&self) -> BackupCollisionKey {
        BackupCollisionKey::new(
            self.target.clone(),
            self.destination_id.clone(),
            &self.prefix,
            self.database.clone(),
        )
    }
}

impl Serialize for UpdateBackup {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("UpdateBackup", 11)?;
        body.serialize_field("schedule", &self.schedule)?;
        body.serialize_field("enabled", &self.enabled)?;
        body.serialize_field("prefix", &self.prefix)?;
        body.serialize_field("backupId", self.backup_id.as_str())?;
        body.serialize_field("destinationId", self.destination_id.as_str())?;
        body.serialize_field("database", &self.database)?;
        body.serialize_field(
            "keepLatestCount",
            &self.keep_latest_count.map(NonZeroU32::get),
        )?;
        body.serialize_field("serviceName", &Option::<String>::None)?;
        body.serialize_field("metadata", &Option::<()>::None)?;
        body.serialize_field("databaseType", self.target.database_type())?;
        body.serialize_field("includeEncryptionKey", &self.include_encryption_key)?;
        body.end()
    }
}

/// The storage mechanism used by a Dokploy mount.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MountType {
    /// Bind a host path into the target service.
    Bind,
    /// Attach a named Docker volume to the target service.
    Volume,
    /// Materialize an opaque file and mount it into the target service.
    File,
}

impl MountType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Bind => "bind",
            Self::Volume => "volume",
            Self::File => "file",
        }
    }
}

/// The exact Dokploy service that owns a mount.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServiceTarget {
    /// An application target.
    Application(ApplicationId),
    /// A raw Compose target.
    Compose(ComposeId),
    /// A LibSQL target.
    LibSql(LibSqlId),
    /// A MariaDB target.
    MariaDb(MariaDbId),
    /// A MongoDB target.
    Mongo(MongoId),
    /// A MySQL target.
    MySql(MySqlId),
    /// A PostgreSQL target.
    Postgres(PostgresId),
    /// A Redis target.
    Redis(RedisId),
}

impl ServiceTarget {
    /// Returns the target kind accepted by Dokploy's Mount operations.
    #[must_use]
    pub const fn service_type(&self) -> &'static str {
        match self {
            Self::Application(_) => "application",
            Self::Compose(_) => "compose",
            Self::LibSql(_) => "libsql",
            Self::MariaDb(_) => "mariadb",
            Self::Mongo(_) => "mongo",
            Self::MySql(_) => "mysql",
            Self::Postgres(_) => "postgres",
            Self::Redis(_) => "redis",
        }
    }

    /// Returns the opaque physical identity of the target.
    #[must_use]
    pub fn service_id(&self) -> &str {
        match self {
            Self::Application(id) => id.as_str(),
            Self::Compose(id) => id.as_str(),
            Self::LibSql(id) => id.as_str(),
            Self::MariaDb(id) => id.as_str(),
            Self::Mongo(id) => id.as_str(),
            Self::MySql(id) => id.as_str(),
            Self::Postgres(id) => id.as_str(),
            Self::Redis(id) => id.as_str(),
        }
    }
}

enum MountSource {
    Bind {
        host_path: String,
    },
    Volume {
        volume_name: String,
    },
    File {
        file_path: String,
        content: Zeroizing<String>,
    },
}

impl MountSource {
    const fn mount_type(&self) -> MountType {
        match self {
            Self::Bind { .. } => MountType::Bind,
            Self::Volume { .. } => MountType::Volume,
            Self::File { .. } => MountType::File,
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Bind { host_path } => !host_path.is_empty(),
            Self::Volume { volume_name } => !volume_name.is_empty(),
            Self::File { file_path, .. } => !file_path.is_empty(),
        }
    }

    fn matches(&self, details: &MountDetails) -> bool {
        match self {
            Self::Bind { host_path } => {
                details.host_path == ResponseField::Value(host_path.clone())
                    && !matches!(details.volume_name, ResponseField::Value(_))
                    && !matches!(details.file_path, ResponseField::Value(_))
            }
            Self::Volume { volume_name } => {
                details.volume_name == ResponseField::Value(volume_name.clone())
                    && !matches!(details.host_path, ResponseField::Value(_))
                    && !matches!(details.file_path, ResponseField::Value(_))
            }
            Self::File { file_path, .. } => {
                details.file_path == ResponseField::Value(file_path.clone())
                    && !matches!(details.host_path, ResponseField::Value(_))
                    && !matches!(details.volume_name, ResponseField::Value(_))
            }
        }
    }
}

impl fmt::Debug for MountSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bind { host_path } => formatter
                .debug_struct("Bind")
                .field("host_path", host_path)
                .finish(),
            Self::Volume { volume_name } => formatter
                .debug_struct("Volume")
                .field("volume_name", volume_name)
                .finish(),
            Self::File { file_path, .. } => formatter
                .debug_struct("File")
                .field("file_path", file_path)
                .field("content", &"[REDACTED]")
                .finish(),
        }
    }
}

/// Inputs required to create one typed Dokploy mount.
///
/// File content can contain credentials. It is retained in zeroizing memory
/// and is always omitted from debug output.
#[derive(Debug)]
pub struct CreateMount {
    target: ServiceTarget,
    mount_path: String,
    source: MountSource,
}

impl CreateMount {
    /// Creates a host bind mount.
    #[must_use]
    pub fn bind(
        target: ServiceTarget,
        host_path: impl Into<String>,
        mount_path: impl Into<String>,
    ) -> Self {
        Self {
            target,
            mount_path: mount_path.into(),
            source: MountSource::Bind {
                host_path: host_path.into(),
            },
        }
    }

    /// Creates a named Docker volume mount.
    #[must_use]
    pub fn volume(
        target: ServiceTarget,
        volume_name: impl Into<String>,
        mount_path: impl Into<String>,
    ) -> Self {
        Self {
            target,
            mount_path: mount_path.into(),
            source: MountSource::Volume {
                volume_name: volume_name.into(),
            },
        }
    }

    /// Creates an opaque file mount.
    #[must_use]
    pub fn file(
        target: ServiceTarget,
        file_path: impl Into<String>,
        mount_path: impl Into<String>,
        content: Zeroizing<String>,
    ) -> Self {
        Self {
            target,
            mount_path: mount_path.into(),
            source: MountSource::File {
                file_path: file_path.into(),
                content,
            },
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.target.service_id().is_empty()
            && !self.mount_path.is_empty()
            && self.source.is_valid()
    }

    pub(crate) fn matches(&self, details: &MountDetails) -> bool {
        !details.mount_id.as_str().is_empty()
            && details.target == self.target
            && details.mount_type == self.source.mount_type()
            && details.mount_path == self.mount_path
            && self.source.matches(details)
    }
}

impl Serialize for CreateMount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("CreateMount", 6)?;
        body.serialize_field("type", self.source.mount_type().as_str())?;
        match &self.source {
            MountSource::Bind { host_path } => body.serialize_field("hostPath", host_path)?,
            MountSource::Volume { volume_name } => {
                body.serialize_field("volumeName", volume_name)?;
            }
            MountSource::File { file_path, content } => {
                body.serialize_field("filePath", file_path)?;
                body.serialize_field("content", content.as_str())?;
            }
        }
        body.serialize_field("mountPath", &self.mount_path)?;
        body.serialize_field("serviceType", self.target.service_type())?;
        body.serialize_field("serviceId", self.target.service_id())?;
        body.end()
    }
}

/// Physical identity returned by Dokploy when a mount is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMount {
    mount_id: MountId,
}

impl CreatedMount {
    pub(crate) fn new(mount_id: MountId) -> Self {
        Self { mount_id }
    }

    /// Returns the new Mount identity.
    #[must_use]
    pub const fn mount_id(&self) -> &MountId {
        &self.mount_id
    }
}

/// An explicit subset of owned Mount fields to update.
pub struct UpdateMount {
    mount_id: MountId,
    mount_path: Option<String>,
    source: Option<MountSource>,
}

impl UpdateMount {
    /// Starts a Mount update with no fields selected.
    #[must_use]
    pub fn new(mount_id: MountId) -> Self {
        Self {
            mount_id,
            mount_path: None,
            source: None,
        }
    }

    /// Replaces the target path inside the service.
    #[must_use]
    pub fn with_mount_path(mut self, mount_path: impl Into<String>) -> Self {
        self.mount_path = Some(mount_path.into());
        self
    }

    /// Replaces the storage mechanism with a host bind source.
    #[must_use]
    pub fn with_bind(mut self, host_path: impl Into<String>) -> Self {
        self.source = Some(MountSource::Bind {
            host_path: host_path.into(),
        });
        self
    }

    /// Replaces the storage mechanism with a named Docker volume source.
    #[must_use]
    pub fn with_volume(mut self, volume_name: impl Into<String>) -> Self {
        self.source = Some(MountSource::Volume {
            volume_name: volume_name.into(),
        });
        self
    }

    /// Replaces the storage mechanism with an opaque file source.
    #[must_use]
    pub fn with_file(mut self, file_path: impl Into<String>, content: Zeroizing<String>) -> Self {
        self.source = Some(MountSource::File {
            file_path: file_path.into(),
            content,
        });
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mount_id.as_str().is_empty()
            && (self
                .mount_path
                .as_ref()
                .is_some_and(|mount_path| !mount_path.is_empty())
                || self.source.as_ref().is_some_and(MountSource::is_valid))
    }
}

impl fmt::Debug for UpdateMount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMount")
            .field("mount_id", &self.mount_id)
            .field("mount_path", &self.mount_path)
            .field("source", &self.source)
            .finish()
    }
}

impl Serialize for UpdateMount {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let source_fields = match &self.source {
            Some(MountSource::File { .. }) => 3,
            Some(MountSource::Bind { .. } | MountSource::Volume { .. }) => 2,
            None => 0,
        };
        let mut body = serializer.serialize_struct(
            "UpdateMount",
            1 + usize::from(self.mount_path.is_some()) + source_fields,
        )?;
        body.serialize_field("mountId", self.mount_id.as_str())?;
        if let Some(mount_path) = &self.mount_path {
            body.serialize_field("mountPath", mount_path)?;
        }
        if let Some(source) = &self.source {
            body.serialize_field("type", source.mount_type().as_str())?;
            match source {
                MountSource::Bind { host_path } => body.serialize_field("hostPath", host_path)?,
                MountSource::Volume { volume_name } => {
                    body.serialize_field("volumeName", volume_name)?;
                }
                MountSource::File { file_path, content } => {
                    body.serialize_field("filePath", file_path)?;
                    body.serialize_field("content", content.as_str())?;
                }
            }
        }
        body.end()
    }
}

fn valid_application_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

/// The topology role of one LibSQL node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LibSqlNode {
    /// A writable primary node.
    Primary,
    /// A replica attached to the supplied primary URL.
    Replica { primary_url: String },
}

impl LibSqlNode {
    fn wire_name(&self) -> &'static str {
        match self {
            Self::Primary => "primary",
            Self::Replica { .. } => "replica",
        }
    }

    fn primary_url(&self) -> Option<&str> {
        match self {
            Self::Primary => None,
            Self::Replica { primary_url } => Some(primary_url),
        }
    }

    fn is_valid(&self) -> bool {
        match self {
            Self::Primary => true,
            Self::Replica { primary_url } => !primary_url.is_empty(),
        }
    }
}

/// Inputs required to create one Dokploy project.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateProject {
    pub(crate) name: String,
    pub(crate) description: Option<String>,
}

impl CreateProject {
    /// Creates project input with no managed description.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: None,
        }
    }

    /// Sets the initial project description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Physical identities returned by Dokploy when a project is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedProject {
    project_id: ProjectId,
    default_environment_id: EnvironmentId,
    default_environment_name: String,
}

impl CreatedProject {
    pub(crate) fn from_response(response: ProjectCreateResponse) -> Self {
        Self {
            project_id: response.project.project_id,
            default_environment_id: response.environment.environment_id,
            default_environment_name: response.environment.name,
        }
    }

    /// Returns the new project identity.
    #[must_use]
    pub const fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    /// Returns the identity of the default environment created with the project.
    #[must_use]
    pub const fn default_environment_id(&self) -> &EnvironmentId {
        &self.default_environment_id
    }

    /// Returns the name of the default environment created with the project.
    #[must_use]
    pub fn default_environment_name(&self) -> &str {
        &self.default_environment_name
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ProjectCreateResponse {
    project: CreatedProjectResponse,
    environment: CreatedEnvironmentResponse,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedProjectResponse {
    project_id: ProjectId,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreatedEnvironmentResponse {
    environment_id: EnvironmentId,
    name: String,
}

/// Inputs required to create one Dokploy environment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateEnvironment {
    pub(crate) name: String,
    pub(crate) project_id: ProjectId,
    pub(crate) description: Option<String>,
}

impl CreateEnvironment {
    /// Creates environment input with no managed description.
    #[must_use]
    pub fn new(name: impl Into<String>, project_id: ProjectId) -> Self {
        Self {
            name: name.into(),
            project_id,
            description: None,
        }
    }

    /// Sets the initial environment description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }
}

/// Physical identity returned by Dokploy when an environment is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedEnvironment {
    environment_id: EnvironmentId,
}

impl CreatedEnvironment {
    pub(crate) fn from_response(response: EnvironmentCreateResponse) -> Self {
        Self {
            environment_id: response.environment_id,
        }
    }

    /// Returns the new environment identity.
    #[must_use]
    pub const fn environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct EnvironmentCreateResponse {
    environment_id: EnvironmentId,
}

/// The managed project description written by one update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateProject {
    project_id: ProjectId,
    description: Nullable<String>,
}

impl UpdateProject {
    /// Selects the exact project description state.
    #[must_use]
    pub fn new(project_id: ProjectId, description: Nullable<String>) -> Self {
        Self {
            project_id,
            description,
        }
    }
}

/// The managed environment description written by one update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateEnvironment {
    environment_id: EnvironmentId,
    description: Nullable<String>,
}

impl UpdateEnvironment {
    /// Selects the exact environment description state.
    #[must_use]
    pub fn new(environment_id: EnvironmentId, description: Nullable<String>) -> Self {
        Self {
            environment_id,
            description,
        }
    }
}

/// Inputs required to create one Dokploy application record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateApplication {
    pub(crate) name: String,
    pub(crate) environment_id: EnvironmentId,
    server_placement: Option<ServerPlacement>,
}

impl CreateApplication {
    /// Creates minimal application input for later explicit configuration.
    #[must_use]
    pub fn new(name: impl Into<String>, environment_id: EnvironmentId) -> Self {
        Self {
            name: name.into(),
            environment_id,
            server_placement: None,
        }
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl Serialize for CreateApplication {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateApplication",
            2 + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when an application is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedApplication {
    application_id: ApplicationId,
}

impl CreatedApplication {
    pub(crate) fn from_response(response: ApplicationCreateResponse) -> Self {
        Self {
            application_id: response.application_id,
        }
    }

    /// Returns the new application identity.
    #[must_use]
    pub const fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationCreateResponse {
    pub(crate) application_id: ApplicationId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// A transient application environment document used only to preserve unowned entries.
pub struct ApplicationEnvironmentDocument(Zeroizing<String>);

impl ApplicationEnvironmentDocument {
    pub(crate) fn from_response(response: ApplicationEnvironmentResponse) -> Self {
        Self(Zeroizing::new(response.environment.unwrap_or_default()))
    }

    /// Transfers the transient environment document to the mutation composer.
    #[must_use]
    pub fn into_document(self) -> Zeroizing<String> {
        self.0
    }
}

impl fmt::Debug for ApplicationEnvironmentDocument {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ApplicationEnvironmentDocument([REDACTED])")
    }
}

#[derive(Deserialize)]
pub(crate) struct ApplicationEnvironmentResponse {
    #[serde(default, rename = "env")]
    environment: Option<String>,
}

/// Owned application fields written by one `application.update` request.
pub struct UpdateApplication {
    application_id: ApplicationId,
    description: Option<Nullable<String>>,
    replicas: Option<Nullable<u32>>,
    source_type: Option<Nullable<String>>,
    repository: Option<Nullable<String>>,
    branch: Option<Nullable<String>>,
    environment: Option<Nullable<Zeroizing<String>>>,
    environment_id: Option<EnvironmentId>,
    build_server_id: Option<Nullable<ServerId>>,
    registry_id: Option<Nullable<RegistryId>>,
    build_registry_id: Option<Nullable<RegistryId>>,
    rollback_registry_id: Option<Nullable<RegistryId>>,
}

impl UpdateApplication {
    /// Starts an application update with no fields selected.
    #[must_use]
    pub fn new(application_id: ApplicationId) -> Self {
        Self {
            application_id,
            description: None,
            replicas: None,
            source_type: None,
            repository: None,
            branch: None,
            environment: None,
            environment_id: None,
            build_server_id: None,
            registry_id: None,
            build_registry_id: None,
            rollback_registry_id: None,
        }
    }

    /// Selects the application description.
    #[must_use]
    pub fn with_description(mut self, description: Nullable<String>) -> Self {
        self.description = Some(description);
        self
    }

    /// Selects the replica count.
    #[must_use]
    pub fn with_replicas(mut self, replicas: Nullable<u32>) -> Self {
        self.replicas = Some(replicas);
        self
    }

    /// Clears the complete source configuration.
    #[must_use]
    pub fn clearing_source(mut self) -> Self {
        self.source_type = Some(Nullable::Null);
        self.repository = Some(Nullable::Null);
        self.branch = Some(Nullable::Null);
        self
    }

    /// Selects a GitHub repository while leaving branch ownership unchanged.
    #[must_use]
    pub fn with_github_repository(mut self, repository: impl Into<String>) -> Self {
        self.source_type = Some(Nullable::Value("github".to_owned()));
        self.repository = Some(Nullable::Value(repository.into()));
        self
    }

    /// Selects the GitHub branch independently from the repository.
    #[must_use]
    pub fn with_branch(mut self, branch: Nullable<String>) -> Self {
        self.branch = Some(branch);
        self
    }

    /// Replaces or clears the complete Dokploy environment document.
    #[must_use]
    pub fn with_environment(mut self, environment: Nullable<Zeroizing<String>>) -> Self {
        self.environment = Some(environment);
        self
    }

    /// Moves the application to another physical environment.
    #[must_use]
    pub fn with_environment_id(mut self, environment_id: EnvironmentId) -> Self {
        self.environment_id = Some(environment_id);
        self
    }

    /// Selects or clears the external build server association.
    #[must_use]
    pub fn with_build_server(mut self, server_id: Nullable<ServerId>) -> Self {
        self.build_server_id = Some(server_id);
        self
    }

    /// Selects or clears the runtime image registry association.
    #[must_use]
    pub fn with_registry(mut self, registry_id: Nullable<RegistryId>) -> Self {
        self.registry_id = Some(registry_id);
        self
    }

    /// Selects or clears the build image registry association.
    #[must_use]
    pub fn with_build_registry(mut self, registry_id: Nullable<RegistryId>) -> Self {
        self.build_registry_id = Some(registry_id);
        self
    }

    /// Selects or clears the rollback image registry association.
    #[must_use]
    pub fn with_rollback_registry(mut self, registry_id: Nullable<RegistryId>) -> Self {
        self.rollback_registry_id = Some(registry_id);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.application_id.as_str().is_empty()
            && (self.description.is_some()
                || self.replicas.is_some()
                || self.source_type.is_some()
                || self.repository.is_some()
                || self.branch.is_some()
                || self.environment.is_some()
                || self.environment_id.is_some()
                || self.build_server_id.is_some()
                || self.registry_id.is_some()
                || self.build_registry_id.is_some()
                || self.rollback_registry_id.is_some())
            && nullable_identifier_is_valid(self.build_server_id.as_ref())
            && nullable_identifier_is_valid(self.registry_id.as_ref())
            && nullable_identifier_is_valid(self.build_registry_id.as_ref())
            && nullable_identifier_is_valid(self.rollback_registry_id.as_ref())
    }
}

impl fmt::Debug for UpdateApplication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateApplication")
            .field("application_id", &self.application_id)
            .field("description", &self.description)
            .field("replicas", &self.replicas)
            .field("source_type", &self.source_type)
            .field("repository", &self.repository)
            .field("branch", &self.branch)
            .field(
                "environment",
                &self.environment.as_ref().map(|_| "[REDACTED]"),
            )
            .field("environment_id", &self.environment_id)
            .field("build_server_id", &self.build_server_id)
            .field("registry_id", &self.registry_id)
            .field("build_registry_id", &self.build_registry_id)
            .field("rollback_registry_id", &self.rollback_registry_id)
            .finish()
    }
}

impl Serialize for UpdateApplication {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut fields = 1;
        fields += usize::from(self.description.is_some());
        fields += usize::from(self.replicas.is_some());
        fields += usize::from(self.source_type.is_some());
        fields += usize::from(self.repository.is_some());
        fields += usize::from(self.branch.is_some());
        fields += usize::from(self.environment.is_some());
        fields += usize::from(self.environment_id.is_some());
        fields += usize::from(self.build_server_id.is_some());
        fields += usize::from(self.registry_id.is_some());
        fields += usize::from(self.build_registry_id.is_some());
        fields += usize::from(self.rollback_registry_id.is_some());
        let mut body = serializer.serialize_struct("UpdateApplication", fields)?;
        body.serialize_field("applicationId", self.application_id.as_str())?;
        if let Some(description) = &self.description {
            body.serialize_field("description", description)?;
        }
        if let Some(replicas) = &self.replicas {
            body.serialize_field("replicas", replicas)?;
        }
        if let Some(source_type) = &self.source_type {
            body.serialize_field("sourceType", source_type)?;
        }
        if let Some(repository) = &self.repository {
            body.serialize_field("repository", repository)?;
        }
        if let Some(branch) = &self.branch {
            body.serialize_field("branch", branch)?;
        }
        if let Some(environment) = &self.environment {
            match environment {
                Nullable::Null => body.serialize_field("env", &Option::<&str>::None)?,
                Nullable::Value(value) => body.serialize_field("env", value.as_str())?,
            }
        }
        if let Some(environment_id) = &self.environment_id {
            body.serialize_field("environmentId", environment_id.as_str())?;
        }
        if let Some(build_server_id) = &self.build_server_id {
            body.serialize_field("buildServerId", build_server_id)?;
        }
        if let Some(registry_id) = &self.registry_id {
            body.serialize_field("registryId", registry_id)?;
        }
        if let Some(build_registry_id) = &self.build_registry_id {
            body.serialize_field("buildRegistryId", build_registry_id)?;
        }
        if let Some(rollback_registry_id) = &self.rollback_registry_id {
            body.serialize_field("rollbackRegistryId", rollback_registry_id)?;
        }
        body.end()
    }
}

fn nullable_identifier_is_valid<T>(value: Option<&Nullable<T>>) -> bool
where
    T: IdentifierValue,
{
    value.is_none_or(|value| match value {
        Nullable::Null => true,
        Nullable::Value(identifier) => !identifier.identifier_value().is_empty(),
    })
}

/// Inputs required to create one Dokploy Postgres database.
pub struct CreatePostgres {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
    server_placement: Option<ServerPlacement>,
}

impl CreatePostgres {
    /// Creates Postgres input while retaining the password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
            server_placement: None,
        }
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreatePostgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreatePostgres")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreatePostgres {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreatePostgres",
            5 + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when Postgres is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedPostgres {
    postgres_id: PostgresId,
}

impl CreatedPostgres {
    pub(crate) fn from_response(response: PostgresCreateResponse) -> Self {
        Self {
            postgres_id: response.postgres_id,
        }
    }

    /// Returns the new Postgres identity.
    #[must_use]
    pub const fn postgres_id(&self) -> &PostgresId {
        &self.postgres_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostgresCreateResponse {
    pub(crate) postgres_id: PostgresId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// Owned Postgres fields written by one update.
pub struct UpdatePostgres {
    postgres_id: PostgresId,
    database_name: Option<String>,
    database_user: Option<String>,
    database_password: Option<Zeroizing<String>>,
}

impl UpdatePostgres {
    /// Starts a Postgres update with no fields selected.
    #[must_use]
    pub fn new(postgres_id: PostgresId) -> Self {
        Self {
            postgres_id,
            database_name: None,
            database_user: None,
            database_password: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    /// Selects the write-only database password.
    #[must_use]
    pub fn with_password(mut self, database_password: Zeroizing<String>) -> Self {
        self.database_password = Some(database_password);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.postgres_id.as_str().is_empty()
            && (self.database_name.is_some()
                || self.database_user.is_some()
                || self.database_password.is_some())
    }
}

impl fmt::Debug for UpdatePostgres {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdatePostgres")
            .field("postgres_id", &self.postgres_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field(
                "database_password",
                &self.database_password.as_ref().map(|_| "[REDACTED]"),
            )
            .finish()
    }
}

impl Serialize for UpdatePostgres {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdatePostgres",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some())
                + usize::from(self.database_password.is_some()),
        )?;
        body.serialize_field("postgresId", self.postgres_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        if let Some(database_password) = &self.database_password {
            body.serialize_field("databasePassword", database_password.as_str())?;
        }
        body.end()
    }
}

/// Inputs required to create one Dokploy LibSQL database.
///
/// The project identity is retained only for authoritative topology discovery;
/// it is never serialized into the `libsql.create` body.
pub struct CreateLibSql {
    name: String,
    app_name: String,
    project_id: ProjectId,
    environment_id: EnvironmentId,
    description: Option<String>,
    database_user: String,
    database_password: Zeroizing<String>,
    node: LibSqlNode,
    enable_namespaces: bool,
    server_placement: Option<ServerPlacement>,
}

impl CreateLibSql {
    /// Creates LibSQL input with a write-only authentication credential.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        app_name: impl Into<String>,
        project_id: ProjectId,
        environment_id: EnvironmentId,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
        node: LibSqlNode,
    ) -> Self {
        Self {
            name: name.into(),
            app_name: app_name.into(),
            project_id,
            environment_id,
            description: None,
            database_user: database_user.into(),
            database_password,
            node,
            enable_namespaces: false,
            server_placement: None,
        }
    }

    /// Sets the initial description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Selects whether the database enables LibSQL namespaces at creation.
    #[must_use]
    pub const fn with_namespaces(mut self, enable_namespaces: bool) -> Self {
        self.enable_namespaces = enable_namespaces;
        self
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn project_id(&self) -> &ProjectId {
        &self.project_id
    }

    pub(crate) fn environment_id(&self) -> &EnvironmentId {
        &self.environment_id
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.app_name.is_empty()
            && !self.project_id.as_str().is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self.node.is_valid()
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateLibSql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateLibSql")
            .field("name", &self.name)
            .field("app_name", &self.app_name)
            .field("project_id", &self.project_id)
            .field("environment_id", &self.environment_id)
            .field("description", &self.description)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("node", &self.node)
            .field("enable_namespaces", &self.enable_namespaces)
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateLibSql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateLibSql",
            10 + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("appName", &self.app_name)?;
        body.serialize_field("dockerImage", LIBSQL_DEFAULT_IMAGE)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("description", &self.description)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.serialize_field("sqldNode", self.node.wire_name())?;
        body.serialize_field("sqldPrimaryUrl", &self.node.primary_url())?;
        body.serialize_field("enableNamespaces", &self.enable_namespaces)?;
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity discovered after Dokploy creates a LibSQL database.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedLibSql {
    libsql_id: LibSqlId,
}

impl CreatedLibSql {
    pub(crate) fn new(libsql_id: LibSqlId) -> Self {
        Self { libsql_id }
    }

    /// Returns the new LibSQL identity.
    #[must_use]
    pub const fn libsql_id(&self) -> &LibSqlId {
        &self.libsql_id
    }
}

/// Owned non-secret LibSQL fields written by one update.
pub struct UpdateLibSql {
    libsql_id: LibSqlId,
    description: Option<String>,
    database_user: Option<String>,
}

impl UpdateLibSql {
    /// Starts a LibSQL update with no fields selected.
    #[must_use]
    pub fn new(libsql_id: LibSqlId) -> Self {
        Self {
            libsql_id,
            description: None,
            database_user: None,
        }
    }

    /// Selects the description.
    #[must_use]
    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.libsql_id.as_str().is_empty()
            && (self.description.is_some()
                || self
                    .database_user
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateLibSql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateLibSql")
            .field("libsql_id", &self.libsql_id)
            .field("description", &self.description)
            .field("database_user", &self.database_user)
            .finish()
    }
}

impl Serialize for UpdateLibSql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateLibSql",
            1 + usize::from(self.description.is_some()) + usize::from(self.database_user.is_some()),
        )?;
        body.serialize_field("libsqlId", self.libsql_id.as_str())?;
        if let Some(description) = &self.description {
            body.serialize_field("description", description)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        body.end()
    }
}

/// A write-only update of the LibSQL authentication credential.
///
/// Dokploy `v0.30.6` calls this field `databasePassword`; it is the credential
/// used as the self-hosted LibSQL authentication token. There is no separate
/// token rotation endpoint in the pinned contract.
pub struct ChangeLibSqlPassword {
    libsql_id: LibSqlId,
    password: Zeroizing<String>,
}

impl ChangeLibSqlPassword {
    /// Creates an explicit LibSQL credential rotation.
    #[must_use]
    pub fn new(libsql_id: LibSqlId, password: Zeroizing<String>) -> Self {
        Self {
            libsql_id,
            password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.libsql_id.as_str().is_empty() && !self.password.is_empty()
    }
}

impl fmt::Debug for ChangeLibSqlPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeLibSqlPassword")
            .field("libsql_id", &self.libsql_id)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for ChangeLibSqlPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeLibSqlPassword", 2)?;
        body.serialize_field("libsqlId", self.libsql_id.as_str())?;
        body.serialize_field("databasePassword", self.password.as_str())?;
        body.end()
    }
}

/// Inputs required to create one Dokploy MongoDB database.
pub struct CreateMongo {
    name: String,
    environment_id: EnvironmentId,
    database_user: String,
    database_password: Zeroizing<String>,
    replica_sets: Option<bool>,
    server_placement: Option<ServerPlacement>,
}

impl CreateMongo {
    /// Creates MongoDB input while retaining the required password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_user: database_user.into(),
            database_password,
            replica_sets: None,
            server_placement: None,
        }
    }

    /// Selects whether Dokploy configures MongoDB replica sets.
    #[must_use]
    pub fn with_replica_sets(mut self, replica_sets: bool) -> Self {
        self.replica_sets = Some(replica_sets);
        self
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self
                .database_password
                .chars()
                .all(valid_database_password_character)
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateMongo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMongo")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("replica_sets", &self.replica_sets)
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateMongo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateMongo",
            4 + usize::from(self.replica_sets.is_some())
                + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(replica_sets) = self.replica_sets {
            body.serialize_field("replicaSets", &replica_sets)?;
        }
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when MongoDB is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMongo {
    mongo_id: MongoId,
}

impl CreatedMongo {
    pub(crate) fn from_response(response: MongoCreateResponse) -> Self {
        Self {
            mongo_id: response.mongo_id,
        }
    }

    /// Returns the new MongoDB identity.
    #[must_use]
    pub const fn mongo_id(&self) -> &MongoId {
        &self.mongo_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MongoCreateResponse {
    pub(crate) mongo_id: MongoId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// Owned non-secret MongoDB fields written by one update.
pub struct UpdateMongo {
    mongo_id: MongoId,
    database_user: Option<String>,
    replica_sets: Option<bool>,
}

impl UpdateMongo {
    /// Starts a MongoDB update with no fields selected.
    #[must_use]
    pub fn new(mongo_id: MongoId) -> Self {
        Self {
            mongo_id,
            database_user: None,
            replica_sets: None,
        }
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    /// Selects whether Dokploy configures MongoDB replica sets.
    #[must_use]
    pub fn with_replica_sets(mut self, replica_sets: bool) -> Self {
        self.replica_sets = Some(replica_sets);
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mongo_id.as_str().is_empty()
            && (self
                .database_user
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self.replica_sets.is_some())
    }
}

impl fmt::Debug for UpdateMongo {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMongo")
            .field("mongo_id", &self.mongo_id)
            .field("database_user", &self.database_user)
            .field("replica_sets", &self.replica_sets)
            .finish()
    }
}

impl Serialize for UpdateMongo {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMongo",
            1 + usize::from(self.database_user.is_some())
                + usize::from(self.replica_sets.is_some()),
        )?;
        body.serialize_field("mongoId", self.mongo_id.as_str())?;
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        if let Some(replica_sets) = self.replica_sets {
            body.serialize_field("replicaSets", &replica_sets)?;
        }
        body.end()
    }
}

/// A write-only MongoDB password rotation.
pub struct ChangeMongoPassword {
    mongo_id: MongoId,
    password: Zeroizing<String>,
}

impl ChangeMongoPassword {
    /// Creates an explicit MongoDB password rotation.
    #[must_use]
    pub fn new(mongo_id: MongoId, password: Zeroizing<String>) -> Self {
        Self { mongo_id, password }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mongo_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMongoPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMongoPassword")
            .field("mongo_id", &self.mongo_id)
            .field("password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for ChangeMongoPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMongoPassword", 2)?;
        body.serialize_field("mongoId", self.mongo_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.end()
    }
}

/// Inputs required to create one Dokploy MariaDB database.
pub struct CreateMariaDb {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
    database_root_password: Option<Zeroizing<String>>,
    server_placement: Option<ServerPlacement>,
}

impl CreateMariaDb {
    /// Creates MariaDB input while retaining the required user password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
            database_root_password: None,
            server_placement: None,
        }
    }

    /// Supplies an explicit root password instead of allowing Dokploy to generate one.
    #[must_use]
    pub fn with_root_password(mut self, database_root_password: Zeroizing<String>) -> Self {
        self.database_root_password = Some(database_root_password);
        self
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && self
                .database_password
                .chars()
                .all(valid_database_password_character)
            && self.database_root_password.as_ref().is_none_or(|password| {
                !password.is_empty() && password.chars().all(valid_database_password_character)
            })
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateMariaDb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMariaDb")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field(
                "database_root_password",
                &self.database_root_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateMariaDb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateMariaDb",
            5 + usize::from(self.database_root_password.is_some())
                + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(database_root_password) = &self.database_root_password {
            body.serialize_field("databaseRootPassword", database_root_password.as_str())?;
        }
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when MariaDB is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMariaDb {
    mariadb_id: MariaDbId,
}

impl CreatedMariaDb {
    pub(crate) fn from_response(response: MariaDbCreateResponse) -> Self {
        Self {
            mariadb_id: response.mariadb_id,
        }
    }

    /// Returns the new MariaDB identity.
    #[must_use]
    pub const fn mariadb_id(&self) -> &MariaDbId {
        &self.mariadb_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MariaDbCreateResponse {
    pub(crate) mariadb_id: MariaDbId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// Owned non-secret MariaDB fields written by one update.
pub struct UpdateMariaDb {
    mariadb_id: MariaDbId,
    database_name: Option<String>,
    database_user: Option<String>,
}

impl UpdateMariaDb {
    /// Starts a MariaDB update with no fields selected.
    #[must_use]
    pub fn new(mariadb_id: MariaDbId) -> Self {
        Self {
            mariadb_id,
            database_name: None,
            database_user: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mariadb_id.as_str().is_empty()
            && (self
                .database_name
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self
                    .database_user
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateMariaDb {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMariaDb")
            .field("mariadb_id", &self.mariadb_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .finish()
    }
}

impl Serialize for UpdateMariaDb {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMariaDb",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some()),
        )?;
        body.serialize_field("mariadbId", self.mariadb_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        body.end()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MariaDbPasswordTarget {
    User,
    Root,
}

impl MariaDbPasswordTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Root => "root",
        }
    }
}

/// A write-only MariaDB user or root password rotation.
pub struct ChangeMariaDbPassword {
    mariadb_id: MariaDbId,
    password: Zeroizing<String>,
    target: MariaDbPasswordTarget,
}

impl ChangeMariaDbPassword {
    /// Rotates the configured database user's password.
    #[must_use]
    pub fn user(mariadb_id: MariaDbId, password: Zeroizing<String>) -> Self {
        Self {
            mariadb_id,
            password,
            target: MariaDbPasswordTarget::User,
        }
    }

    /// Rotates the MariaDB root password.
    #[must_use]
    pub fn root(mariadb_id: MariaDbId, password: Zeroizing<String>) -> Self {
        Self {
            mariadb_id,
            password,
            target: MariaDbPasswordTarget::Root,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mariadb_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMariaDbPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMariaDbPassword")
            .field("mariadb_id", &self.mariadb_id)
            .field("password", &"[REDACTED]")
            .field("target", &self.target)
            .finish()
    }
}

impl Serialize for ChangeMariaDbPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMariaDbPassword", 3)?;
        body.serialize_field("mariadbId", self.mariadb_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.serialize_field("type", self.target.as_str())?;
        body.end()
    }
}

/// Inputs required to create one Dokploy MySQL database.
pub struct CreateMySql {
    name: String,
    environment_id: EnvironmentId,
    database_name: String,
    database_user: String,
    database_password: Zeroizing<String>,
    database_root_password: Zeroizing<String>,
    server_placement: Option<ServerPlacement>,
}

impl CreateMySql {
    /// Creates MySQL input while retaining both required passwords in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_name: impl Into<String>,
        database_user: impl Into<String>,
        database_password: Zeroizing<String>,
        database_root_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_name: database_name.into(),
            database_user: database_user.into(),
            database_password,
            database_root_password,
            server_placement: None,
        }
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_name.is_empty()
            && !self.database_user.is_empty()
            && !self.database_password.is_empty()
            && !self.database_root_password.is_empty()
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateMySql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateMySql")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .field("database_password", &"[REDACTED]")
            .field("database_root_password", &"[REDACTED]")
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateMySql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateMySql",
            6 + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databaseName", &self.database_name)?;
        body.serialize_field("databaseUser", &self.database_user)?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.serialize_field("databaseRootPassword", self.database_root_password.as_str())?;
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when MySQL is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedMySql {
    mysql_id: MySqlId,
}

impl CreatedMySql {
    pub(crate) fn from_response(response: MySqlCreateResponse) -> Self {
        Self {
            mysql_id: response.mysql_id,
        }
    }

    /// Returns the new MySQL identity.
    #[must_use]
    pub const fn mysql_id(&self) -> &MySqlId {
        &self.mysql_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MySqlCreateResponse {
    pub(crate) mysql_id: MySqlId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// Owned non-secret MySQL fields written by one update.
pub struct UpdateMySql {
    mysql_id: MySqlId,
    database_name: Option<String>,
    database_user: Option<String>,
}

impl UpdateMySql {
    /// Starts a MySQL update with no fields selected.
    #[must_use]
    pub fn new(mysql_id: MySqlId) -> Self {
        Self {
            mysql_id,
            database_name: None,
            database_user: None,
        }
    }

    /// Selects the database name.
    #[must_use]
    pub fn with_database(mut self, database_name: impl Into<String>) -> Self {
        self.database_name = Some(database_name.into());
        self
    }

    /// Selects the database user.
    #[must_use]
    pub fn with_username(mut self, database_user: impl Into<String>) -> Self {
        self.database_user = Some(database_user.into());
        self
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mysql_id.as_str().is_empty()
            && (self
                .database_name
                .as_ref()
                .is_some_and(|value| !value.is_empty())
                || self
                    .database_user
                    .as_ref()
                    .is_some_and(|value| !value.is_empty()))
    }
}

impl fmt::Debug for UpdateMySql {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateMySql")
            .field("mysql_id", &self.mysql_id)
            .field("database_name", &self.database_name)
            .field("database_user", &self.database_user)
            .finish()
    }
}

impl Serialize for UpdateMySql {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "UpdateMySql",
            1 + usize::from(self.database_name.is_some())
                + usize::from(self.database_user.is_some()),
        )?;
        body.serialize_field("mysqlId", self.mysql_id.as_str())?;
        if let Some(database_name) = &self.database_name {
            body.serialize_field("databaseName", database_name)?;
        }
        if let Some(database_user) = &self.database_user {
            body.serialize_field("databaseUser", database_user)?;
        }
        body.end()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MySqlPasswordTarget {
    User,
    Root,
}

impl MySqlPasswordTarget {
    const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Root => "root",
        }
    }
}

/// A write-only MySQL user or root password rotation.
pub struct ChangeMySqlPassword {
    mysql_id: MySqlId,
    password: Zeroizing<String>,
    target: MySqlPasswordTarget,
}

impl ChangeMySqlPassword {
    /// Rotates the configured database user's password.
    #[must_use]
    pub fn user(mysql_id: MySqlId, password: Zeroizing<String>) -> Self {
        Self {
            mysql_id,
            password,
            target: MySqlPasswordTarget::User,
        }
    }

    /// Rotates the MySQL root password.
    #[must_use]
    pub fn root(mysql_id: MySqlId, password: Zeroizing<String>) -> Self {
        Self {
            mysql_id,
            password,
            target: MySqlPasswordTarget::Root,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.mysql_id.as_str().is_empty()
            && !self.password.is_empty()
            && self.password.chars().all(valid_database_password_character)
    }
}

impl fmt::Debug for ChangeMySqlPassword {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChangeMySqlPassword")
            .field("mysql_id", &self.mysql_id)
            .field("password", &"[REDACTED]")
            .field("target", &self.target)
            .finish()
    }
}

impl Serialize for ChangeMySqlPassword {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("ChangeMySqlPassword", 3)?;
        body.serialize_field("mysqlId", self.mysql_id.as_str())?;
        body.serialize_field("password", self.password.as_str())?;
        body.serialize_field("type", self.target.as_str())?;
        body.end()
    }
}

fn valid_database_password_character(character: char) -> bool {
    character.is_ascii_alphanumeric()
        || matches!(
            character,
            '@' | '#'
                | '%'
                | '^'
                | '&'
                | '*'
                | '('
                | ')'
                | '_'
                | '+'
                | '-'
                | '='
                | '['
                | ']'
                | '{'
                | '}'
                | '|'
                | ';'
                | ':'
                | ','
                | '.'
                | '<'
                | '>'
                | '?'
                | '~'
                | '`'
        )
}

/// Inputs required to create one Dokploy Redis database.
pub struct CreateRedis {
    name: String,
    environment_id: EnvironmentId,
    database_password: Zeroizing<String>,
    server_placement: Option<ServerPlacement>,
}

impl CreateRedis {
    /// Creates Redis input while retaining the password in zeroizing memory.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        environment_id: EnvironmentId,
        database_password: Zeroizing<String>,
    ) -> Self {
        Self {
            name: name.into(),
            environment_id,
            database_password,
            server_placement: None,
        }
    }

    /// Selects an explicit local or external server placement.
    #[must_use]
    pub fn with_server_placement(mut self, placement: ServerPlacement) -> Self {
        self.server_placement = Some(placement);
        self
    }

    pub(crate) const fn server_placement(&self) -> Option<&ServerPlacement> {
        self.server_placement.as_ref()
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.name.is_empty()
            && !self.environment_id.as_str().is_empty()
            && !self.database_password.is_empty()
            && self
                .server_placement
                .as_ref()
                .is_none_or(ServerPlacement::is_valid)
    }
}

impl fmt::Debug for CreateRedis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CreateRedis")
            .field("name", &self.name)
            .field("environment_id", &self.environment_id)
            .field("database_password", &"[REDACTED]")
            .field("server_placement", &self.server_placement)
            .finish()
    }
}

impl Serialize for CreateRedis {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct(
            "CreateRedis",
            3 + usize::from(self.server_placement.is_some()),
        )?;
        body.serialize_field("name", &self.name)?;
        body.serialize_field("environmentId", self.environment_id.as_str())?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        if let Some(placement) = &self.server_placement {
            match placement {
                ServerPlacement::Local => {
                    body.serialize_field("serverId", &Option::<&str>::None)?;
                }
                ServerPlacement::Server(server_id) => {
                    body.serialize_field("serverId", server_id.as_str())?;
                }
            }
        }
        body.end()
    }
}

/// Physical identity returned by Dokploy when Redis is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedRedis {
    redis_id: RedisId,
}

impl CreatedRedis {
    pub(crate) fn from_response(response: RedisCreateResponse) -> Self {
        Self {
            redis_id: response.redis_id,
        }
    }

    /// Returns the new Redis identity.
    #[must_use]
    pub const fn redis_id(&self) -> &RedisId {
        &self.redis_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedisCreateResponse {
    pub(crate) redis_id: RedisId,
    #[serde(default)]
    pub(crate) server_id: ResponseField<ServerId>,
}

/// A write-only Redis password update.
pub struct UpdateRedis {
    redis_id: RedisId,
    database_password: Zeroizing<String>,
}

impl UpdateRedis {
    /// Selects the exact Redis password intent.
    #[must_use]
    pub fn new(redis_id: RedisId, database_password: Zeroizing<String>) -> Self {
        Self {
            redis_id,
            database_password,
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.redis_id.as_str().is_empty() && !self.database_password.is_empty()
    }
}

impl fmt::Debug for UpdateRedis {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpdateRedis")
            .field("redis_id", &self.redis_id)
            .field("database_password", &"[REDACTED]")
            .finish()
    }
}

impl Serialize for UpdateRedis {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut body = serializer.serialize_struct("UpdateRedis", 2)?;
        body.serialize_field("redisId", self.redis_id.as_str())?;
        body.serialize_field("databasePassword", self.database_password.as_str())?;
        body.end()
    }
}

/// Inputs required to attach one domain to a Dokploy application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateDomain {
    pub(crate) host: String,
    pub(crate) application_id: ApplicationId,
}

impl CreateDomain {
    /// Creates an application domain with Dokploy's transport defaults.
    #[must_use]
    pub fn new(host: impl Into<String>, application_id: ApplicationId) -> Self {
        Self {
            host: host.into(),
            application_id,
        }
    }
}

/// Physical identity returned by Dokploy when a domain is created.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreatedDomain {
    domain_id: DomainId,
}

impl CreatedDomain {
    pub(crate) fn from_response(response: DomainCreateResponse) -> Self {
        Self {
            domain_id: response.domain_id,
        }
    }

    /// Returns the new domain identity.
    #[must_use]
    pub const fn domain_id(&self) -> &DomainId {
        &self.domain_id
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DomainCreateResponse {
    domain_id: DomainId,
}

/// The required host pair written by one Domain update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDomain {
    domain_id: DomainId,
    host: String,
}

impl UpdateDomain {
    /// Selects the exact Domain host.
    #[must_use]
    pub fn new(domain_id: DomainId, host: impl Into<String>) -> Self {
        Self {
            domain_id,
            host: host.into(),
        }
    }

    pub(crate) fn is_valid(&self) -> bool {
        !self.domain_id.as_str().is_empty() && !self.host.is_empty()
    }
}

/// Presence-aware value returned by a tolerant Dokploy response model.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum ResponseField<T> {
    /// Dokploy omitted the field, so its current value is unknown.
    #[default]
    NotReturned,
    /// Dokploy returned an explicit JSON null.
    Null,
    /// Dokploy returned a concrete value.
    Value(T),
}

impl<'de, T> Deserialize<'de> for ResponseField<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Option::<T>::deserialize(deserializer)?.map_or(Self::Null, Self::Value))
    }
}

/// A stable subset of the response returned by `application.one`.
///
/// Unknown response fields are intentionally ignored. Dokploy's OpenAPI
/// document currently declares an empty response object, while the runtime
/// handler returns the application and its relations.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationDetails {
    pub application_id: ApplicationId,
    pub name: String,
    pub app_name: String,
    pub environment_id: EnvironmentId,
    #[serde(default)]
    pub source_type: ResponseField<String>,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub replicas: ResponseField<u32>,
    #[serde(default)]
    pub repository: ResponseField<String>,
    #[serde(default)]
    pub branch: ResponseField<String>,
    #[serde(default)]
    pub build_path: ResponseField<String>,
    #[serde(default, rename = "env")]
    pub environment: ResponseField<ApplicationEnvironmentShape>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub has_git_provider_access: Option<bool>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
    #[serde(default)]
    pub build_server_id: ResponseField<ServerId>,
    #[serde(default)]
    pub registry_id: ResponseField<RegistryId>,
    #[serde(default)]
    pub build_registry_id: ResponseField<RegistryId>,
    #[serde(default)]
    pub rollback_registry_id: ResponseField<RegistryId>,
    #[serde(default)]
    pub unauthorized_provider: Option<String>,
}

/// Value-free shape of the secret-bearing application `env` field.
///
/// The custom deserializer deliberately consumes strings without retaining
/// their bytes, so runtime environment values cannot enter snapshots, logs,
/// diagnostics, or debug output through the SDK model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplicationEnvironmentShape {
    /// Dokploy returned an empty environment document.
    Empty,
    /// Dokploy returned a non-empty environment document whose bytes are opaque.
    Opaque,
}

impl<'de> Deserialize<'de> for ApplicationEnvironmentShape {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ShapeVisitor;

        impl Visitor<'_> for ShapeVisitor {
            type Value = ApplicationEnvironmentShape;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an application environment string")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(if value.is_empty() {
                    ApplicationEnvironmentShape::Empty
                } else {
                    ApplicationEnvironmentShape::Opaque
                })
            }
        }

        deserializer.deserialize_str(ShapeVisitor)
    }
}

/// One safe application entry returned by `application.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSearchItem {
    pub application_id: ApplicationId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected application search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct ApplicationCollection {
    pub(crate) applications: Vec<ApplicationSearchItem>,
}

impl ApplicationCollection {
    /// Returns all applications discovered in the parent environment.
    #[must_use]
    pub fn applications(&self) -> &[ApplicationSearchItem] {
        &self.applications
    }
}

/// One page returned by the runtime `application.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationSearchPage {
    pub(crate) items: Vec<ApplicationSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `compose.one`.
///
/// The Compose document, environment document, refresh token, nested provider
/// records, and other unowned runtime fields are deliberately absent. Unknown
/// fields are ignored because the pinned success response schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComposeDetails {
    pub compose_id: ComposeId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub source_type: ResponseField<String>,
    #[serde(default)]
    pub compose_type: ResponseField<String>,
    #[serde(default)]
    pub auto_deploy: ResponseField<bool>,
    #[serde(default)]
    pub compose_path: ResponseField<String>,
    #[serde(default)]
    pub compose_status: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// One safe Compose entry returned by `compose.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ComposeSearchItem {
    pub compose_id: ComposeId,
    pub environment_id: EnvironmentId,
    pub name: String,
    #[serde(default)]
    pub app_name: ResponseField<String>,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub source_type: ResponseField<String>,
    #[serde(default)]
    pub compose_status: Option<String>,
}

/// The fully collected Compose search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct ComposeCollection {
    pub(crate) composes: Vec<ComposeSearchItem>,
}

impl ComposeCollection {
    /// Returns all Compose records discovered in the parent environment.
    #[must_use]
    pub fn composes(&self) -> &[ComposeSearchItem] {
        &self.composes
    }
}

/// One page returned by the runtime `compose.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ComposeSearchPage {
    pub(crate) items: Vec<ComposeSearchItem>,
    pub(crate) total: u64,
}

/// A safe complete Port record returned by Dokploy's Port operations.
///
/// Unknown response fields and nested application data are intentionally
/// ignored. Both port numbers are nonzero 16-bit integers at the SDK seam.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PortDetails {
    pub port_id: PortId,
    pub application_id: ApplicationId,
    pub published_port: NonZeroU16,
    pub target_port: NonZeroU16,
    pub publish_mode: PublishMode,
    pub protocol: PortProtocol,
}

impl PortDetails {
    pub(crate) fn is_valid(&self) -> bool {
        !self.port_id.as_str().is_empty() && !self.application_id.as_str().is_empty()
    }
}

/// The complete bounded Port collection for one exact application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortCollection {
    application_id: ApplicationId,
    ports: Vec<PortDetails>,
}

impl PortCollection {
    pub(crate) fn new(application_id: ApplicationId, ports: Vec<PortDetails>) -> Self {
        Self {
            application_id,
            ports,
        }
    }

    /// Returns the application whose Port collection was read.
    #[must_use]
    pub const fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    /// Returns every Port authoritatively reported by the application.
    #[must_use]
    pub fn ports(&self) -> &[PortDetails] {
        &self.ports
    }
}

/// Minimal parent response used to reconcile Ports through `application.one`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationPortCollectionResponse {
    pub(crate) application_id: ApplicationId,
    pub(crate) ports: Vec<PortDetails>,
}

/// A safe complete Redirect record returned by Dokploy's Redirect operations.
///
/// Nested application data and unknown response fields are ignored so
/// application environment values cannot enter the model.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedirectDetails {
    pub redirect_id: RedirectId,
    pub application_id: ApplicationId,
    pub regex: String,
    pub replacement: String,
    pub permanent: bool,
}

impl RedirectDetails {
    pub(crate) fn is_valid(&self) -> bool {
        !self.redirect_id.as_str().is_empty()
            && !self.application_id.as_str().is_empty()
            && !self.regex.is_empty()
            && !self.replacement.is_empty()
    }
}

/// The complete bounded Redirect collection for one exact application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedirectCollection {
    application_id: ApplicationId,
    redirects: Vec<RedirectDetails>,
}

impl RedirectCollection {
    pub(crate) fn new(application_id: ApplicationId, redirects: Vec<RedirectDetails>) -> Self {
        Self {
            application_id,
            redirects,
        }
    }

    /// Returns the application whose Redirect collection was read.
    #[must_use]
    pub const fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    /// Returns every Redirect authoritatively reported by the application.
    #[must_use]
    pub fn redirects(&self) -> &[RedirectDetails] {
        &self.redirects
    }
}

/// Minimal parent response used to reconcile Redirects through `application.one`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationRedirectCollectionResponse {
    pub(crate) application_id: ApplicationId,
    pub(crate) redirects: Vec<RedirectDetails>,
}

/// A safe basic-auth record returned by Dokploy's Security operations.
///
/// Plaintext password bytes are consumed during decoding and immediately
/// discarded. The model exposes only whether a non-empty password was present.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SecurityDetails {
    pub security_id: SecurityId,
    pub application_id: ApplicationId,
    pub username: String,
    #[serde(rename = "password", deserialize_with = "deserialize_secret_presence")]
    pub password_present: bool,
}

impl SecurityDetails {
    pub(crate) fn is_valid(&self) -> bool {
        !self.security_id.as_str().is_empty()
            && !self.application_id.as_str().is_empty()
            && !self.username.is_empty()
    }
}

/// The complete bounded Security collection for one exact application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SecurityCollection {
    application_id: ApplicationId,
    entries: Vec<SecurityDetails>,
}

impl SecurityCollection {
    pub(crate) fn new(application_id: ApplicationId, entries: Vec<SecurityDetails>) -> Self {
        Self {
            application_id,
            entries,
        }
    }

    /// Returns the application whose Security collection was read.
    #[must_use]
    pub const fn application_id(&self) -> &ApplicationId {
        &self.application_id
    }

    /// Returns every basic-auth entry authoritatively reported by the application.
    #[must_use]
    pub fn entries(&self) -> &[SecurityDetails] {
        &self.entries
    }
}

/// Minimal parent response used to reconcile Security through `application.one`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationSecurityCollectionResponse {
    pub(crate) application_id: ApplicationId,
    #[serde(rename = "security")]
    pub(crate) entries: Vec<SecurityDetails>,
}

pub(crate) struct SecurityProofDetails {
    details: SecurityDetails,
    password: Option<Zeroizing<String>>,
}

impl SecurityProofDetails {
    pub(crate) const fn details(&self) -> &SecurityDetails {
        &self.details
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SecurityProofResponse {
    security_id: SecurityId,
    application_id: ApplicationId,
    username: String,
    #[serde(deserialize_with = "deserialize_optional_zeroizing_string")]
    password: Option<Zeroizing<String>>,
}

impl<'de> Deserialize<'de> for SecurityProofDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let response = SecurityProofResponse::deserialize(deserializer)?;
        let password_present = response
            .password
            .as_ref()
            .is_some_and(|password| !password.is_empty());

        Ok(Self {
            details: SecurityDetails {
                security_id: response.security_id,
                application_id: response.application_id,
                username: response.username,
                password_present,
            },
            password: response.password,
        })
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ApplicationSecurityProofCollectionResponse {
    pub(crate) application_id: ApplicationId,
    #[serde(rename = "security")]
    pub(crate) entries: Vec<SecurityProofDetails>,
}

/// A safe Schedule returned by direct or target-scoped read operations.
///
/// Command and script bytes are consumed during decoding, zeroized when owned,
/// and represented only by presence flags.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleDetails {
    pub schedule_id: ScheduleId,
    pub target: ScheduleTarget,
    pub name: String,
    pub description: Option<String>,
    pub cron_expression: String,
    pub shell_type: ShellType,
    pub command_present: bool,
    pub script_present: bool,
    pub enabled: bool,
    pub timezone: Option<String>,
}

impl ScheduleDetails {
    pub(crate) fn is_valid(&self) -> bool {
        !self.schedule_id.as_str().is_empty()
            && self.target.is_valid()
            && !self.name.is_empty()
            && !self.cron_expression.is_empty()
            && self.command_present
            && self
                .timezone
                .as_ref()
                .is_none_or(|timezone| !timezone.is_empty())
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleResponse {
    schedule_id: ScheduleId,
    name: String,
    #[serde(default)]
    description: Option<String>,
    cron_expression: String,
    shell_type: ShellType,
    schedule_type: ScheduleTypeResponse,
    #[serde(default)]
    application_id: Option<ApplicationId>,
    #[serde(default)]
    compose_id: Option<ComposeId>,
    #[serde(default)]
    server_id: Option<ServerId>,
    #[serde(default)]
    service_name: Option<String>,
    #[serde(rename = "command", deserialize_with = "deserialize_secret_presence")]
    command_present: bool,
    #[serde(
        default,
        rename = "script",
        deserialize_with = "deserialize_secret_presence"
    )]
    script_present: bool,
    enabled: bool,
    #[serde(default)]
    timezone: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum ScheduleTypeResponse {
    Application,
    Compose,
    Server,
    DokployServer,
}

fn schedule_target_from_response(
    schedule_type: ScheduleTypeResponse,
    application_id: Option<ApplicationId>,
    compose_id: Option<ComposeId>,
    server_id: Option<ServerId>,
    service_name: Option<String>,
) -> Result<ScheduleTarget, &'static str> {
    match schedule_type {
        ScheduleTypeResponse::Application => {
            match (
                application_id,
                compose_id,
                server_id,
                service_name.as_deref(),
            ) {
                (Some(application_id), None, None, None | Some("")) => {
                    Ok(ScheduleTarget::Application(application_id))
                }
                _ => Err("invalid application Schedule target"),
            }
        }
        ScheduleTypeResponse::Compose => {
            match (application_id, compose_id, server_id, service_name) {
                (None, Some(compose_id), None, Some(service_name)) if !service_name.is_empty() => {
                    Ok(ScheduleTarget::Compose {
                        compose_id,
                        service_name,
                    })
                }
                _ => Err("invalid Compose Schedule target"),
            }
        }
        ScheduleTypeResponse::Server | ScheduleTypeResponse::DokployServer => {
            Err("unsupported privileged Schedule target")
        }
    }
}

impl<'de> Deserialize<'de> for ScheduleDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let response = ScheduleResponse::deserialize(deserializer)?;
        let target = schedule_target_from_response(
            response.schedule_type,
            response.application_id,
            response.compose_id,
            response.server_id,
            response.service_name,
        )
        .map_err(de::Error::custom)?;

        Ok(Self {
            schedule_id: response.schedule_id,
            target,
            name: response.name,
            description: response.description,
            cron_expression: response.cron_expression,
            shell_type: response.shell_type,
            command_present: response.command_present,
            script_present: response.script_present,
            enabled: response.enabled,
            timezone: response.timezone,
        })
    }
}

pub(crate) struct ScheduleProofDetails {
    details: ScheduleDetails,
    command: Zeroizing<String>,
    script: Option<Zeroizing<String>>,
}

impl ScheduleProofDetails {
    pub(crate) const fn details(&self) -> &ScheduleDetails {
        &self.details
    }

    pub(crate) fn into_details(self) -> ScheduleDetails {
        self.details
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ScheduleProofResponse {
    schedule_id: ScheduleId,
    name: String,
    #[serde(default)]
    description: Option<String>,
    cron_expression: String,
    shell_type: ShellType,
    schedule_type: ScheduleTypeResponse,
    #[serde(default)]
    application_id: Option<ApplicationId>,
    #[serde(default)]
    compose_id: Option<ComposeId>,
    #[serde(default)]
    server_id: Option<ServerId>,
    #[serde(default)]
    service_name: Option<String>,
    #[serde(deserialize_with = "deserialize_zeroizing_string")]
    command: Zeroizing<String>,
    #[serde(default, deserialize_with = "deserialize_optional_zeroizing_string")]
    script: Option<Zeroizing<String>>,
    enabled: bool,
    #[serde(default)]
    timezone: Option<String>,
}

impl<'de> Deserialize<'de> for ScheduleProofDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let response = ScheduleProofResponse::deserialize(deserializer)?;
        let target = schedule_target_from_response(
            response.schedule_type,
            response.application_id,
            response.compose_id,
            response.server_id,
            response.service_name,
        )
        .map_err(de::Error::custom)?;
        let command_present = !response.command.is_empty();
        let script_present = response
            .script
            .as_ref()
            .is_some_and(|script| !script.is_empty());

        Ok(Self {
            details: ScheduleDetails {
                schedule_id: response.schedule_id,
                target,
                name: response.name,
                description: response.description,
                cron_expression: response.cron_expression,
                shell_type: response.shell_type,
                command_present,
                script_present,
                enabled: response.enabled,
                timezone: response.timezone,
            },
            command: response.command,
            script: response.script,
        })
    }
}

fn deserialize_zeroizing_string<'de, D>(deserializer: D) -> Result<Zeroizing<String>, D::Error>
where
    D: Deserializer<'de>,
{
    String::deserialize(deserializer).map(Zeroizing::new)
}

fn deserialize_optional_zeroizing_string<'de, D>(
    deserializer: D,
) -> Result<Option<Zeroizing<String>>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<String>::deserialize(deserializer).map(|value| value.map(Zeroizing::new))
}

/// The complete bounded Schedule collection for one exact supported target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduleCollection {
    target: ScheduleTarget,
    schedules: Vec<ScheduleDetails>,
}

impl ScheduleCollection {
    pub(crate) fn new(target: ScheduleTarget, schedules: Vec<ScheduleDetails>) -> Self {
        Self { target, schedules }
    }

    /// Returns the exact target whose Schedule collection was read.
    #[must_use]
    pub const fn target(&self) -> &ScheduleTarget {
        &self.target
    }

    /// Returns every Schedule authoritatively reported for the target.
    #[must_use]
    pub fn schedules(&self) -> &[ScheduleDetails] {
        &self.schedules
    }
}

/// A safe database Backup returned by direct or target-scoped reads.
///
/// Nested database records, destination records, deployment history, user
/// relations, and metadata are deliberately excluded. Those upstream
/// relations can contain database credentials, destination keys, executable
/// errors, or Compose metadata secrets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupDetails {
    pub backup_id: BackupId,
    pub target: BackupTarget,
    pub destination_id: DestinationId,
    pub schedule: String,
    pub enabled: Option<bool>,
    pub prefix: String,
    pub database: String,
    pub keep_latest_count: Option<NonZeroU32>,
    pub include_encryption_key: bool,
}

impl BackupDetails {
    pub(crate) fn is_valid(&self) -> bool {
        !self.backup_id.as_str().is_empty()
            && self.target.is_valid()
            && !self.destination_id.as_str().is_empty()
            && !self.schedule.is_empty()
            && !self.prefix.is_empty()
            && !self.database.is_empty()
    }

    pub(crate) fn collision_key(&self) -> BackupCollisionKey {
        BackupCollisionKey::new(
            self.target.clone(),
            self.destination_id.clone(),
            &self.prefix,
            self.database.clone(),
        )
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(crate) struct BackupCollisionKey {
    target: BackupTarget,
    destination_id: DestinationId,
    normalized_prefix: String,
    database: String,
}

impl BackupCollisionKey {
    fn new(
        target: BackupTarget,
        destination_id: DestinationId,
        prefix: &str,
        database: String,
    ) -> Self {
        Self {
            target,
            destination_id,
            normalized_prefix: normalize_backup_prefix(prefix),
            database,
        }
    }
}

fn normalize_backup_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim().trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupResponse {
    backup_id: BackupId,
    schedule: String,
    #[serde(default)]
    enabled: ResponseField<bool>,
    database: String,
    prefix: String,
    destination_id: DestinationId,
    #[serde(default)]
    keep_latest_count: ResponseField<u32>,
    include_encryption_key: bool,
    backup_type: BackupTypeResponse,
    database_type: BackupDatabaseTypeResponse,
    #[serde(default)]
    compose_id: ResponseField<ComposeId>,
    #[serde(default)]
    postgres_id: ResponseField<PostgresId>,
    #[serde(default)]
    mariadb_id: ResponseField<MariaDbId>,
    #[serde(default)]
    mysql_id: ResponseField<MySqlId>,
    #[serde(default)]
    mongo_id: ResponseField<MongoId>,
    #[serde(default)]
    libsql_id: ResponseField<LibSqlId>,
    #[serde(default)]
    service_name: ResponseField<String>,
    #[serde(default)]
    metadata: ResponseField<de::IgnoredAny>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BackupTypeResponse {
    Database,
    Compose,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum BackupDatabaseTypeResponse {
    Postgres,
    Mariadb,
    Mysql,
    Mongo,
    WebServer,
    Libsql,
}

impl<'de> Deserialize<'de> for BackupDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let response = BackupResponse::deserialize(deserializer)?;
        if !matches!(response.backup_type, BackupTypeResponse::Database)
            || !matches!(response.compose_id, ResponseField::Null)
            || !matches!(response.service_name, ResponseField::Null)
            || !matches!(response.metadata, ResponseField::Null)
        {
            return Err(de::Error::custom("unsupported Backup target or metadata"));
        }

        let target = match (
            response.database_type,
            response.postgres_id,
            response.mysql_id,
            response.mariadb_id,
            response.mongo_id,
            response.libsql_id,
        ) {
            (
                BackupDatabaseTypeResponse::Postgres,
                ResponseField::Value(id),
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
            ) => BackupTarget::Postgres(id),
            (
                BackupDatabaseTypeResponse::Mysql,
                ResponseField::Null,
                ResponseField::Value(id),
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
            ) => BackupTarget::MySql(id),
            (
                BackupDatabaseTypeResponse::Mariadb,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Value(id),
                ResponseField::Null,
                ResponseField::Null,
            ) => BackupTarget::MariaDb(id),
            (
                BackupDatabaseTypeResponse::Mongo,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Value(id),
                ResponseField::Null,
            ) => BackupTarget::Mongo(id),
            (
                BackupDatabaseTypeResponse::Libsql,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Null,
                ResponseField::Value(id),
            ) => BackupTarget::LibSql(id),
            (BackupDatabaseTypeResponse::WebServer, ..) => {
                return Err(de::Error::custom("invalid or unsupported Backup target"));
            }
            _ => {
                return Err(de::Error::custom("invalid or unsupported Backup target"));
            }
        };
        let enabled = match response.enabled {
            ResponseField::Null => None,
            ResponseField::Value(enabled) => Some(enabled),
            ResponseField::NotReturned => {
                return Err(de::Error::custom("Backup enabled state was not returned"));
            }
        };
        let keep_latest_count = match response.keep_latest_count {
            ResponseField::Null => None,
            ResponseField::Value(count) => Some(
                NonZeroU32::new(count)
                    .ok_or_else(|| de::Error::custom("Backup retention count cannot be zero"))?,
            ),
            ResponseField::NotReturned => {
                return Err(de::Error::custom("Backup retention count was not returned"));
            }
        };

        Ok(Self {
            backup_id: response.backup_id,
            target,
            destination_id: response.destination_id,
            schedule: response.schedule,
            enabled,
            prefix: response.prefix,
            database: response.database,
            keep_latest_count,
            include_encryption_key: response.include_encryption_key,
        })
    }
}

/// The complete bounded Backup collection for one exact database target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackupCollection {
    target: BackupTarget,
    backups: Vec<BackupDetails>,
}

impl BackupCollection {
    pub(crate) fn new(target: BackupTarget, backups: Vec<BackupDetails>) -> Self {
        Self { target, backups }
    }

    /// Returns the exact database target whose Backup collection was read.
    #[must_use]
    pub const fn target(&self) -> &BackupTarget {
        &self.target
    }

    /// Returns every Backup authoritatively reported by the target.
    #[must_use]
    pub fn backups(&self) -> &[BackupDetails] {
        &self.backups
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostgresBackupCollectionResponse {
    pub(crate) postgres_id: PostgresId,
    pub(crate) backups: Vec<BackupDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MySqlBackupCollectionResponse {
    pub(crate) mysql_id: MySqlId,
    pub(crate) backups: Vec<BackupDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MariaDbBackupCollectionResponse {
    pub(crate) mariadb_id: MariaDbId,
    pub(crate) backups: Vec<BackupDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MongoBackupCollectionResponse {
    pub(crate) mongo_id: MongoId,
    pub(crate) backups: Vec<BackupDetails>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LibSqlBackupCollectionResponse {
    pub(crate) libsql_id: LibSqlId,
    pub(crate) backups: Vec<BackupDetails>,
}

fn deserialize_secret_presence<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    struct SecretPresenceVisitor;

    impl Visitor<'_> for SecretPresenceVisitor {
        type Value = bool;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a plaintext secret string or null")
        }

        fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(!value.is_empty())
        }

        fn visit_string<E>(self, mut value: String) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            let present = !value.is_empty();
            value.zeroize();

            Ok(present)
        }

        fn visit_none<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(false)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E>
        where
            E: de::Error,
        {
            Ok(false)
        }
    }

    deserializer.deserialize_any(SecretPresenceVisitor)
}

/// A safe subset of the response returned by Mount read operations.
///
/// Opaque file content and nested target records are deliberately absent.
/// Unknown fields are ignored because Dokploy's pinned success schemas are
/// empty even though the runtime returns complete Mount records.
#[derive(Clone, Debug, PartialEq)]
pub struct MountDetails {
    pub mount_id: MountId,
    pub mount_type: MountType,
    pub mount_path: String,
    pub target: ServiceTarget,
    pub host_path: ResponseField<String>,
    pub volume_name: ResponseField<String>,
    pub file_path: ResponseField<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MountResponse {
    mount_id: MountId,
    #[serde(rename = "type")]
    mount_type: MountType,
    mount_path: String,
    service_type: String,
    #[serde(default)]
    host_path: ResponseField<String>,
    #[serde(default)]
    volume_name: ResponseField<String>,
    #[serde(default)]
    file_path: ResponseField<String>,
    #[serde(default)]
    application_id: ResponseField<ApplicationId>,
    #[serde(default)]
    compose_id: ResponseField<ComposeId>,
    #[serde(default)]
    libsql_id: ResponseField<LibSqlId>,
    #[serde(default)]
    mariadb_id: ResponseField<MariaDbId>,
    #[serde(default)]
    mongo_id: ResponseField<MongoId>,
    #[serde(default)]
    mysql_id: ResponseField<MySqlId>,
    #[serde(default)]
    postgres_id: ResponseField<PostgresId>,
    #[serde(default)]
    redis_id: ResponseField<RedisId>,
}

impl<'de> Deserialize<'de> for MountDetails {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let response = MountResponse::deserialize(deserializer)?;
        let mut targets = Vec::new();
        if let ResponseField::Value(id) = response.application_id {
            targets.push(ServiceTarget::Application(id));
        }
        if let ResponseField::Value(id) = response.compose_id {
            targets.push(ServiceTarget::Compose(id));
        }
        if let ResponseField::Value(id) = response.libsql_id {
            targets.push(ServiceTarget::LibSql(id));
        }
        if let ResponseField::Value(id) = response.mariadb_id {
            targets.push(ServiceTarget::MariaDb(id));
        }
        if let ResponseField::Value(id) = response.mongo_id {
            targets.push(ServiceTarget::Mongo(id));
        }
        if let ResponseField::Value(id) = response.mysql_id {
            targets.push(ServiceTarget::MySql(id));
        }
        if let ResponseField::Value(id) = response.postgres_id {
            targets.push(ServiceTarget::Postgres(id));
        }
        if let ResponseField::Value(id) = response.redis_id {
            targets.push(ServiceTarget::Redis(id));
        }
        if targets.len() != 1 {
            return Err(de::Error::custom(
                "a Mount response must contain exactly one target identity",
            ));
        }
        let target = targets.pop().expect("one Mount target was validated");
        if target.service_id().is_empty() || target.service_type() != response.service_type {
            return Err(de::Error::custom(
                "a Mount response target must match its service type",
            ));
        }

        Ok(Self {
            mount_id: response.mount_id,
            mount_type: response.mount_type,
            mount_path: response.mount_path,
            target,
            host_path: response.host_path,
            volume_name: response.volume_name,
            file_path: response.file_path,
        })
    }
}

/// The complete, bounded Mount collection for one exact target.
#[derive(Clone, Debug, PartialEq)]
pub struct MountCollection {
    pub(crate) mounts: Vec<MountDetails>,
}

impl MountCollection {
    /// Returns every Mount discovered on the requested service target.
    #[must_use]
    pub fn mounts(&self) -> &[MountDetails] {
        &self.mounts
    }
}

/// A safe subset of the response returned by `postgres.one`.
///
/// Credentials, environment variables, and other secret-bearing runtime
/// fields are deliberately absent, so they cannot enter plans, state, logs,
/// diagnostics, or snapshots through this model. Unknown response fields are
/// intentionally ignored because Dokploy's OpenAPI response schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresDetails {
    pub postgres_id: PostgresId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// A safe subset of the response returned by `libsql.one`.
///
/// The database password/authentication token, environment document, mounts,
/// and other secret-bearing runtime fields are deliberately absent. Unknown
/// fields are ignored because the pinned success response schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibSqlDetails {
    pub libsql_id: LibSqlId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub sqld_node: ResponseField<String>,
    #[serde(default)]
    pub sqld_primary_url: ResponseField<String>,
    #[serde(default)]
    pub enable_namespaces: ResponseField<bool>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub external_grpcport: Option<u16>,
    #[serde(default)]
    pub external_admin_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// One safe LibSQL entry embedded in an environment from `project.one`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibSqlSearchItem {
    pub libsql_id: LibSqlId,
    pub name: String,
    pub app_name: String,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// Sparse LibSQL identity embedded by `project.all`.
///
/// Dokploy `v0.30.6` returns only `libsqlId` at this boundary. Rich names and
/// application metadata remain exclusive to `project.one` and `libsql.one`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LibSqlTopologySummary {
    pub libsql_id: LibSqlId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub app_name: Option<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

impl LibSqlTopologySummary {
    pub(crate) fn into_search_item(self) -> Option<LibSqlSearchItem> {
        let name = self.name.filter(|name| !name.is_empty())?;
        let app_name = self.app_name.filter(|app_name| !app_name.is_empty())?;

        Some(LibSqlSearchItem {
            libsql_id: self.libsql_id,
            name,
            app_name,
            application_status: self.application_status,
            description: self.description,
            server_id: self.server_id,
        })
    }
}

/// The authoritative LibSQL collection nested under one exact environment.
#[derive(Clone, Debug, PartialEq)]
pub struct LibSqlCollection {
    pub(crate) libsql: Vec<LibSqlSearchItem>,
}

impl LibSqlCollection {
    /// Returns all LibSQL databases discovered in the parent environment.
    #[must_use]
    pub fn libsql(&self) -> &[LibSqlSearchItem] {
        &self.libsql
    }
}

/// A safe subset of the response returned by `mariadb.one`.
///
/// User and root passwords, environment variables, and other secret-bearing
/// runtime fields are deliberately absent. Unknown response fields are ignored
/// because Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MariaDbDetails {
    pub mariadb_id: MariaDbId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// A safe subset of the response returned by `mongo.one`.
///
/// Passwords, environment variables, mounts, and other secret-bearing runtime
/// fields are deliberately absent. Unknown response fields are ignored because
/// Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MongoDetails {
    pub mongo_id: MongoId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub replica_sets: ResponseField<bool>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// One safe MongoDB entry returned by `mongo.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MongoSearchItem {
    pub mongo_id: MongoId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MongoDB search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MongoCollection {
    pub(crate) mongo: Vec<MongoSearchItem>,
}

impl MongoCollection {
    /// Returns all MongoDB databases discovered in the parent environment.
    #[must_use]
    pub fn mongo(&self) -> &[MongoSearchItem] {
        &self.mongo
    }
}

/// One page returned by the runtime `mongo.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MongoSearchPage {
    pub(crate) items: Vec<MongoSearchItem>,
    pub(crate) total: u64,
}

/// One safe MariaDB entry returned by `mariadb.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MariaDbSearchItem {
    pub mariadb_id: MariaDbId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MariaDB search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MariaDbCollection {
    pub(crate) mariadb: Vec<MariaDbSearchItem>,
}

impl MariaDbCollection {
    /// Returns all MariaDB databases discovered in the parent environment.
    #[must_use]
    pub fn mariadb(&self) -> &[MariaDbSearchItem] {
        &self.mariadb
    }
}

/// One page returned by the runtime `mariadb.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MariaDbSearchPage {
    pub(crate) items: Vec<MariaDbSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `mysql.one`.
///
/// User and root passwords, environment variables, mounts, backups, and other
/// secret-bearing runtime fields are deliberately absent. Unknown response
/// fields are ignored because Dokploy's OpenAPI success schema is empty.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MySqlDetails {
    pub mysql_id: MySqlId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub database_name: ResponseField<String>,
    #[serde(default)]
    pub database_user: ResponseField<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// One safe MySQL entry returned by `mysql.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct MySqlSearchItem {
    pub mysql_id: MySqlId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected MySQL search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct MySqlCollection {
    pub(crate) mysql: Vec<MySqlSearchItem>,
}

impl MySqlCollection {
    /// Returns all MySQL databases discovered in the parent environment.
    #[must_use]
    pub fn mysql(&self) -> &[MySqlSearchItem] {
        &self.mysql
    }
}

/// One page returned by the runtime `mysql.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct MySqlSearchPage {
    pub(crate) items: Vec<MySqlSearchItem>,
    pub(crate) total: u64,
}

/// One safe Postgres entry returned by `postgres.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresSearchItem {
    pub postgres_id: PostgresId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected Postgres search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct PostgresCollection {
    pub(crate) postgres: Vec<PostgresSearchItem>,
}

impl PostgresCollection {
    /// Returns all Postgres databases discovered in the parent environment.
    #[must_use]
    pub fn postgres(&self) -> &[PostgresSearchItem] {
        &self.postgres
    }
}

/// One page returned by the runtime `postgres.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PostgresSearchPage {
    pub(crate) items: Vec<PostgresSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `redis.one`.
///
/// Passwords, environment variables, and nested runtime data are deliberately
/// absent so secret bytes cannot cross the SDK read seam.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisDetails {
    pub redis_id: RedisId,
    pub environment_id: EnvironmentId,
    pub name: String,
    pub app_name: String,
    pub docker_image: String,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub external_port: Option<u16>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// One safe Redis entry returned by `redis.search`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisSearchItem {
    pub redis_id: RedisId,
    pub environment_id: EnvironmentId,
    pub name: String,
}

/// The fully collected Redis search result for one environment.
#[derive(Clone, Debug, PartialEq)]
pub struct RedisCollection {
    pub(crate) redis: Vec<RedisSearchItem>,
}

impl RedisCollection {
    /// Returns all Redis databases discovered in the parent environment.
    #[must_use]
    pub fn redis(&self) -> &[RedisSearchItem] {
        &self.redis
    }
}

/// One page returned by the runtime `redis.search` operation.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RedisSearchPage {
    pub(crate) items: Vec<RedisSearchItem>,
    pub(crate) total: u64,
}

/// A safe subset of the response returned by `domain.one`.
///
/// Routing middleware and application runtime details are deliberately absent;
/// reconciliation owns only the domain identity, host, and application link.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DomainDetails {
    pub domain_id: DomainId,
    pub host: String,
    #[serde(default)]
    pub application_id: Option<ApplicationId>,
}

/// The complete domain collection returned by `domain.byApplicationId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct DomainCollection {
    domains: Vec<DomainDetails>,
}

impl DomainCollection {
    /// Returns all domains discovered for the selected application.
    #[must_use]
    pub fn domains(&self) -> &[DomainDetails] {
        &self.domains
    }
}

/// The project collection returned by `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct ProjectTopology {
    projects: Vec<ProjectDetails>,
}

impl ProjectTopology {
    #[must_use]
    pub fn projects(&self) -> &[ProjectDetails] {
        &self.projects
    }
}

/// A project and the environment topology needed for resource discovery.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDetails {
    pub project_id: ProjectId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
    #[serde(default)]
    pub environments: Vec<EnvironmentTopology>,
}

/// An environment and the initial MVP resource summaries nested below it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTopology {
    pub environment_id: EnvironmentId,
    pub name: String,
    pub is_default: bool,
    #[serde(default)]
    pub applications: Vec<ApplicationSummary>,
    #[serde(default)]
    pub postgres: Vec<PostgresSummary>,
    #[serde(default)]
    pub redis: Vec<RedisSummary>,
    #[serde(default)]
    pub libsql: Vec<LibSqlTopologySummary>,
}

/// A safe subset of the response returned by `environment.one`.
///
/// Environment variables and nested resources are deliberately omitted so
/// secret-bearing runtime fields cannot cross into discovery snapshots.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentDetails {
    pub environment_id: EnvironmentId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
    pub project_id: ProjectId,
}

/// One safe environment entry returned by `environment.byProjectId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentSummary {
    pub environment_id: EnvironmentId,
    pub name: String,
    #[serde(default)]
    pub description: ResponseField<String>,
}

/// The parent-scoped environment collection returned by `environment.byProjectId`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(transparent)]
pub struct EnvironmentCollection {
    environments: Vec<EnvironmentSummary>,
}

impl EnvironmentCollection {
    #[must_use]
    pub fn environments(&self) -> &[EnvironmentSummary] {
        &self.environments
    }
}

/// The role-dependent application projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationSummary {
    pub application_id: ApplicationId,
    pub name: String,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// The role-dependent Postgres projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct PostgresSummary {
    pub postgres_id: PostgresId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

/// The role-dependent Redis projection embedded in `project.all`.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RedisSummary {
    pub redis_id: RedisId,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub application_status: Option<String>,
    #[serde(default)]
    pub server_id: ResponseField<ServerId>,
}

#[cfg(test)]
mod tests {
    use super::{
        ApplicationDetails, ApplicationEnvironmentShape, PostgresDetails, ProjectTopology,
        ResponseField,
    };

    #[test]
    fn application_details_tolerate_unknown_runtime_fields() {
        let application: ApplicationDetails = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/application-one.owner.json"
        ))
        .expect("fixture must deserialize");

        assert_eq!(application.application_id.as_str(), "application-1");
        assert_eq!(application.environment_id.as_str(), "environment-1");
        assert_eq!(application.server_id, ResponseField::Null);
        assert_eq!(application.build_server_id, ResponseField::Null);
        assert_eq!(application.registry_id, ResponseField::Null);
        assert_eq!(application.build_registry_id, ResponseField::Null);
        assert_eq!(application.rollback_registry_id, ResponseField::Null);
        assert_eq!(application.description, ResponseField::Null);
        assert_eq!(application.replicas, ResponseField::Value(1));
        assert_eq!(application.build_path, ResponseField::Value("/".to_owned()));
        assert_eq!(application.environment, ResponseField::Null);
    }

    #[test]
    fn application_environment_shape_never_retains_runtime_bytes() {
        let application: ApplicationDetails = serde_json::from_str(
            r#"{
                "applicationId":"application-1",
                "environmentId":"environment-1",
                "name":"API",
                "appName":"api",
                "env":"SECRET=environment-value-canary"
            }"#,
        )
        .expect("application response is valid");

        assert_eq!(
            application.environment,
            ResponseField::Value(ApplicationEnvironmentShape::Opaque)
        );
        assert!(!format!("{application:?}").contains("environment-value-canary"));
    }

    #[test]
    fn application_fields_preserve_omitted_null_and_value() {
        let application: ApplicationDetails = serde_json::from_str(
            r#"{
                "applicationId":"application-1",
                "environmentId":"environment-1",
                "name":"API",
                "appName":"api",
                "description":null,
                "replicas":2,
                "repository":"owner/repository",
                "branch":"main",
                "sourceType":"github",
                "buildPath":"apps/api",
                "env":""
            }"#,
        )
        .expect("application response is valid");

        assert_eq!(application.description, ResponseField::Null);
        assert_eq!(application.replicas, ResponseField::Value(2));
        assert_eq!(
            application.repository,
            ResponseField::Value("owner/repository".to_owned())
        );
        assert_eq!(application.branch, ResponseField::Value("main".to_owned()));
        assert_eq!(
            application.source_type,
            ResponseField::Value("github".to_owned())
        );
        assert_eq!(
            application.environment,
            ResponseField::Value(ApplicationEnvironmentShape::Empty)
        );
    }

    #[test]
    fn project_topology_accepts_role_dependent_service_summaries() {
        let topology: ProjectTopology = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/project-all.populated.owner.json"
        ))
        .expect("fixture must deserialize");

        let project = &topology.projects()[0];
        let environment = &project.environments[0];

        assert_eq!(project.project_id.as_str(), "project-1");
        assert_eq!(
            project.description,
            ResponseField::Value("Disposable integration fixture".to_owned())
        );
        assert_eq!(environment.applications[0].name, "API");
        assert_eq!(environment.postgres[0].name, None);
    }

    #[test]
    fn project_description_preserves_omitted_null_and_string_responses() {
        let cases = [
            (
                r#"[{"projectId":"project-1","name":"omitted"}]"#,
                ResponseField::NotReturned,
            ),
            (
                r#"[{"projectId":"project-1","name":"null","description":null}]"#,
                ResponseField::Null,
            ),
            (
                r#"[{"projectId":"project-1","name":"value","description":"known"}]"#,
                ResponseField::Value("known".to_owned()),
            ),
        ];

        for (json, expected) in cases {
            let topology: ProjectTopology =
                serde_json::from_str(json).expect("project topology is valid");

            assert_eq!(topology.projects()[0].description, expected);
        }
    }

    #[test]
    fn postgres_details_ignore_secret_bearing_runtime_fields() {
        let mut fixture: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/api/live/v0.30.6/postgres-one.owner.json"
        ))
        .expect("fixture must be valid JSON");
        fixture["databasePassword"] = "password-canary".into();
        fixture["env"] = "environment-secret-canary".into();
        fixture["buildSecrets"] = "build-secret-canary".into();

        let postgres: PostgresDetails =
            serde_json::from_value(fixture).expect("fixture must deserialize");
        let debug = format!("{postgres:?}");

        assert_eq!(postgres.postgres_id.as_str(), "postgres-1");
        assert_eq!(postgres.environment_id.as_str(), "environment-1");
        assert!(!debug.contains("password-canary"));
        assert!(!debug.contains("environment-secret-canary"));
        assert!(!debug.contains("build-secret-canary"));
    }

    #[test]
    fn postgres_owned_fields_preserve_omitted_null_and_value() {
        let cases = [
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"omitted","appName":"omitted","dockerImage":"postgres:16"}"#,
                ResponseField::NotReturned,
                ResponseField::NotReturned,
            ),
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"null","appName":"null","dockerImage":"postgres:16","databaseName":null,"databaseUser":null}"#,
                ResponseField::Null,
                ResponseField::Null,
            ),
            (
                r#"{"postgresId":"postgres-1","environmentId":"environment-1","name":"value","appName":"value","dockerImage":"postgres:16","databaseName":"app","databaseUser":"owner"}"#,
                ResponseField::Value("app".to_owned()),
                ResponseField::Value("owner".to_owned()),
            ),
        ];

        for (json, expected_database, expected_user) in cases {
            let postgres: PostgresDetails =
                serde_json::from_str(json).expect("Postgres details are valid");

            assert_eq!(postgres.database_name, expected_database);
            assert_eq!(postgres.database_user, expected_user);
        }
    }
}
