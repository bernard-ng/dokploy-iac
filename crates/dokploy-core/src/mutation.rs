use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::PropertyPath;

/// How Dokploy can converge one owned field or containment edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MutationMode {
    /// Only durable logical metadata changes; no remote mutation is needed.
    StateOnly,
    /// The existing physical resource can be changed in place.
    InPlace,
    /// Convergence requires replacing the physical resource.
    Replace,
    /// The adapter has no proven mutation contract for this transition.
    Unsupported,
}

/// Set and clear behavior for one typed property.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PropertyMutation {
    set: MutationMode,
    clear: MutationMode,
}

impl PropertyMutation {
    /// Defines separate behavior for non-null writes and explicit clears.
    #[must_use]
    pub const fn new(set: MutationMode, clear: MutationMode) -> Self {
        Self { set, clear }
    }

    pub(crate) const fn mode(self, clear: bool) -> MutationMode {
        if clear { self.clear } else { self.set }
    }
}

/// Safe ordering for the two physical steps of replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplacementOrder {
    /// Create the new object before removing the old identity.
    CreateBeforeDelete,
    /// Remove the old identity before creating its replacement.
    DeleteBeforeCreate,
}

/// Adapter-owned facts used by the pure planner to classify mutations.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MutationContract {
    allowed_on_create: BTreeSet<PropertyPath>,
    required_on_create: BTreeSet<PropertyPath>,
    properties: BTreeMap<PropertyPath, PropertyMutation>,
    default_property: PropertyMutation,
    containment: MutationMode,
    replacement_order: ReplacementOrder,
}

impl MutationContract {
    /// Starts a fail-closed contract.
    #[must_use]
    pub fn deny_all(replacement_order: ReplacementOrder) -> Self {
        Self {
            allowed_on_create: BTreeSet::new(),
            required_on_create: BTreeSet::new(),
            properties: BTreeMap::new(),
            default_property: PropertyMutation::new(
                MutationMode::Unsupported,
                MutationMode::Unsupported,
            ),
            containment: MutationMode::Unsupported,
            replacement_order,
        }
    }

    /// Creates a permissive contract for explicit in-memory adapters and tests.
    #[must_use]
    pub fn permissive() -> Self {
        Self {
            allowed_on_create: BTreeSet::new(),
            required_on_create: BTreeSet::new(),
            properties: BTreeMap::new(),
            default_property: PropertyMutation::new(MutationMode::InPlace, MutationMode::InPlace),
            containment: MutationMode::InPlace,
            replacement_order: ReplacementOrder::DeleteBeforeCreate,
        }
    }

    /// Marks a property as required when creating a physical resource.
    #[must_use]
    pub fn requiring(mut self, path: PropertyPath) -> Self {
        self.allowed_on_create.insert(path.clone());
        self.required_on_create.insert(path);
        self
    }

    /// Allows a property to be supplied at creation without requiring it.
    #[must_use]
    pub fn allowing_on_create(mut self, path: PropertyPath) -> Self {
        self.allowed_on_create.insert(path);
        self
    }

    /// Defines mutation behavior for one property.
    #[must_use]
    pub fn with_property(mut self, path: PropertyPath, mutation: PropertyMutation) -> Self {
        self.properties.insert(path, mutation);
        self
    }

    /// Defines behavior for properties not listed explicitly.
    #[must_use]
    pub fn with_default_property(mut self, mutation: PropertyMutation) -> Self {
        self.default_property = mutation;
        self
    }

    /// Defines physical containment-change behavior.
    #[must_use]
    pub fn with_containment(mut self, mode: MutationMode) -> Self {
        self.containment = mode;
        self
    }

    pub(crate) fn missing_required<'a>(
        &'a self,
        properties: &BTreeMap<PropertyPath, crate::OwnedValue>,
    ) -> Option<&'a PropertyPath> {
        self.required_on_create
            .iter()
            .find(|path| !properties.contains_key(*path))
    }

    pub(crate) fn accepts_set_on_create(&self, path: &PropertyPath) -> bool {
        self.allowed_on_create.contains(path)
    }

    pub(crate) fn property_mode(&self, path: &PropertyPath, clear: bool) -> MutationMode {
        self.properties
            .get(path)
            .copied()
            .unwrap_or(self.default_property)
            .mode(clear)
    }

    pub(crate) const fn containment_mode(&self) -> MutationMode {
        self.containment
    }

    pub(crate) const fn replacement_order(&self) -> ReplacementOrder {
        self.replacement_order
    }
}
