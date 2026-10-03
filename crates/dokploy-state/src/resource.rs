use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fmt,
    str::FromStr,
    sync::{OnceLock, RwLock},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;

/// The document and durable state a resource kind belongs to.
///
/// A workspace can hold a project document and a settings document side by side.
/// Each owns an independent state lineage, so a crash or recovery in one scope
/// never blocks the other.
#[derive(
    Clone, Copy, Debug, Default, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum StateScope {
    /// Projects, environments, services, and everything below them.
    #[default]
    Project,
    /// Instance-level settings such as tags.
    Settings,
}

impl StateScope {
    /// Returns the canonical scope name used in state files and diagnostics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Settings => "settings",
        }
    }
}

impl fmt::Display for StateScope {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A resource kind: a lower snake case name such as `application` or `registry`.
///
/// Every kind comes from a kind spec and must be [registered](ResourceKind::register) with
/// its scope and containment parent before it can be parsed or deserialized, so state and
/// journals can only name kinds the running tool knows.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct ResourceKind(&'static str);

/// What the state layer must know about a registered kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct KindInfo {
    scope: StateScope,
    parent: Option<ResourceKind>,
}

/// Registered kinds are bounded so a hostile name cannot grow the table.
const MAX_REGISTERED_KINDS: usize = 1024;
const MAX_KIND_LEN: usize = 64;

fn registry() -> &'static RwLock<BTreeMap<&'static str, KindInfo>> {
    static KINDS: OnceLock<RwLock<BTreeMap<&'static str, KindInfo>>> = OnceLock::new();
    KINDS.get_or_init(|| RwLock::new(BTreeMap::new()))
}

impl ResourceKind {
    /// Returns the canonical kind segment used in logical addresses.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Registers a kind from a spec and returns it.
    ///
    /// `parent` is the kind that contains it (`None` for a document root) and must
    /// already be known. Registering a kind again with the same facts returns the same
    /// kind; different facts are refused.
    pub fn register(
        name: &str,
        scope: StateScope,
        parent: Option<Self>,
    ) -> Result<Self, KindRegistrationError> {
        let valid = name.len() <= MAX_KIND_LEN
            && name.chars().next().is_some_and(|c| c.is_ascii_lowercase())
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if !valid {
            return Err(KindRegistrationError::InvalidName {
                name: name.to_owned(),
            });
        }
        let info = KindInfo { scope, parent };
        let mut kinds = registry().write().expect("kind registry is not poisoned");
        if let Some((known, existing)) = kinds.get_key_value(name) {
            return if *existing == info {
                Ok(Self(known))
            } else {
                Err(KindRegistrationError::Conflict {
                    name: name.to_owned(),
                })
            };
        }
        if kinds.len() >= MAX_REGISTERED_KINDS {
            return Err(KindRegistrationError::TooMany);
        }
        let interned: &'static str = Box::leak(name.to_owned().into_boxed_str());
        kinds.insert(interned, info);
        Ok(Self(interned))
    }

    fn registered(self) -> Option<KindInfo> {
        registry()
            .read()
            .expect("kind registry is not poisoned")
            .get(self.0)
            .copied()
    }

    /// Returns the document scope this kind is declared and tracked in.
    #[must_use]
    pub fn scope(self) -> StateScope {
        self.registered()
            .map_or(StateScope::Project, |info| info.scope)
    }

    /// Returns the required containment parent kind, if the resource is nested.
    #[must_use]
    pub fn containment_parent_kind(self) -> Option<Self> {
        self.registered().and_then(|info| info.parent)
    }
}

impl fmt::Debug for ResourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ResourceKind({})", self.0)
    }
}

impl fmt::Display for ResourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Ord for ResourceKind {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl PartialOrd for ResourceKind {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl FromStr for ResourceKind {
    type Err = ResourceKindParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        registry()
            .read()
            .expect("kind registry is not poisoned")
            .get_key_value(value)
            .map(|(name, _)| Self(name))
            .ok_or_else(|| ResourceKindParseError {
                value: value.to_owned(),
            })
    }
}

impl Serialize for ResourceKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ResourceKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// A kind that cannot be registered.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum KindRegistrationError {
    /// The name is not lower snake case.
    #[error("`{name}` is not a valid kind name")]
    InvalidName { name: String },
    /// The kind is already known with different facts.
    #[error("kind `{name}` is already registered with a different scope or parent")]
    Conflict { name: String },
    /// Too many kinds are registered.
    #[error("too many registered resource kinds")]
    TooMany,
}

/// An unknown logical resource kind.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
#[error("unknown resource kind `{value}`")]
pub struct ResourceKindParseError {
    value: String,
}

/// A validated logical name within one resource kind.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceName(String);

impl ResourceName {
    /// Validates a logical name.
    pub fn new(value: impl Into<String>) -> Result<Self, ResourceNameError> {
        let value = value.into();
        let mut characters = value.char_indices();

        let Some((_, first)) = characters.next() else {
            return Err(ResourceNameError::Empty);
        };

        if !first.is_ascii_lowercase() {
            return Err(ResourceNameError::InvalidStart { character: first });
        }

        if let Some((index, character)) =
            characters.find(|(_, character)| !is_name_character(*character))
        {
            return Err(ResourceNameError::InvalidCharacter { index, character });
        }

        Ok(Self(value))
    }

    /// Returns the validated logical name.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ResourceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ResourceName {
    type Err = ResourceNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

impl Serialize for ResourceName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ResourceName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(de::Error::custom)
    }
}

/// A logical resource name that does not satisfy the canonical grammar.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResourceNameError {
    #[error("resource name cannot be empty")]
    Empty,
    #[error("resource name must start with a lowercase ASCII letter, found `{character}`")]
    InvalidStart { character: char },
    #[error(
        "resource name contains invalid character `{character}` at byte {index}; use lowercase ASCII letters, digits, `-`, or `_`"
    )]
    InvalidCharacter { index: usize, character: char },
}

/// Longest address, in segments, so a hostile state file cannot nest without bound.
const MAX_ADDRESS_DEPTH: usize = 16;
/// Longest rendered address in bytes.
const MAX_ADDRESS_LEN: usize = 8192;

/// One `kind.key` step of an address.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AddressSegment {
    kind: ResourceKind,
    name: ResourceName,
}

impl AddressSegment {
    /// Creates a segment from already typed parts.
    #[must_use]
    pub fn new(kind: ResourceKind, name: ResourceName) -> Self {
        Self { kind, name }
    }

    /// Returns the segment's kind.
    #[must_use]
    pub const fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the segment's key.
    #[must_use]
    pub const fn name(&self) -> &ResourceName {
        &self.name
    }
}

impl fmt::Display for AddressSegment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.kind, self.name)
    }
}

impl FromStr for AddressSegment {
    type Err = ResourceAddressParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (kind, name) = value
            .split_once('.')
            .ok_or(ResourceAddressParseError::MissingSeparator)?;

        Ok(Self {
            kind: kind.parse()?,
            name: name.parse()?,
        })
    }
}

/// A stable logical identity independent of Dokploy's remote identifier.
///
/// An address is a `/`-separated path of `kind.key` segments from the document root,
/// such as `project.shop/environment.staging/application.api/redirect.www`. It is the
/// path of keys through the containment tree (ADR 0006). An address like
/// `application.api` is a path of one segment.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceAddress {
    segments: Vec<AddressSegment>,
}

impl ResourceAddress {
    /// Creates a one-segment address from already typed parts.
    #[must_use]
    pub fn new(kind: ResourceKind, name: ResourceName) -> Self {
        Self {
            segments: vec![AddressSegment { kind, name }],
        }
    }

    /// Creates an address from its segments.
    pub fn from_segments(segments: Vec<AddressSegment>) -> Result<Self, ResourceAddressParseError> {
        if segments.is_empty() {
            return Err(ResourceAddressParseError::Empty);
        }
        if segments.len() > MAX_ADDRESS_DEPTH {
            return Err(ResourceAddressParseError::TooDeep);
        }
        let address = Self { segments };
        if address.to_string().len() > MAX_ADDRESS_LEN {
            return Err(ResourceAddressParseError::TooLong);
        }
        Ok(address)
    }

    /// Returns the address of a resource this one contains.
    pub fn child(
        &self,
        kind: ResourceKind,
        name: ResourceName,
    ) -> Result<Self, ResourceAddressParseError> {
        let mut segments = self.segments.clone();
        segments.push(AddressSegment { kind, name });
        Self::from_segments(segments)
    }

    /// Returns the addressed resource kind: the kind of the last segment.
    #[must_use]
    pub fn kind(&self) -> ResourceKind {
        self.last().kind
    }

    /// Returns the addressed key: the key of the last segment.
    #[must_use]
    pub fn name(&self) -> &ResourceName {
        &self.last().name
    }

    /// Returns every segment from the document root.
    #[must_use]
    pub fn segments(&self) -> &[AddressSegment] {
        &self.segments
    }

    /// Returns how many segments the address has; a document root has one.
    #[must_use]
    pub fn depth(&self) -> usize {
        self.segments.len()
    }

    /// Returns the address of the containing resource, if there is one in the path.
    #[must_use]
    pub fn parent(&self) -> Option<Self> {
        (self.segments.len() > 1).then(|| Self {
            segments: self.segments[..self.segments.len() - 1].to_vec(),
        })
    }

    /// Returns whether `other` lies strictly below this address.
    #[must_use]
    pub fn is_ancestor_of(&self, other: &Self) -> bool {
        other.segments.len() > self.segments.len() && other.segments.starts_with(&self.segments)
    }

    /// Returns whether this address is `prefix` or lies below it.
    #[must_use]
    pub fn starts_with(&self, prefix: &Self) -> bool {
        self.segments.starts_with(&prefix.segments)
    }

    /// Replaces the leading `from` with `to`, or returns `None` when it does not lead.
    #[must_use]
    pub fn rebased(&self, from: &Self, to: &Self) -> Option<Self> {
        let rest = self.segments.strip_prefix(from.segments.as_slice())?;
        let mut segments = to.segments.clone();
        segments.extend_from_slice(rest);
        Self::from_segments(segments).ok()
    }

    /// Resolves a user-typed suffix against known addresses (ADR 0006).
    ///
    /// `domain.api` matches every address ending in that segment. An exact full
    /// address always matches itself. One match is returned; none or several are
    /// errors, and several list their candidates in address order.
    pub fn resolve_suffix<'a>(
        candidates: impl IntoIterator<Item = &'a Self>,
        suffix: &str,
    ) -> Result<&'a Self, AddressSuffixError> {
        let wanted = parse_segments(suffix).map_err(AddressSuffixError::Malformed)?;
        let mut matches: Vec<&'a Self> = candidates
            .into_iter()
            .filter(|candidate| candidate.segments.ends_with(&wanted))
            .collect();
        matches.sort();
        match matches.as_slice() {
            [] => Err(AddressSuffixError::NotFound {
                suffix: suffix.to_owned(),
            }),
            [only] => Ok(only),
            _ => Err(AddressSuffixError::Ambiguous {
                suffix: suffix.to_owned(),
                candidates: matches.into_iter().cloned().collect(),
            }),
        }
    }

    fn last(&self) -> &AddressSegment {
        self.segments
            .last()
            .expect("an address always has at least one segment")
    }
}

fn parse_segments(value: &str) -> Result<Vec<AddressSegment>, ResourceAddressParseError> {
    value
        .split('/')
        .map(|segment| {
            if segment.is_empty() {
                Err(ResourceAddressParseError::Empty)
            } else {
                segment.parse()
            }
        })
        .collect()
}

impl fmt::Display for ResourceAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, segment) in self.segments.iter().enumerate() {
            if index > 0 {
                formatter.write_str("/")?;
            }
            write!(formatter, "{segment}")?;
        }
        Ok(())
    }
}

impl FromStr for ResourceAddress {
    type Err = ResourceAddressParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() > MAX_ADDRESS_LEN {
            return Err(ResourceAddressParseError::TooLong);
        }
        Self::from_segments(parse_segments(value)?)
    }
}

impl Serialize for ResourceAddress {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ResourceAddress {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

/// A malformed logical resource address.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum ResourceAddressParseError {
    #[error("resource address must contain a kind and name separated by `.`")]
    MissingSeparator,
    #[error("resource address has an empty segment")]
    Empty,
    #[error("resource address is nested more than {MAX_ADDRESS_DEPTH} levels deep")]
    TooDeep,
    #[error("resource address is longer than {MAX_ADDRESS_LEN} bytes")]
    TooLong,
    #[error(transparent)]
    Kind(#[from] ResourceKindParseError),
    #[error(transparent)]
    Name(#[from] ResourceNameError),
}

/// A typed suffix that does not name exactly one known address.
#[derive(Clone, Debug, Error, Eq, PartialEq)]
pub enum AddressSuffixError {
    /// The suffix is not a sequence of `kind.key` segments.
    #[error(transparent)]
    Malformed(ResourceAddressParseError),
    /// No known address ends with the suffix.
    #[error("no resource matches `{suffix}`")]
    NotFound { suffix: String },
    /// Several known addresses end with the suffix.
    #[error(
        "`{suffix}` is ambiguous; it could be any of: {}",
        join_addresses(candidates)
    )]
    Ambiguous {
        suffix: String,
        candidates: Vec<ResourceAddress>,
    },
}

fn join_addresses(addresses: &[ResourceAddress]) -> String {
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(", ")
}

fn is_name_character(character: char) -> bool {
    character.is_ascii_lowercase() || character.is_ascii_digit() || matches!(character, '-' | '_')
}
