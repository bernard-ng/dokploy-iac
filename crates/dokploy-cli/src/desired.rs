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

use dokploy_config::{ConfigValue, DokployConfig, Field, ResourceConfig, SourceConfig};
use dokploy_core::{
    ConfigDigest, DesiredResource, DesiredState, DesiredStateError, MoveDirective, OwnedValue,
    PropertyPath, ProtectionIntent, RemovalDirective, SensitiveIntent,
};
use dokploy_state::{InstanceIdentity, ResourceAddress, SensitiveFingerprint, StateFile};
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
    sensitive: BTreeMap<(ResourceAddress, PropertyPath), SensitiveExecutionValue>,
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
            ResourceConfig::Project(_)
            | ResourceConfig::Environment(_)
            | ResourceConfig::Application(_)
            | ResourceConfig::Postgres(_)
            | ResourceConfig::MySql(_)
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
