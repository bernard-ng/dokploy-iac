//! A small, synthetic kind set for the kernel tests.
//!
//! The planner, snapshots, and checkpoints know no kind: everything comes from a spec. These
//! tests therefore use specs of their own (`widget`, a nested `gadget`, and `cache`) that
//! exercise every property shape, instead of depending on the repository's specs, which grow
//! with every milestone.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::sync::OnceLock;

use dokploy_core::{
    ComparableValue, ConfigDigest, DesiredResource, DesiredState, MutationContract, OwnedValue,
    PropertyObservation, PropertyPath, PropertyUnknownReason, ProtectionIntent, RemoteObservation,
    RemoteResource, RemoteState, SensitiveIntent, StoredState, register_spec_kinds,
};
use dokploy_spec::{KindSpec, SpecRegistry, parse_spec};
use dokploy_state::{
    DocumentId, FingerprintKeyId, InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress,
    ResourceState, SensitiveFingerprint, SensitiveInputs, SensitivePropertyPath, StateFile,
};
use semver::Version;
use serde_json::Value;
use uuid::Uuid;

const WIDGET: &str = "
kind: widget
scope: settings
section: widgets
title: Widget
identity: { key: name, collision: [name], address: \"widget.{key}\" }
api:
  id: widgetId
  create: { op: widget.create }
  update: { op: widget.update }
  remove: { op: widget.remove }
  read:
    list: { op: widget.all, authority: authoritative }
  create_identity: { from_response: /widgetId }
fields:
  name: { type: text, min_len: 1, default: key }
  description: { type: text, nullable: true }
  replicas: { type: int, min: 0, nullable: true }
  flavor: { type: \"enum[small, large]\", mutability: create_only }
  status: { type: text, mutability: computed }
  limits:
    type: struct
    granularity: field
    members:
      cpu: { type: text, nullable: true }
      memory: { type: text, nullable: true }
  environment: { type: env, granularity: key }
  labels: { type: \"map<text, text>\", granularity: key }
  password: { type: text, class: secret, mutability: write_only, nullable: true }
  token: { type: text, class: secret, mutability: write_only }
  server: { type: selector(server), nullable: true }
  mirror: { type: selector(registry), nullable: true }
write:
  - { op: widget.update, fields: [name, description, replicas, limits, environment, labels, password, token, server, mirror], shape: partial }
children:
  - { kind: gadget, section: gadgets }
";

const GADGET: &str = "
kind: gadget
scope: settings
parent: widget
section: gadgets
title: Gadget
identity: { key: name, collision: [name], address: \"gadget.{key}\" }
api:
  id: gadgetId
  create: { op: gadget.create, attach: { widgetId: parent_id } }
  update: { op: gadget.update }
  remove: { op: gadget.remove }
  read:
    list: { op: gadget.all, authority: authoritative }
  create_identity: { from_response: /gadgetId }
fields:
  name: { type: text, min_len: 1, default: key }
  note: { type: text, nullable: true }
write:
  - { op: gadget.update, fields: [name, note], shape: partial }
";

const CACHE: &str = "
kind: cache
scope: settings
section: caches
title: Cache
identity: { key: name, collision: [name], address: \"cache.{key}\" }
api:
  id: cacheId
  create: { op: cache.create }
  update: { op: cache.update }
  remove: { op: cache.remove }
  read:
    list: { op: cache.all, authority: authoritative }
  create_identity: { from_response: /cacheId }
fields:
  name: { type: text, min_len: 1, default: key }
  password: { type: text, class: secret, mutability: write_only, nullable: true }
write:
  - { op: cache.update, fields: [name, password], shape: partial }
";

pub fn specs() -> &'static SpecRegistry {
    static SPECS: OnceLock<SpecRegistry> = OnceLock::new();
    SPECS.get_or_init(|| {
        let kinds: Vec<KindSpec> = [WIDGET, GADGET, CACHE]
            .iter()
            .map(|text| parse_spec(text).expect("the test spec parses"))
            .collect();
        let registry = SpecRegistry::from_specs(kinds, &std::collections::BTreeSet::new())
            .expect("the test specs are consistent");
        register_spec_kinds(&registry).expect("the test kinds register");

        registry
    })
}

pub fn path(kind: &str, property: &str) -> PropertyPath {
    PropertyPath::from_spec(specs().get(kind).expect("kind has a spec"), property)
        .unwrap_or_else(|error| panic!("{kind}.{property}: {error}"))
}

pub fn addr(text: &str) -> ResourceAddress {
    specs();
    text.parse().expect("address is valid")
}

pub fn instance() -> InstanceIdentity {
    InstanceIdentity::parse("https://dokploy.example.test").expect("instance is valid")
}

pub fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("digest is valid")
}

pub fn comparable(content: Value) -> ComparableValue {
    ComparableValue::try_from_json(content).expect("value is comparable")
}

/// An owned value.
pub fn val(content: Value) -> OwnedValue {
    OwnedValue::Value(comparable(content))
}

pub fn key_id(last: u8) -> FingerprintKeyId {
    let mut bytes = [0x11_u8; 16];
    bytes[15] = last;
    FingerprintKeyId::new(Uuid::from_bytes(bytes)).expect("key id is non-nil")
}

pub fn fingerprint(byte: u8) -> SensitiveFingerprint {
    SensitiveFingerprint::new_v1(key_id(1), [byte; 32])
}

/// A secret owned through a receipt.
pub fn secret(byte: u8) -> OwnedValue {
    OwnedValue::Sensitive(SensitiveIntent::from_fingerprint(fingerprint(byte)))
}

pub fn known(content: Value) -> PropertyObservation {
    PropertyObservation::Known(comparable(content))
}

pub fn unreadable() -> PropertyObservation {
    PropertyObservation::Unknown(PropertyUnknownReason::Sensitive)
}

pub fn not_returned() -> PropertyObservation {
    PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
}

/// What a person writes in a document for one resource.
pub struct Want {
    address: ResourceAddress,
    properties: BTreeMap<PropertyPath, OwnedValue>,
    dependencies: Vec<ResourceAddress>,
    ignored: Vec<PropertyPath>,
    replaced: Vec<PropertyPath>,
    protection: ProtectionIntent,
}

pub fn want(address: &str) -> Want {
    Want {
        address: addr(address),
        properties: BTreeMap::new(),
        dependencies: Vec::new(),
        ignored: Vec::new(),
        replaced: Vec::new(),
        protection: ProtectionIntent::Unmanaged,
    }
}

impl Want {
    pub fn prop(mut self, property: &str, value: OwnedValue) -> Self {
        let path = path(self.address.kind().as_str(), property);
        self.properties.insert(path, value);
        self
    }

    pub fn depends_on(mut self, others: &[&str]) -> Self {
        self.dependencies = others.iter().map(|other| addr(other)).collect();
        self
    }

    pub fn ignoring(mut self, properties: &[&str]) -> Self {
        let kind = self.address.kind().as_str();
        self.ignored = properties.iter().map(|p| path(kind, p)).collect();
        self
    }

    pub fn replacing_on(mut self, properties: &[&str]) -> Self {
        let kind = self.address.kind().as_str();
        self.replaced = properties.iter().map(|p| path(kind, p)).collect();
        self
    }

    pub fn protected(mut self, protect: bool) -> Self {
        self.protection = ProtectionIntent::Set(protect);
        self
    }

    fn build(self) -> (ResourceAddress, DesiredResource) {
        let resource = DesiredResource::new(self.properties)
            .with_containment(self.address.parent())
            .with_dependencies(self.dependencies)
            .with_ignored_changes(self.ignored)
            .with_replacement_changes(self.replaced)
            .with_protection(self.protection);
        (self.address, resource)
    }
}

pub fn wanted(resources: Vec<Want>) -> DesiredState {
    try_wanted(resources).expect("desired state is valid")
}

pub fn try_wanted(resources: Vec<Want>) -> Result<DesiredState, dokploy_core::DesiredStateError> {
    DesiredState::try_new(digest(), resources.into_iter().map(Want::build).collect())
}

/// What state remembers about one resource.
pub struct Have {
    address: ResourceAddress,
    id: String,
    managed: Value,
    sensitive: Vec<(String, u8)>,
    protected: bool,
    dependencies: Vec<ResourceAddress>,
}

pub fn have(address: &str, id: &str, managed: Value) -> Have {
    Have {
        address: addr(address),
        id: id.to_owned(),
        managed,
        sensitive: Vec::new(),
        protected: false,
        dependencies: Vec::new(),
    }
}

impl Have {
    pub fn secret(mut self, property: &str, byte: u8) -> Self {
        self.sensitive.push((property.to_owned(), byte));
        self
    }

    pub fn protected(mut self) -> Self {
        self.protected = true;
        self
    }

    pub fn depends_on(mut self, others: &[&str]) -> Self {
        self.dependencies = others.iter().map(|other| addr(other)).collect();
        self
    }

    pub fn into_state_entry(self) -> (ResourceAddress, ResourceState) {
        let resource = self.resource();
        (self.address, resource)
    }

    fn resource(&self) -> ResourceState {
        let sensitive =
            SensitiveInputs::try_from_entries(self.sensitive.iter().map(|(property, byte)| {
                (
                    SensitivePropertyPath::parse(property).expect("sensitive path is valid"),
                    fingerprint(*byte),
                )
            }))
            .expect("sensitive inputs are valid");
        ResourceState::try_new(
            self.address.kind(),
            RemoteId::new(&self.id).expect("remote id is valid"),
            self.protected,
            ManagedInputs::try_from_json(self.managed.clone()).expect("inputs are valid"),
            sensitive,
            self.address.parent(),
            self.dependencies.clone(),
        )
        .expect("resource state is valid")
    }
}

pub fn state(resources: Vec<Have>) -> StateFile {
    let map = resources
        .iter()
        .map(|have| (have.address.clone(), have.resource()))
        .collect();
    StateFile::with_resources(Version::new(0, 1, 0), instance(), DocumentId::Settings, map)
        .expect("state is valid")
}

pub fn stored(resources: Vec<Have>) -> StoredState {
    StoredState::try_from_state(&state(resources), specs()).expect("state projects")
}

pub fn nothing_stored() -> StoredState {
    StoredState::absent(instance())
}

/// One remote observation.
pub fn there(
    address: &str,
    id: &str,
    properties: Vec<(&str, PropertyObservation)>,
) -> (ResourceAddress, RemoteObservation) {
    let address = addr(address);
    let kind = address.kind().as_str().to_owned();
    let properties = properties
        .into_iter()
        .map(|(property, observation)| (path(&kind, property), observation))
        .collect();
    (
        address,
        RemoteObservation::Present(RemoteResource::new(
            RemoteId::new(id).expect("remote id is valid"),
            properties,
        )),
    )
}

pub fn missing(address: &str) -> (ResourceAddress, RemoteObservation) {
    (addr(address), RemoteObservation::Missing)
}

pub fn remote(observations: Vec<(ResourceAddress, RemoteObservation)>) -> RemoteState {
    try_remote(observations).expect("remote snapshot is valid")
}

pub fn try_remote(
    observations: Vec<(ResourceAddress, RemoteObservation)>,
) -> Result<RemoteState, dokploy_core::RemoteStateError> {
    let contracts: Vec<_> = observations
        .iter()
        .map(|(address, _)| {
            (
                address.clone(),
                MutationContract::from_spec(
                    specs()
                        .get(address.kind().as_str())
                        .expect("kind has a spec"),
                ),
            )
        })
        .collect();
    RemoteState::try_new_with_contracts(instance(), observations, contracts)
}

pub fn changes_of(plan: &dokploy_core::Plan) -> Vec<(String, dokploy_core::ChangeKind)> {
    plan.changes()
        .iter()
        .map(|change| (change.address().to_string(), change.kind()))
        .collect()
}

pub fn field_names(change: &dokploy_core::PlannedChange) -> Vec<String> {
    change
        .fields()
        .iter()
        .map(|f| f.key().to_string())
        .collect()
}

pub fn json_of(plan: &dokploy_core::Plan) -> String {
    String::from_utf8(plan.to_json_bytes()).expect("plan JSON is UTF-8")
}

/// A widget with everything its create needs: a flavor (create-only) and a token (write-only).
pub fn creatable(address: &str) -> Want {
    want(address)
        .prop("flavor", val(Value::String("small".into())))
        .prop("token", secret(7))
}

/// State for a widget created by `creatable`.
pub fn created(address: &str, id: &str) -> Have {
    have(address, id, serde_json::json!({"flavor": "small"})).secret("token", 7)
}

/// A remote widget matching `created`.
pub fn alive(address: &str, id: &str) -> (ResourceAddress, RemoteObservation) {
    there(
        address,
        id,
        vec![
            ("flavor", known(Value::String("small".into()))),
            ("token", unreadable()),
        ],
    )
}

/// Observations judged by an explicit contract instead of the one derived from the specs.
pub fn remote_with_contract(
    observations: Vec<(ResourceAddress, RemoteObservation)>,
    contract: MutationContract,
) -> RemoteState {
    let contracts: Vec<_> = observations
        .iter()
        .map(|(address, _)| (address.clone(), contract.clone()))
        .collect();
    RemoteState::try_new_with_contracts(instance(), observations, contracts)
        .expect("contract coverage is exact")
}
