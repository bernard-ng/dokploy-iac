//! Compiles validated configuration into planner input and execution bindings.
//!
//! This module is the composition seam between configuration syntax and the
//! pure planning domain. The offline seam never performs I/O. The
//! instance-bound seam resolves configured sensitive sources exactly once,
//! derives opaque planner receipts, and retains bytes only in a redacted
//! one-shot execution sidecar. Neither seam resolves remote IDs.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
};

use dokploy_config::{
    ConfigValue, DokployConfig, ExternalSelector, Field, LibSqlNodeConfig, MountSourceConfig,
    ResourceConfig, SelectorKind, SourceConfig,
};
use dokploy_core::{
    ConfigDigest, DesiredResource, DesiredState, DesiredStateError, ExternalResolution,
    MoveDirective, OwnedValue, PropertyPath, ProtectionIntent, RemoteState, RemovalDirective,
    SensitiveIntent,
};
use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress, SensitiveFingerprint, StateFile};
use thiserror::Error;
use zeroize::Zeroizing;

mod sensitive_compilation;

/// Planner input paired with deferred values needed by a future executor.
pub struct CompiledDesired {
    desired_state: DesiredState,
    bindings: ExecutionBindings,
}

impl CompiledDesired {
    /// Returns the pure planner input.
    #[must_use]
    pub const fn desired_state(&self) -> &DesiredState {
        &self.desired_state
    }

    /// Returns deferred execution metadata without resolving it.
    #[must_use]
    pub const fn bindings(&self) -> &ExecutionBindings {
        &self.bindings
    }

    /// Removes one resolved sensitive value from the execution sidecar.
    ///
    /// A value can be taken at most once and never appears in debug or
    /// serialized planner data.
    pub fn take_sensitive(
        &mut self,
        address: &ResourceAddress,
        path: &PropertyPath,
    ) -> Option<SensitiveExecutionValue> {
        self.bindings
            .sensitive
            .remove(&(address.clone(), path.clone()))
    }

    /// Copies freshly resolved external identities into the execution sidecar.
    ///
    /// Only selectors that resolved uniquely are retained, so execution fails
    /// closed for anything the plan could not have been applyable with. The
    /// identities are non-serializable and never appear in debug output.
    pub fn bind_external_resolutions(&mut self, remote: &RemoteState) {
        let mut resolved = BTreeMap::new();
        for (address, path) in self.bindings.selectors.keys() {
            match remote.external_resolution(address, path) {
                Some(ExternalResolution::Local) => {
                    resolved.insert((address.clone(), path.clone()), ExternalExecutionId::Local);
                }
                Some(ExternalResolution::Resolved(remote_id)) => {
                    resolved.insert(
                        (address.clone(), path.clone()),
                        ExternalExecutionId::Remote(remote_id.clone()),
                    );
                }
                Some(
                    ExternalResolution::Unmatched
                    | ExternalResolution::Ambiguous
                    | ExternalResolution::Unavailable(_),
                )
                | None => {}
            }
        }
        self.bindings.external_ids = resolved;
    }

    /// Builds the synthetic empty desired state used by workspace destruction.
    pub(crate) fn destroy_all(
        state: &StateFile,
        digest: ConfigDigest,
    ) -> Result<Self, CompileDesiredError> {
        let removals = state
            .resources()
            .keys()
            .cloned()
            .map(|address| RemovalDirective::new(address, true))
            .collect();
        let desired_state = DesiredState::try_new(digest, BTreeMap::new())
            .map_err(CompileDesiredError::InvalidDesiredState)?
            .with_removals(removals);

        Ok(Self {
            desired_state,
            bindings: ExecutionBindings::default(),
        })
    }
}

impl fmt::Debug for CompiledDesired {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CompiledDesired")
            .field("resource_count", &self.desired_state.resources().len())
            .field("bindings", &self.bindings)
            .finish()
    }
}

/// Deferred execution metadata. Its contents are deliberately non-serializable.
#[derive(Default)]
pub struct ExecutionBindings {
    parents: BTreeMap<ResourceAddress, ResourceAddress>,
    domain_applications: BTreeMap<ResourceAddress, ResourceAddress>,
    domain_hosts: BTreeMap<ResourceAddress, String>,
    ports: BTreeMap<ResourceAddress, PortBinding>,
    redirect_regexes: BTreeMap<ResourceAddress, String>,
    security_usernames: BTreeMap<ResourceAddress, String>,
    mounts: BTreeMap<ResourceAddress, MountBinding>,
    selectors: BTreeMap<(ResourceAddress, PropertyPath), SelectorBinding>,
    external_ids: BTreeMap<(ResourceAddress, PropertyPath), ExternalExecutionId>,
    sensitive: BTreeMap<(ResourceAddress, PropertyPath), SensitiveExecutionValue>,
}

/// One configured external selector and the kind of record it selects.
#[derive(Clone)]
struct SelectorBinding {
    kind: SelectorKind,
    selector: ExternalSelector,
}

/// A freshly resolved external identity retained only for execution.
///
/// The value is non-serializable and redacted from debug output.
#[derive(Clone)]
pub enum ExternalExecutionId {
    /// The explicit local server, which is sent to Dokploy as JSON null.
    Local,
    /// One resolved external record.
    Remote(RemoteId),
}

impl fmt::Debug for ExternalExecutionId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local => formatter.write_str("ExternalExecutionId::Local"),
            Self::Remote(_) => formatter.write_str("ExternalExecutionId::Remote([REDACTED])"),
        }
    }
}

#[derive(Clone, Copy)]
struct PortBinding {
    published_port: u16,
    target_port: u16,
    publish_mode: &'static str,
    protocol: &'static str,
}

struct MountBinding {
    target: ResourceAddress,
    mount_type: &'static str,
    mount_path: String,
}

impl ExecutionBindings {
    /// Returns the direct logical containment parent, if this is a nested resource.
    #[must_use]
    pub fn parent_of(&self, address: &ResourceAddress) -> Option<&ResourceAddress> {
        self.parents.get(address)
    }

    /// Returns the logical application selected by a domain.
    #[must_use]
    pub fn domain_application(&self, address: &ResourceAddress) -> Option<&ResourceAddress> {
        self.domain_applications.get(address)
    }

    /// Returns the exact configured host used for domain identity discovery.
    #[must_use]
    pub fn domain_host(&self, address: &ResourceAddress) -> Option<&str> {
        self.domain_hosts.get(address).map(String::as_str)
    }

    /// Returns the configured Redirect regular expression, the application-scoped collision key.
    #[must_use]
    pub fn redirect_regex(&self, address: &ResourceAddress) -> Option<&str> {
        self.redirect_regexes.get(address).map(String::as_str)
    }

    /// Returns the configured Security username, the application-scoped collision key.
    #[must_use]
    pub fn security_username(&self, address: &ResourceAddress) -> Option<&str> {
        self.security_usernames.get(address).map(String::as_str)
    }

    /// Returns every configured external selector as `(address, property, kind, selector)`.
    pub fn external_selectors(
        &self,
    ) -> impl Iterator<
        Item = (
            &ResourceAddress,
            &PropertyPath,
            SelectorKind,
            &ExternalSelector,
        ),
    > {
        self.selectors
            .iter()
            .map(|((address, path), binding)| (address, path, binding.kind, &binding.selector))
    }

    /// Returns the freshly resolved external identity for one selector property.
    ///
    /// The result is available only after [`CompiledDesired::bind_external_resolutions`]
    /// and only for selectors that resolved to exactly one usable identity.
    #[must_use]
    pub fn external_id(
        &self,
        address: &ResourceAddress,
        path: &PropertyPath,
    ) -> Option<&ExternalExecutionId> {
        self.external_ids.get(&(address.clone(), path.clone()))
    }

    /// Returns the complete non-secret Port input for collision and execution checks.
    #[must_use]
    pub fn port(
        &self,
        address: &ResourceAddress,
    ) -> Option<(u16, u16, &'static str, &'static str)> {
        self.ports.get(address).map(|port| {
            (
                port.published_port,
                port.target_port,
                port.publish_mode,
                port.protocol,
            )
        })
    }

    /// Returns the typed target, storage type, and path used for Mount identity discovery.
    #[must_use]
    pub fn mount(
        &self,
        address: &ResourceAddress,
    ) -> Option<(&ResourceAddress, &'static str, &str)> {
        self.mounts
            .get(address)
            .map(|mount| (&mount.target, mount.mount_type, mount.mount_path.as_str()))
    }
}

/// Resolved sensitive execution input paired with its durable receipt.
///
/// The value is non-cloneable and non-serializable. Taking it apart transfers
/// ownership to the future executor while retaining zeroization on drop.
pub struct SensitiveExecutionValue {
    value: Zeroizing<Vec<u8>>,
    fingerprint: SensitiveFingerprint,
}

impl SensitiveExecutionValue {
    /// Transfers the exact resolved bytes and opaque receipt to the caller.
    #[must_use]
    pub fn into_parts(self) -> (Zeroizing<Vec<u8>>, SensitiveFingerprint) {
        (self.value, self.fingerprint)
    }
}

impl fmt::Debug for SensitiveExecutionValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveExecutionValue([REDACTED])")
    }
}

impl fmt::Debug for ExecutionBindings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExecutionBindings([REDACTED])")
    }
}

/// A redaction-safe configuration compilation failure.
#[derive(Debug, Error)]
pub enum CompileDesiredError {
    /// Explicit null cannot represent a boolean protection intent safely.
    #[error("DOKCMP001: lifecycle protection cannot be null")]
    ProtectionCannotBeCleared,
    /// A validated configuration path has no equivalent in the planner vocabulary.
    #[error("DOKCMP002: lifecycle property path is unsupported by this planner")]
    UnsupportedLifecycleProperty,
    /// Core desired-state invariants rejected the compiled snapshot.
    #[error("DOKCMP003: compiled desired state is invalid")]
    InvalidDesiredState(#[source] DesiredStateError),
    /// A sensitive value lacks the durable intent needed for convergent planning.
    #[error("DOKCMP004: sensitive desired values are unsupported")]
    SensitiveIntentUnsupported,
    /// The per-instance fingerprint key could not be loaded safely.
    #[error("DOKCMP005: sensitive fingerprint key storage is unavailable")]
    SensitiveFingerprintUnavailable,
    /// A configured environment source is absent.
    #[error("DOKCMP006: sensitive environment source is missing")]
    SensitiveEnvironmentMissing,
    /// A configured environment source cannot be represented as UTF-8.
    #[error("DOKCMP007: sensitive environment source is not valid UTF-8")]
    SensitiveEnvironmentNotUtf8,
    /// A configured file source could not be opened and read safely.
    #[error("DOKCMP008: sensitive file source is unavailable or unsafe")]
    SensitiveFileUnavailable,
    /// A configured file source exceeds the strict byte limit.
    #[error("DOKCMP009: sensitive file source exceeds the one MiB limit")]
    SensitiveFileTooLarge,
    /// A configured file source is not valid UTF-8.
    #[error("DOKCMP010: sensitive file source is not valid UTF-8")]
    SensitiveFileNotUtf8,
}

impl CompileDesiredError {
    /// Returns the stable diagnostic code.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::ProtectionCannotBeCleared => "DOKCMP001",
            Self::UnsupportedLifecycleProperty => "DOKCMP002",
            Self::InvalidDesiredState(_) => "DOKCMP003",
            Self::SensitiveIntentUnsupported => "DOKCMP004",
            Self::SensitiveFingerprintUnavailable => "DOKCMP005",
            Self::SensitiveEnvironmentMissing => "DOKCMP006",
            Self::SensitiveEnvironmentNotUtf8 => "DOKCMP007",
            Self::SensitiveFileUnavailable => "DOKCMP008",
            Self::SensitiveFileTooLarge => "DOKCMP009",
            Self::SensitiveFileNotUtf8 => "DOKCMP010",
        }
    }
}

/// Compiles a validated configuration without performing I/O or resolving values.
///
/// Concrete sensitive values fail closed with DOKCMP004; clear and unmanaged
/// sensitive fields remain available to offline callers.
pub fn compile_desired(
    config: &DokployConfig,
    digest: ConfigDigest,
) -> Result<CompiledDesired, CompileDesiredError> {
    compile_desired_with_fingerprints(config, digest, &BTreeMap::new())
}

fn compile_desired_with_fingerprints(
    config: &DokployConfig,
    digest: ConfigDigest,
    fingerprints: &BTreeMap<(ResourceAddress, PropertyPath), SensitiveFingerprint>,
) -> Result<CompiledDesired, CompileDesiredError> {
    let mut resources = BTreeMap::new();

    for (address, resource) in config.resources() {
        let mut properties = BTreeMap::new();

        match resource {
            ResourceConfig::Project(project) => compile_string_field(
                &mut properties,
                PropertyPath::Description,
                project.description(),
            ),
            ResourceConfig::Environment(environment) => compile_string_field(
                &mut properties,
                PropertyPath::Description,
                environment.description(),
            ),
            ResourceConfig::Application(application) => {
                compile_string_field(
                    &mut properties,
                    PropertyPath::Description,
                    application.description(),
                );
                compile_u32_field(
                    &mut properties,
                    PropertyPath::Replicas,
                    application.replicas(),
                );
                compile_source(&mut properties, application.source());
                compile_environment(
                    &mut properties,
                    address,
                    application.environment(),
                    fingerprints,
                )?;
                for (path, field) in [
                    (PropertyPath::Server, application.server()),
                    (PropertyPath::BuildServer, application.build_server()),
                    (PropertyPath::Registry, application.registry()),
                    (PropertyPath::BuildRegistry, application.build_registry()),
                    (
                        PropertyPath::RollbackRegistry,
                        application.rollback_registry(),
                    ),
                ] {
                    compile_selector_field(&mut properties, path, field);
                }
            }
            ResourceConfig::Compose(compose) => {
                compile_string_field(
                    &mut properties,
                    PropertyPath::Description,
                    compose.description(),
                );
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::ComposeDocument,
                    compose.document(),
                    fingerprints,
                )?;
            }
            ResourceConfig::Postgres(postgres) => {
                compile_string_field(&mut properties, PropertyPath::Database, postgres.database());
                compile_string_field(&mut properties, PropertyPath::Username, postgres.username());
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    postgres.password(),
                    fingerprints,
                )?;
            }
            ResourceConfig::MySql(mysql) => {
                compile_string_field(&mut properties, PropertyPath::Database, mysql.database());
                compile_string_field(&mut properties, PropertyPath::Username, mysql.username());
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    mysql.password(),
                    fingerprints,
                )?;
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::RootPassword,
                    mysql.root_password(),
                    fingerprints,
                )?;
            }
            ResourceConfig::MariaDb(mariadb) => {
                compile_string_field(&mut properties, PropertyPath::Database, mariadb.database());
                compile_string_field(&mut properties, PropertyPath::Username, mariadb.username());
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    mariadb.password(),
                    fingerprints,
                )?;
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::RootPassword,
                    mariadb.root_password(),
                    fingerprints,
                )?;
            }
            ResourceConfig::Mongo(mongo) => {
                compile_string_field(&mut properties, PropertyPath::Username, mongo.username());
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    mongo.password(),
                    fingerprints,
                )?;
                compile_bool_field(
                    &mut properties,
                    PropertyPath::ReplicaSets,
                    mongo.replica_sets(),
                );
            }
            ResourceConfig::LibSql(libsql) => {
                compile_string_field(
                    &mut properties,
                    PropertyPath::Description,
                    libsql.description(),
                );
                compile_string_field(&mut properties, PropertyPath::Username, libsql.username());
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    libsql.password(),
                    fingerprints,
                )?;
                compile_libsql_node(&mut properties, libsql.node());
            }
            ResourceConfig::Redis(redis) => compile_sensitive_field(
                &mut properties,
                address,
                PropertyPath::Password,
                redis.password(),
                fingerprints,
            )?,
            ResourceConfig::Domain(domain) => {
                compile_string_field(&mut properties, PropertyPath::Host, domain.host());
                let application = match domain.application() {
                    Field::Unmanaged => None,
                    Field::Clear => Some(OwnedValue::Null),
                    Field::Set(address) => Some(comparable(serde_json::json!(address.to_string()))),
                };
                if let Some(application) = application {
                    properties.insert(PropertyPath::Application, application);
                }
            }
            ResourceConfig::Port(port) => {
                properties.insert(
                    PropertyPath::PublishedPort,
                    comparable(serde_json::json!(port.published_port().get())),
                );
                properties.insert(
                    PropertyPath::TargetPort,
                    comparable(serde_json::json!(port.target_port().get())),
                );
                properties.insert(
                    PropertyPath::PublishMode,
                    comparable(serde_json::json!(match port.publish_mode() {
                        dokploy_config::PortPublishModeConfig::Ingress => "ingress",
                        dokploy_config::PortPublishModeConfig::Host => "host",
                    })),
                );
                properties.insert(
                    PropertyPath::Protocol,
                    comparable(serde_json::json!(match port.protocol() {
                        dokploy_config::PortProtocolConfig::Tcp => "tcp",
                        dokploy_config::PortProtocolConfig::Udp => "udp",
                    })),
                );
            }
            ResourceConfig::Redirect(redirect) => {
                properties.insert(
                    PropertyPath::Regex,
                    comparable(serde_json::json!(redirect.regex().as_str())),
                );
                properties.insert(
                    PropertyPath::Replacement,
                    comparable(serde_json::json!(redirect.replacement().as_str())),
                );
                properties.insert(
                    PropertyPath::Permanent,
                    comparable(serde_json::json!(redirect.permanent())),
                );
            }
            ResourceConfig::Security(security) => {
                properties.insert(
                    PropertyPath::Username,
                    comparable(serde_json::json!(security.username().as_str())),
                );
                compile_sensitive_field(
                    &mut properties,
                    address,
                    PropertyPath::Password,
                    security.password(),
                    fingerprints,
                )?;
            }
            ResourceConfig::Mount(mount) => {
                properties.insert(
                    PropertyPath::Target,
                    comparable(serde_json::json!(mount.target().to_string())),
                );
                properties.insert(
                    PropertyPath::MountType,
                    comparable(serde_json::json!(mount.source().type_name())),
                );
                properties.insert(
                    PropertyPath::MountPath,
                    comparable(serde_json::json!(mount.mount_path())),
                );
                match mount.source() {
                    MountSourceConfig::Bind { host_path } => {
                        properties.insert(
                            PropertyPath::HostPath,
                            comparable(serde_json::json!(host_path)),
                        );
                    }
                    MountSourceConfig::Volume { volume_name } => {
                        properties.insert(
                            PropertyPath::VolumeName,
                            comparable(serde_json::json!(volume_name)),
                        );
                    }
                    MountSourceConfig::File { file_path, content } => {
                        properties.insert(
                            PropertyPath::FilePath,
                            comparable(serde_json::json!(file_path)),
                        );
                        compile_sensitive_field(
                            &mut properties,
                            address,
                            PropertyPath::FileContent,
                            content,
                            fingerprints,
                        )?;
                    }
                }
            }
        }

        let protection = match resource.lifecycle().protect() {
            Field::Set(value) => ProtectionIntent::Set(*value),
            Field::Unmanaged => ProtectionIntent::Unmanaged,
            Field::Clear => return Err(CompileDesiredError::ProtectionCannotBeCleared),
        };

        let containment = config.parent_of(address).cloned();
        let dependencies = compile_dependencies(resource);
        let ignored_changes = resource
            .lifecycle()
            .ignore_changes()
            .iter()
            .map(ToString::to_string)
            .map(|path| {
                path.parse()
                    .map_err(|_| CompileDesiredError::UnsupportedLifecycleProperty)
            })
            .collect::<Result<Vec<_>, _>>()?;

        resources.insert(
            address.clone(),
            DesiredResource::new(properties)
                .with_protection(protection)
                .with_containment(containment)
                .with_dependencies(dependencies)
                .with_ignored_changes(ignored_changes),
        );
    }

    let moves = config
        .moves()
        .iter()
        .map(|declaration| MoveDirective::new(declaration.from().clone(), declaration.to().clone()))
        .collect();
    let removals = config
        .removed()
        .iter()
        .map(|declaration| RemovalDirective::new(declaration.from().clone(), declaration.destroy()))
        .collect();
    let desired_state = DesiredState::try_new(digest, resources)
        .map_err(CompileDesiredError::InvalidDesiredState)?
        .with_moves(moves)
        .with_removals(removals);

    Ok(CompiledDesired {
        desired_state,
        bindings: compile_bindings(config),
    })
}

/// Compiles desired state for one normalized Dokploy instance.
///
/// Concrete sensitive values are resolved once, fingerprinted for convergent
/// planning, and retained only in the non-serializable execution sidecar.
/// Clear and unmanaged sensitive fields remain fully offline.
pub fn compile_desired_for_instance(
    config: &DokployConfig,
    source_digest: ConfigDigest,
    instance: InstanceIdentity,
    workspace_directory: &Path,
) -> Result<CompiledDesired, CompileDesiredError> {
    sensitive_compilation::compile_for_instance(
        config,
        source_digest,
        instance,
        workspace_directory,
    )
}

fn compile_bindings(config: &DokployConfig) -> ExecutionBindings {
    let mut bindings = ExecutionBindings {
        parents: config.parents().clone(),
        ..ExecutionBindings::default()
    };

    for (address, resource) in config.resources() {
        for (name, kind, selector) in resource.external_selectors() {
            let path: PropertyPath = name
                .parse()
                .expect("configured selector properties are in the planner vocabulary");
            bindings.selectors.insert(
                (address.clone(), path),
                SelectorBinding {
                    kind,
                    selector: selector.clone(),
                },
            );
        }
        match resource {
            ResourceConfig::Domain(domain) => {
                if let Some(host) = domain.host().as_set() {
                    bindings.domain_hosts.insert(address.clone(), host.clone());
                }
                if let Some(application) = domain.application().as_set() {
                    bindings
                        .domain_applications
                        .insert(address.clone(), application.clone());
                }
            }
            ResourceConfig::Port(port) => {
                bindings.ports.insert(
                    address.clone(),
                    PortBinding {
                        published_port: port.published_port().get(),
                        target_port: port.target_port().get(),
                        publish_mode: match port.publish_mode() {
                            dokploy_config::PortPublishModeConfig::Ingress => "ingress",
                            dokploy_config::PortPublishModeConfig::Host => "host",
                        },
                        protocol: match port.protocol() {
                            dokploy_config::PortProtocolConfig::Tcp => "tcp",
                            dokploy_config::PortProtocolConfig::Udp => "udp",
                        },
                    },
                );
            }
            ResourceConfig::Redirect(redirect) => {
                bindings
                    .redirect_regexes
                    .insert(address.clone(), redirect.regex().as_str().to_owned());
            }
            ResourceConfig::Security(security) => {
                bindings
                    .security_usernames
                    .insert(address.clone(), security.username().as_str().to_owned());
            }
            ResourceConfig::Mount(mount) => {
                bindings.mounts.insert(
                    address.clone(),
                    MountBinding {
                        target: mount.target().clone(),
                        mount_type: mount.source().type_name(),
                        mount_path: mount.mount_path().to_owned(),
                    },
                );
            }
            ResourceConfig::Project(_)
            | ResourceConfig::Environment(_)
            | ResourceConfig::Application(_)
            | ResourceConfig::Compose(_)
            | ResourceConfig::Postgres(_)
            | ResourceConfig::MySql(_)
            | ResourceConfig::MariaDb(_)
            | ResourceConfig::Mongo(_)
            | ResourceConfig::LibSql(_)
            | ResourceConfig::Redis(_) => {}
        }
    }

    bindings
}

fn compile_dependencies(resource: &ResourceConfig) -> Vec<ResourceAddress> {
    let mut dependencies: BTreeSet<_> = resource.depends_on().iter().cloned().collect();

    if let ResourceConfig::Application(application) = resource
        && let Field::Set(environment) = application.environment()
    {
        for value in environment.values() {
            if let Field::Set(ConfigValue::Reference(reference)) = value {
                dependencies.insert(reference.address().clone());
            }
        }
    }

    if let ResourceConfig::Domain(domain) = resource
        && let Field::Set(application) = domain.application()
    {
        dependencies.insert(application.clone());
    }

    if let ResourceConfig::Mount(mount) = resource {
        dependencies.insert(mount.target().clone());
    }

    dependencies.into_iter().collect()
}

fn compile_string_field(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    path: PropertyPath,
    field: &Field<String>,
) {
    let value = match field {
        Field::Unmanaged => return,
        Field::Clear => OwnedValue::Null,
        Field::Set(value) => comparable(serde_json::json!(value)),
    };
    properties.insert(path, value);
}

fn compile_bool_field(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    path: PropertyPath,
    field: &Field<bool>,
) {
    let value = match field {
        Field::Unmanaged => return,
        Field::Clear => OwnedValue::Null,
        Field::Set(value) => comparable(serde_json::json!(value)),
    };
    properties.insert(path, value);
}

fn compile_selector_field(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    path: PropertyPath,
    field: &Field<ExternalSelector>,
) {
    let value = match field {
        Field::Unmanaged => return,
        Field::Clear => OwnedValue::Null,
        Field::Set(selector) => OwnedValue::Value(crate::external::selector_value(selector)),
    };
    properties.insert(path, value);
}

fn compile_libsql_node(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    field: &Field<LibSqlNodeConfig>,
) {
    let value = match field {
        Field::Unmanaged => return,
        Field::Clear => OwnedValue::Null,
        Field::Set(LibSqlNodeConfig::Primary) => comparable(serde_json::json!({"type": "primary"})),
        Field::Set(LibSqlNodeConfig::Replica { primary_url }) => comparable(serde_json::json!({
            "type": "replica",
            "primary_url": primary_url,
        })),
    };
    properties.insert(PropertyPath::Node, value);
}

fn compile_u32_field(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    path: PropertyPath,
    field: &Field<u32>,
) {
    let value = match field {
        Field::Unmanaged => return,
        Field::Clear => OwnedValue::Null,
        Field::Set(value) => comparable(serde_json::json!(value)),
    };
    properties.insert(path, value);
}

fn compile_sensitive_field<T>(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    address: &ResourceAddress,
    path: PropertyPath,
    field: &Field<T>,
    fingerprints: &BTreeMap<(ResourceAddress, PropertyPath), SensitiveFingerprint>,
) -> Result<(), CompileDesiredError> {
    let value = match field {
        Field::Unmanaged => return Ok(()),
        Field::Clear => OwnedValue::Null,
        Field::Set(_) => {
            let fingerprint = fingerprints
                .get(&(address.clone(), path.clone()))
                .ok_or(CompileDesiredError::SensitiveIntentUnsupported)?;
            OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(fingerprint.clone()))
        }
    };
    properties.insert(path, value);

    Ok(())
}

fn compile_source(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    source: &Field<SourceConfig>,
) {
    let github = match source {
        Field::Unmanaged => return,
        Field::Clear => {
            properties.insert(PropertyPath::Source, OwnedValue::Null);
            return;
        }
        Field::Set(SourceConfig::GitHub(github)) => github,
    };

    properties.insert(
        PropertyPath::SourceRepository,
        comparable(serde_json::json!(github.repository())),
    );
    compile_string_field(properties, PropertyPath::SourceBranch, github.branch());
}

fn compile_environment(
    properties: &mut BTreeMap<PropertyPath, OwnedValue>,
    address: &ResourceAddress,
    environment: &Field<BTreeMap<String, Field<ConfigValue>>>,
    fingerprints: &BTreeMap<(ResourceAddress, PropertyPath), SensitiveFingerprint>,
) -> Result<(), CompileDesiredError> {
    let variables = match environment {
        Field::Unmanaged => return Ok(()),
        Field::Clear => {
            properties.insert(PropertyPath::Environment, OwnedValue::Null);
            return Ok(());
        }
        Field::Set(variables) if variables.is_empty() => {
            properties.insert(PropertyPath::Environment, OwnedValue::EmptyCollection);
            return Ok(());
        }
        Field::Set(variables) => variables,
    };

    for (name, value) in variables {
        let path = PropertyPath::environment_variable(name)
            .expect("validated configuration has valid environment names");
        compile_sensitive_field(properties, address, path, value, fingerprints)?;
    }

    Ok(())
}

fn comparable(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(
        dokploy_core::ComparableValue::try_from_json(value)
            .expect("supported config values are non-null"),
    )
}
