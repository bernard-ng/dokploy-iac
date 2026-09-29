//! Compiles validated configuration into planner input and deferred execution bindings.
//!
//! This module is the composition seam between configuration syntax and the
//! pure planning domain. Compilation never resolves remote IDs, environment
//! variables, secret files, or secret bytes.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};

use dokploy_config::{ConfigValue, DokployConfig, Field, ResourceConfig, SourceConfig};
use dokploy_core::{
    ConfigDigest, DesiredResource, DesiredState, DesiredStateError, MoveDirective, OwnedValue,
    PropertyPath, ProtectionIntent, RemovalDirective,
};
use dokploy_state::ResourceAddress;
use thiserror::Error;

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
        }
    }
}

/// Compiles a validated configuration without performing I/O or resolving values.
///
/// Concrete environment values and set database passwords fail closed until
/// the planner can compare a durable, non-secret intent fingerprint.
pub fn compile_desired(
    config: &DokployConfig,
    digest: ConfigDigest,
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
                compile_environment(&mut properties, application.environment())?;
            }
            ResourceConfig::Postgres(postgres) => {
                compile_string_field(&mut properties, PropertyPath::Database, postgres.database());
                compile_string_field(&mut properties, PropertyPath::Username, postgres.username());
                compile_sensitive_field(
                    &mut properties,
                    PropertyPath::Password,
                    postgres.password(),
                )?;
            }
            ResourceConfig::Redis(redis) => {
                compile_sensitive_field(&mut properties, PropertyPath::Password, redis.password())?
            }
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

        let dependencies = compile_dependencies(address, resource, config.parents());

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

fn compile_bindings(config: &DokployConfig) -> ExecutionBindings {
    let mut bindings = ExecutionBindings {
        parents: config.parents().clone(),
        ..ExecutionBindings::default()
    };

    for (address, resource) in config.resources() {
        match resource {
            ResourceConfig::Domain(domain) => {
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
            | ResourceConfig::Redis(_) => {}
        }
    }

    bindings
}

fn compile_dependencies(
    address: &ResourceAddress,
    resource: &ResourceConfig,
    parents: &BTreeMap<ResourceAddress, ResourceAddress>,
) -> Vec<ResourceAddress> {
    let mut dependencies: BTreeSet<_> = resource.depends_on().iter().cloned().collect();

    if let Some(parent) = parents.get(address) {
        dependencies.insert(parent.clone());
    }

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
    path: PropertyPath,
    field: &Field<T>,
) -> Result<(), CompileDesiredError> {
    let value = match field {
        Field::Unmanaged => return Ok(()),
        Field::Clear => OwnedValue::Null,
        Field::Set(_) => return Err(CompileDesiredError::SensitiveIntentUnsupported),
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
    environment: &Field<BTreeMap<String, Field<dokploy_config::ConfigValue>>>,
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
        compile_sensitive_field(properties, path, value)?;
    }

    Ok(())
}

fn comparable(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(
        dokploy_core::ComparableValue::try_from_json(value)
            .expect("supported config values are non-null"),
    )
}
