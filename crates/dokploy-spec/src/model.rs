//! The serde model of a kind spec.

use std::collections::BTreeMap;

use serde::Deserialize;

/// Whether a kind lives in a project document or the settings document.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// Inside a `project:` document.
    Project,
    /// Inside the `settings:` document.
    Settings,
}

/// How a kind may be created and destroyed (ADR 0013).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum KindClass {
    /// Created, updated, and deleted by the tool.
    #[default]
    Managed,
    /// Exists on every instance; updated, never created or destroyed.
    AdoptOnly,
}

/// Whether the spec claims to cover every API field of its operations.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Coverage {
    /// Every request field must be classified; the ledger fails otherwise.
    #[default]
    Full,
    /// A prototype or work in progress; unclassified fields are counted, not failed.
    Partial,
}

/// A complete kind description.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct KindSpec {
    /// The kind id and address prefix.
    pub kind: String,
    /// The document scope.
    pub scope: Scope,
    /// The containing kind, if any.
    #[serde(default)]
    pub parent: Option<String>,
    /// The key under the parent (or document root) that holds this kind's map.
    pub section: String,
    /// The first Dokploy version the kind exists in.
    #[serde(default)]
    pub since: Option<String>,
    /// A human title for diagnostics and generated docs.
    pub title: String,
    /// Creation rules.
    #[serde(default)]
    pub class: KindClass,
    /// Whether the ledger must be complete.
    #[serde(default)]
    pub coverage: Coverage,
    /// Identity and collision rules.
    pub identity: Identity,
    /// The Dokploy operations.
    pub api: Api,
    /// The configurable fields, by document name.
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
    /// How mutable fields are written.
    #[serde(default)]
    pub write: Vec<WriteGroup>,
    /// Kinds nested under this one.
    #[serde(default)]
    pub children: Vec<Child>,
    /// The Rust hook this kind needs, if any.
    #[serde(default)]
    pub hook: Option<Hook>,
    /// Deliberately unmapped API fields.
    #[serde(default)]
    pub ledger: Ledger,
}

/// Identity rules (ADR 0006).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    /// The field whose value is the natural key, or none when the document key is
    /// purely logical.
    #[serde(default)]
    pub key: Option<String>,
    /// Fields that must be unique among siblings, remotely.
    #[serde(default)]
    pub collision: Vec<String>,
    /// The address segment template, such as `registry.{key}`.
    pub address: String,
}

/// The operations of a kind, each an OpenAPI `resource.operation` id.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Api {
    /// The id field in requests and responses.
    pub id: String,
    /// Creates one.
    #[serde(default)]
    pub create: Option<Operation>,
    /// Updates one.
    #[serde(default)]
    pub update: Option<Operation>,
    /// Removes one.
    #[serde(default)]
    pub remove: Option<Operation>,
    /// Reads.
    #[serde(default)]
    pub read: Read,
    /// How a new identity is learned after create (ADR 0008).
    #[serde(default)]
    pub create_identity: Option<CreateIdentity>,
    /// Deploys, for deployable kinds (ADR 0012).
    #[serde(default)]
    pub deploy: Option<Deploy>,
}

/// One referenced operation.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    /// `resource.operation`, matching the OpenAPI `operationId` with `.` for `-`.
    pub op: String,
    /// The parameter that carries the identity, when it is not the id field.
    #[serde(default)]
    pub id_param: Option<String>,
    /// Request fields filled from context instead of configuration.
    #[serde(default)]
    pub attach: BTreeMap<String, String>,
}

/// Read operations.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Read {
    /// The authoritative collection.
    #[serde(default)]
    pub list: Option<ListRead>,
    /// The direct read.
    #[serde(default)]
    pub one: Option<OneRead>,
}

/// How absence is proven (ADR 0007).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    /// Absence from the collection proves nonexistence.
    #[default]
    Authoritative,
    /// Role filtering may hide items; absence is unknown.
    Partial,
}

/// A collection read.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ListRead {
    /// A list operation, when the collection has its own endpoint.
    #[serde(default)]
    pub op: Option<String>,
    /// Whether absence is proof.
    #[serde(default)]
    pub authority: Authority,
    /// A collection embedded in the parent's response instead.
    #[serde(default)]
    pub embedded_in: Option<Embedded>,
}

/// A collection that arrives inside another operation's response.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Embedded {
    /// The parent operation that returns it.
    pub parent_op: String,
    /// A JSON pointer to the array inside that response.
    pub pointer: String,
}

/// A direct read.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OneRead {
    /// The operation.
    pub op: String,
    /// The parameter carrying the identity.
    pub id_param: String,
    /// Whether the direct read must agree with the collection.
    #[serde(default)]
    pub agree: bool,
}

/// How the identity of a newly created resource is learned.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum CreateIdentity {
    /// Read it from the create response at a JSON pointer.
    FromResponse(String),
    /// List before and after and take the one new item matching the key fields.
    DiffCollection {
        /// The fields that identify the new item.
        key: Vec<String>,
    },
    /// Not learnable; the outcome is unknown and recovery observes.
    None,
}

/// A deploy operation.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Deploy {
    /// The operation.
    pub op: String,
}

/// Whether a value is public, secret, or content (ADR 0010).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ValueClass {
    /// Plain configuration.
    #[default]
    Public,
    /// Never stored; compared by fingerprint.
    Secret,
    /// File content; compared by digest.
    Content,
}

/// How a field may change (ADR 0004).
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Mutability {
    /// A change plans an update.
    #[default]
    InPlace,
    /// A change plans a replacement.
    CreateOnly,
    /// A change moves the resource.
    Reparent,
    /// Never read back; compared by fingerprint.
    WriteOnly,
    /// Readable, never configurable.
    Computed,
}

/// Planning granularity of a composite field.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Granularity {
    /// One property for the whole value.
    Field,
    /// One property per map key.
    Key,
}

/// One configurable field.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Field {
    /// The request/response field name or JSON pointer; defaults to the field name.
    #[serde(default)]
    pub api: Option<String>,
    /// The type expression (ADR 0004).
    #[serde(rename = "type")]
    pub ty: String,
    /// Public, secret, or content.
    #[serde(default)]
    pub class: ValueClass,
    /// How the field changes.
    #[serde(default)]
    pub mutability: Mutability,
    /// Whether `null` clears the value.
    #[serde(default)]
    pub nullable: bool,
    /// A default: `key` means the document key.
    #[serde(default)]
    pub default: Option<String>,
    /// Minimum text length.
    #[serde(default)]
    pub min_len: Option<u64>,
    /// Minimum numeric value.
    #[serde(default)]
    pub min: Option<f64>,
    /// Maximum numeric value.
    #[serde(default)]
    pub max: Option<f64>,
    /// A regular expression the text must match.
    #[serde(default)]
    pub pattern: Option<String>,
    /// Whether a change needs a deploy to take effect (ADR 0012).
    #[serde(default)]
    pub needs_deploy: bool,
    /// Whether a change restarts Traefik or the server (ADR 0013).
    #[serde(default)]
    pub disruptive: bool,
    /// Planning granularity for maps and structs.
    #[serde(default)]
    pub granularity: Option<Granularity>,
    /// Whether surrounding whitespace is ignored when comparing text.
    #[serde(default)]
    pub trim: bool,
    /// Whether the update operation replaces the whole object and so must resend
    /// this secret (ADR 0008).
    #[serde(default)]
    pub resend_on_update: bool,
    /// The first Dokploy version the field exists in.
    #[serde(default)]
    pub since: Option<String>,
    /// The last Dokploy version the field exists in.
    #[serde(default)]
    pub until: Option<String>,
    /// One line for the generated reference.
    #[serde(default)]
    pub doc: Option<String>,
    /// Members of a `struct`.
    #[serde(default)]
    pub members: BTreeMap<String, Field>,
    /// Arms of a `union`, each a set of fields.
    #[serde(default)]
    pub arms: BTreeMap<String, BTreeMap<String, Field>>,
}

/// Whether an update sends only changes or the whole object.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Shape {
    /// The id plus only the changed fields.
    #[default]
    Partial,
    /// The id plus the complete group from a fresh read, overlaid with changes.
    Full,
}

/// How a set of fields is written (ADR 0008).
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum WriteGroup {
    /// One operation carries the listed fields.
    Op {
        /// `resource.operation`.
        op: String,
        /// The fields it carries.
        fields: Vec<String>,
        /// Partial or full body.
        #[serde(default)]
        shape: Shape,
    },
    /// The operation depends on a union tag.
    ByVariant {
        /// The union field whose tag selects the operation.
        by_variant: String,
        /// One operation per arm.
        ops: BTreeMap<String, String>,
        /// Partial or full body.
        #[serde(default)]
        shape: Shape,
    },
}

/// A nested kind.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Child {
    /// The child kind.
    pub kind: String,
    /// Its section in this kind's mapping.
    pub section: String,
}

/// A Rust hook (ADR 0002).
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Hook {
    /// The registered hook name.
    pub name: String,
    /// Why the generic path is not enough.
    pub reason: String,
}

/// API fields that are deliberately not configuration.
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    /// Status, ids, and timestamps.
    #[serde(default)]
    pub readonly: Vec<String>,
    /// Operations or fields that are actions.
    #[serde(default)]
    pub action: Vec<String>,
    /// Fields ignored with a reason.
    #[serde(default)]
    pub ignored: Vec<Ignored>,
    /// Fields derived from other fields.
    #[serde(default)]
    pub derived: Vec<String>,
}

/// One ignored field and why.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ignored {
    /// The API field.
    pub field: String,
    /// Why it is not modeled.
    pub reason: String,
}

impl Field {
    /// The top-level request or response field this document field maps to.
    ///
    /// `api` may be a plain name or a JSON pointer; only the first segment names the
    /// field in a request body or query string. Defaults to the document name.
    #[must_use]
    pub fn request_name<'a>(&'a self, document_name: &'a str) -> &'a str {
        let raw = self.api.as_deref().unwrap_or(document_name);
        let trimmed = raw.strip_prefix('/').unwrap_or(raw);
        trimmed.split('/').next().unwrap_or(trimmed)
    }
}
