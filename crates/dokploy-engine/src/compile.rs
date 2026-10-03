//! Compiling a validated document into the planner's desired state.

use std::collections::BTreeMap;

use dokploy_core::{
    ComparableValue, ConfigDigest, DesiredResource, DesiredState, DesiredStateError, OwnedValue,
    PropertyPath, ProtectionIntent, SensitiveIntent,
};
use dokploy_model::{Document, EnvValue, Resource, Root, Sections, Source, Value};
use dokploy_spec::{FieldType, Granularity, KindSpec, PathError, Scope, SpecRegistry, parse_type};
use dokploy_state::{
    AddressSuffixError, DocumentId, ResourceAddress, ResourceAddressParseError, ResourceKind,
    ResourceName, ResourceNameError, SensitivePropertyPath,
};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::canonical::{NotComparable, canonical};
use crate::fingerprint::Fingerprinter;
use crate::secrets::{SecretError, SecretReader};

/// A document compiled for planning.
pub struct Compiled {
    pub(crate) scope: Scope,
    pub(crate) document: DocumentId,
    pub(crate) desired: DesiredState,
    pub(crate) resources: BTreeMap<ResourceAddress, CompiledResource>,
    /// Secret values by address and dotted property path, kept only so an apply can send
    /// them. They are zeroed when the compiled document is dropped and never printed.
    pub(crate) secrets: BTreeMap<(ResourceAddress, String), zeroize::Zeroizing<Vec<u8>>>,
}

/// What discovery needs to know about one desired resource.
#[derive(Debug)]
pub(crate) struct CompiledResource {
    pub(crate) spec_kind: String,
    /// The values of the kind's collision fields, in canonical form.
    pub(crate) collision: BTreeMap<String, serde_json::Value>,
}

impl std::fmt::Debug for Compiled {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Compiled")
            .field("document", &self.document)
            .field("resources", &self.resources.keys().collect::<Vec<_>>())
            .field("secrets", &self.secrets.len())
            .finish_non_exhaustive()
    }
}

impl Compiled {
    /// The planner's input.
    #[must_use]
    pub fn desired(&self) -> &DesiredState {
        &self.desired
    }

    /// Whether this is a project or the settings document.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        self.scope
    }

    /// The document this compiles, which names its state.
    #[must_use]
    pub fn document(&self) -> &DocumentId {
        &self.document
    }

    /// Every address the document declares, in order.
    pub fn addresses(&self) -> impl Iterator<Item = &ResourceAddress> {
        self.resources.keys()
    }
}

/// A document that cannot be compiled.
#[derive(Debug, Error)]
pub enum CompileError {
    /// A key does not make a valid address.
    #[error("`{key}` does not make a valid address: {source}")]
    Address {
        key: String,
        #[source]
        source: AddressError,
    },
    /// A kind in the document is unknown to the state layer (the engine registers every
    /// spec kind, so this means the specs and the document disagree).
    #[error("kind `{kind}` is not registered")]
    UnknownKind { kind: String },
    /// A field's path is not a property of its kind.
    #[error("`{address}`: field `{field}`: {source}")]
    Property {
        address: ResourceAddress,
        field: String,
        #[source]
        source: PathError,
    },
    /// A secret source could not be read.
    #[error("`{address}`: field `{field}`: {source}")]
    Secret {
        address: ResourceAddress,
        field: String,
        #[source]
        source: SecretError,
    },
    /// A value the engine cannot compile yet.
    #[error("`{address}`: field `{field}` {reason}")]
    Unsupported {
        address: ResourceAddress,
        field: String,
        reason: &'static str,
    },
    /// A `depends_on` entry names nothing, or something ambiguous.
    #[error("`{address}`: depends_on: {source}")]
    Dependency {
        address: ResourceAddress,
        #[source]
        source: AddressSuffixError,
    },
    /// The planner rejected the desired state.
    #[error("the desired state is invalid: {0}")]
    Desired(#[from] DesiredStateError),
}

/// Why a key is not a valid address.
#[derive(Debug, Error)]
pub enum AddressError {
    #[error(transparent)]
    Name(#[from] ResourceNameError),
    #[error(transparent)]
    Address(#[from] ResourceAddressParseError),
}

pub(crate) struct Compiler<'a> {
    pub(crate) specs: &'a SpecRegistry,
    pub(crate) fingerprinter: &'a Fingerprinter,
    pub(crate) secrets: &'a dyn SecretReader,
}

impl Compiler<'_> {
    pub(crate) fn compile(&self, document: &Document) -> Result<Compiled, CompileError> {
        let mut pending: Vec<(ResourceAddress, &Resource, Option<ResourceAddress>)> = Vec::new();
        match &document.root {
            Root::Project(project) => self.collect(project, None, &mut pending)?,
            Root::Settings(sections) => {
                for resource in sections.values().flat_map(BTreeMap::values) {
                    self.collect(resource, None, &mut pending)?;
                }
            }
        }
        let known: Vec<ResourceAddress> = pending.iter().map(|(a, _, _)| a.clone()).collect();

        let mut desired = BTreeMap::new();
        let mut resources = BTreeMap::new();
        let mut secrets = BTreeMap::new();
        for (address, resource, containment) in &pending {
            let spec = self
                .specs
                .get(&resource.kind)
                .ok_or_else(|| CompileError::UnknownKind {
                    kind: resource.kind.clone(),
                })?;
            let properties = self.properties(address, spec, resource, &mut secrets)?;
            let dependencies = resource
                .depends_on
                .iter()
                .map(|text| {
                    ResourceAddress::resolve_suffix(known.iter(), text)
                        .cloned()
                        .map_err(|source| CompileError::Dependency {
                            address: address.clone(),
                            source,
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let ignored = resource
                .lifecycle
                .ignore_changes
                .iter()
                .map(|path| {
                    PropertyPath::from_spec(spec, path).map_err(|source| CompileError::Property {
                        address: address.clone(),
                        field: path.clone(),
                        source,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let protection = resource
                .lifecycle
                .protect
                .map_or(ProtectionIntent::Unmanaged, ProtectionIntent::Set);
            desired.insert(
                address.clone(),
                DesiredResource::new(properties)
                    .with_containment(containment.clone())
                    .with_dependencies(dependencies)
                    .with_ignored_changes(ignored)
                    .with_protection(protection),
            );
            resources.insert(
                address.clone(),
                CompiledResource {
                    spec_kind: resource.kind.clone(),
                    collision: collision_values(spec, resource),
                },
            );
        }

        let digest = ConfigDigest::parse(hex(&Sha256::digest(document.render().as_bytes())))
            .expect("a SHA-256 digest is a valid configuration digest");
        let document_id = match &document.root {
            Root::Settings(_) => DocumentId::Settings,
            Root::Project(project) => {
                DocumentId::Project(ResourceName::new(project.key.clone()).map_err(|error| {
                    CompileError::Address {
                        key: project.key.clone(),
                        source: error.into(),
                    }
                })?)
            }
        };
        Ok(Compiled {
            scope: document.scope,
            document: document_id,
            desired: DesiredState::try_new(digest, desired)?,
            resources,
            secrets,
        })
    }

    /// Walks the tree, giving every resource its hierarchical address.
    fn collect<'d>(
        &self,
        resource: &'d Resource,
        parent: Option<&ResourceAddress>,
        found: &mut Vec<(ResourceAddress, &'d Resource, Option<ResourceAddress>)>,
    ) -> Result<(), CompileError> {
        let address_error = |source: AddressError| CompileError::Address {
            key: resource.key.clone(),
            source,
        };
        let kind: ResourceKind = resource
            .kind
            .parse()
            .map_err(|_| CompileError::UnknownKind {
                kind: resource.kind.clone(),
            })?;
        let name =
            ResourceName::new(resource.key.clone()).map_err(|error| address_error(error.into()))?;
        let address = match parent {
            Some(parent) => parent
                .child(kind, name)
                .map_err(|error| address_error(error.into()))?,
            None => ResourceAddress::new(kind, name),
        };
        found.push((address.clone(), resource, parent.cloned()));
        self.collect_sections(&resource.children, &address, found)
    }

    fn collect_sections<'d>(
        &self,
        sections: &'d Sections,
        parent: &ResourceAddress,
        found: &mut Vec<(ResourceAddress, &'d Resource, Option<ResourceAddress>)>,
    ) -> Result<(), CompileError> {
        for child in sections.values().flat_map(BTreeMap::values) {
            self.collect(child, Some(parent), found)?;
        }

        Ok(())
    }

    fn properties(
        &self,
        address: &ResourceAddress,
        spec: &KindSpec,
        resource: &Resource,
        secrets: &mut Secrets,
    ) -> Result<BTreeMap<PropertyPath, OwnedValue>, CompileError> {
        let mut properties = BTreeMap::new();
        // A field that defaults to the key (the remote name) is managed even when the
        // document leaves it out, so a rename is corrected and a create names it (ADR 0006).
        for (name, spec_field) in &spec.fields {
            if spec_field.default.as_deref() == Some("key") && !resource.fields.contains_key(name) {
                let path = PropertyPath::from_spec(spec, name).map_err(|source| {
                    CompileError::Property {
                        address: address.clone(),
                        field: name.clone(),
                        source,
                    }
                })?;
                properties.insert(
                    path,
                    comparable(serde_json::Value::String(resource.key.clone())),
                );
            }
        }
        for (name, field) in &resource.fields {
            let spec_field = spec
                .fields
                .get(name)
                .ok_or_else(|| CompileError::Property {
                    address: address.clone(),
                    field: name.clone(),
                    source: PathError::UnknownField {
                        kind: spec.kind.clone(),
                        field: name.clone(),
                    },
                })?;
            let ty = parse_type(&spec_field.ty).map_err(|_| CompileError::Unsupported {
                address: address.clone(),
                field: name.clone(),
                reason: "has a type the engine cannot read",
            })?;
            let path = |dotted: &str| {
                PropertyPath::from_spec(spec, dotted).map_err(|source| CompileError::Property {
                    address: address.clone(),
                    field: dotted.to_owned(),
                    source,
                })
            };
            let unsupported = |reason| CompileError::Unsupported {
                address: address.clone(),
                field: name.clone(),
                reason,
            };

            match &field.value {
                Value::Null => {
                    properties.insert(path(name)?, OwnedValue::Null);
                }
                Value::Source(source) => {
                    let bytes = self.read(address, name, source)?;
                    properties.insert(path(name)?, self.receipt(address, name, &bytes));
                    secrets.insert((address.clone(), name.clone()), bytes);
                }
                Value::Env(variables) if variables.is_empty() => {
                    properties.insert(path(name)?, OwnedValue::EmptyCollection);
                }
                Value::Env(variables) => {
                    for (variable, value) in variables {
                        let dotted = format!("{name}.{variable}");
                        let bytes = match value {
                            EnvValue::Public(text) => zeroizing(text.as_bytes()),
                            EnvValue::Secret(source) => self.read(address, &dotted, source)?,
                        };
                        properties.insert(path(&dotted)?, self.receipt(address, &dotted, &bytes));
                        secrets.insert((address.clone(), dotted), bytes);
                    }
                }
                Value::Map(entries)
                    if matches!(ty, FieldType::Map(_))
                        && spec_field.granularity == Some(Granularity::Key) =>
                {
                    if entries.is_empty() {
                        properties.insert(path(name)?, OwnedValue::EmptyCollection);
                    }
                    for (key, entry) in entries {
                        let dotted = format!("{name}.{key}");
                        let item = match &ty {
                            FieldType::Map(item) => item.as_ref(),
                            _ => &ty,
                        };
                        let json = canonical(entry, item).map_err(|NotComparable::HoldsSecret| {
                            unsupported("holds a secret inside a collection, which is not supported yet")
                        })?;
                        properties.insert(path(&dotted)?, comparable(json));
                    }
                }
                Value::Map(members)
                    if matches!(ty, FieldType::Struct)
                        && spec_field.granularity == Some(Granularity::Field) =>
                {
                    for (member, value) in members {
                        let dotted = format!("{name}.{member}");
                        let json = match value {
                            Value::Null => {
                                properties.insert(path(&dotted)?, OwnedValue::Null);
                                continue;
                            }
                            other => {
                                let member_type = spec_field
                                    .members
                                    .get(member)
                                    .and_then(|m| parse_type(&m.ty).ok())
                                    .unwrap_or(FieldType::Text);
                                canonical(other, &member_type).map_err(
                                    |NotComparable::HoldsSecret| {
                                        unsupported("holds a secret inside a struct, which is not supported yet")
                                    },
                                )?
                            }
                        };
                        properties.insert(path(&dotted)?, comparable(json));
                    }
                }
                other => {
                    let json = canonical(other, &ty).map_err(|NotComparable::HoldsSecret| {
                        unsupported(
                            "holds a secret inside a composite value, which is not supported yet",
                        )
                    })?;
                    properties.insert(path(name)?, comparable(json));
                }
            }
        }

        Ok(properties)
    }

    fn read(
        &self,
        address: &ResourceAddress,
        field: &str,
        source: &Source,
    ) -> Result<zeroize::Zeroizing<Vec<u8>>, CompileError> {
        self.secrets
            .read(source)
            .map_err(|source| CompileError::Secret {
                address: address.clone(),
                field: field.to_owned(),
                source,
            })
    }

    fn receipt(&self, address: &ResourceAddress, dotted: &str, bytes: &[u8]) -> OwnedValue {
        let path = SensitivePropertyPath::parse(dotted)
            .expect("a property path that passed the spec is a valid sensitive path");
        OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(
            self.fingerprinter.fingerprint(address, &path, bytes),
        ))
    }
}

type Secrets = BTreeMap<(ResourceAddress, String), zeroize::Zeroizing<Vec<u8>>>;

fn comparable(json: serde_json::Value) -> OwnedValue {
    ComparableValue::try_from_json(json).map_or(OwnedValue::Null, OwnedValue::Value)
}

fn zeroizing(bytes: &[u8]) -> zeroize::Zeroizing<Vec<u8>> {
    zeroize::Zeroizing::new(bytes.to_vec())
}

/// The canonical values of the fields that identify this resource among its siblings.
fn collision_values(spec: &KindSpec, resource: &Resource) -> BTreeMap<String, serde_json::Value> {
    let mut values = BTreeMap::new();
    for name in &spec.identity.collision {
        let Some(field) = spec.fields.get(name) else {
            continue;
        };
        let value = match resource.fields.get(name) {
            Some(configured) => match &configured.value {
                Value::Text(text) => Some(serde_json::Value::String(text.clone())),
                _ => None,
            },
            None if field.default.as_deref() == Some("key") => {
                Some(serde_json::Value::String(resource.key.clone()))
            }
            None => None,
        };
        if let Some(value) = value {
            values.insert(name.clone(), value);
        }
    }

    values
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(text, "{byte:02x}");
    }

    text
}
