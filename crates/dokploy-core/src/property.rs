use std::{fmt, sync::Arc};

use dokploy_spec::{KindSpec, PathError, PathShape, PropertyInfo};
use dokploy_state::{ResourceKind, SensitiveFingerprint};
use serde::{Serialize, Serializer};
use thiserror::Error;

/// A property path that a kind spec made legal, with the facts the planner needs.
///
/// A property is a field at planning granularity (ADR 0004): a scalar field, one entry of
/// a keyed collection (`environment.LOG_LEVEL`), or one member of a struct (`resources.cpu`).
/// Equality, ordering, and hashing follow the kind and the dotted path only.
#[derive(Clone)]
pub struct PropertyPath(Arc<PropertyInfo>);

impl PropertyPath {
    /// Resolves `path` against a kind spec; the spec decides whether it is legal.
    pub fn from_spec(spec: &KindSpec, path: &str) -> Result<Self, PathError> {
        spec.property(path).map(Self::from_property_info)
    }

    /// Wraps already resolved property facts.
    #[must_use]
    pub fn from_property_info(info: PropertyInfo) -> Self {
        Self(Arc::new(info))
    }

    /// The facts the spec attached to this path.
    #[must_use]
    pub fn info(&self) -> &PropertyInfo {
        &self.0
    }

    /// The canonical dotted path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0.path
    }

    fn key(&self) -> (&str, &str) {
        (&self.0.path, &self.0.kind)
    }

    /// Returns whether values at this path are sensitive or write-only.
    #[must_use]
    pub fn is_sensitive(&self) -> bool {
        self.0.is_sensitive()
    }

    /// Returns whether this path is lifecycle-only.
    #[must_use]
    pub fn is_lifecycle_only(&self) -> bool {
        self.0.is_lifecycle_only()
    }

    /// Returns whether this path owns a keyed collection as a whole.
    ///
    /// Such a root is owned only to clear it or declare it empty; its entries are
    /// separate properties and cannot be owned beside it.
    #[must_use]
    pub fn is_collection_root(&self) -> bool {
        self.0.shape == PathShape::CollectionRoot
    }

    /// Returns whether this path is one entry of the collection rooted at `root`.
    #[must_use]
    pub fn is_entry_of(&self, root: &Self) -> bool {
        self.0.kind == root.0.kind && self.0.root.as_deref() == Some(root.as_str())
    }

    /// Returns whether values at this path select a resource outside the document.
    ///
    /// Selector values are stable `{"local": true}` or `{"name": "..."}` objects.
    /// Physical external identities never become property values.
    #[must_use]
    pub fn is_external_selector(&self) -> bool {
        self.0.is_selector()
    }

    /// Returns whether a selector at this path accepts `{"local": true}`.
    pub(crate) fn accepts_local_selector(&self) -> bool {
        self.0.selector.as_deref() == Some("server")
    }

    pub(crate) fn valid_for_kind(&self, kind: ResourceKind) -> bool {
        self.0.kind == kind.as_str()
    }
}

impl PartialEq for PropertyPath {
    fn eq(&self, other: &Self) -> bool {
        self.key() == other.key()
    }
}

impl Eq for PropertyPath {}

impl PartialOrd for PropertyPath {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for PropertyPath {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.key().cmp(&other.key())
    }
}

impl std::hash::Hash for PropertyPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

impl fmt::Debug for PropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "PropertyPath({}:{})", self.0.kind, self.0.path)
    }
}

impl fmt::Display for PropertyPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for PropertyPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

/// A non-null property value comparable only inside the pure planner.
#[derive(Clone, Eq, PartialEq)]
pub struct ComparableValue(pub(crate) serde_json::Value);

impl ComparableValue {
    /// Wraps a non-null JSON value without exposing it through debug or plans.
    pub fn try_from_json(value: serde_json::Value) -> Result<Self, ComparableValueError> {
        if value.is_null() {
            return Err(ComparableValueError);
        }

        Ok(Self(value))
    }

    pub(crate) const fn as_json(&self) -> &serde_json::Value {
        &self.0
    }
}

impl fmt::Debug for ComparableValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ComparableValue([REDACTED])")
    }
}

/// A null value that must use [`OwnedValue::Null`] instead.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("comparable values cannot be null; use explicit property null ownership")]
pub struct ComparableValueError;

/// An opaque, comparable receipt for one sensitive desired input.
///
/// The receipt can be compared inside the planner, but its key identifier and
/// MAC are never exposed through this interface, debug output, or plan data.
#[derive(Clone)]
pub struct SensitiveIntent(SensitiveFingerprint);

impl SensitiveIntent {
    /// Wraps a durable fingerprint for pure intent comparison.
    #[must_use]
    pub const fn from_fingerprint(fingerprint: SensitiveFingerprint) -> Self {
        Self(fingerprint)
    }

    /// Compares two receipts without exposing their representation.
    #[must_use]
    pub fn matches(&self, other: &Self) -> bool {
        self.0 == other.0
    }

    pub(crate) const fn fingerprint(&self) -> &SensitiveFingerprint {
        &self.0
    }
}

impl PartialEq for SensitiveIntent {
    fn eq(&self, other: &Self) -> bool {
        self.matches(other)
    }
}

impl Eq for SensitiveIntent {}

impl fmt::Debug for SensitiveIntent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SensitiveIntent([REDACTED])")
    }
}

/// A property value owned by desired or durable stored state.
#[derive(Clone, Eq, PartialEq)]
pub enum OwnedValue {
    /// The property is owned and should be cleared remotely.
    Null,
    /// The environment collection is owned and intentionally empty.
    EmptyCollection,
    /// The property is owned with an opaque comparable value.
    Value(ComparableValue),
    /// A sensitive value is owned through an opaque intent receipt.
    Sensitive(SensitiveIntent),
}

impl fmt::Debug for OwnedValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("Null"),
            Self::EmptyCollection => formatter.write_str("EmptyCollection"),
            Self::Value(_) => formatter.write_str("Value([REDACTED])"),
            Self::Sensitive(_) => formatter.write_str("Sensitive([REDACTED])"),
        }
    }
}
