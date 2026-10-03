//! The typed values of a document.

use std::collections::BTreeMap;

use crate::Span;

/// Where a secret or content value comes from (ADR 0010). A literal is never one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// A process environment variable.
    Env(String),
    /// A file, relative to the workspace.
    File(String),
    /// A Dokploy Vault secret, resolved by Dokploy; the value never reaches this tool.
    Vault {
        /// The vault provider's name.
        provider: String,
        /// The secret's name in that provider.
        secret: String,
    },
}

/// A resource outside the document, named so it can be resolved against fresh remote state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selector {
    /// An exact remote name.
    Name(String),
    /// The Dokploy host itself (for servers only).
    Local,
}

/// One environment variable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvValue {
    /// A literal value.
    Public(String),
    /// A value read from a source; it is secret.
    Secret(Source),
}

/// A value of a field, as its spec type describes it.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// `null`: the remote value is cleared.
    Null,
    /// A boolean.
    Bool(bool),
    /// An integer.
    Int(i64),
    /// A finite number.
    Number(f64),
    /// Text. Also an enum value or a reference to another resource.
    Text(String),
    /// A list, or a set in canonical (sorted) order.
    List(Vec<Value>),
    /// A map from text to value, or a struct's members.
    Map(BTreeMap<String, Value>),
    /// A tagged union: the active arm and its fields.
    Union {
        /// The arm named by the tag field.
        tag: String,
        /// The arm's fields.
        fields: BTreeMap<String, Value>,
    },
    /// An environment block.
    Env(BTreeMap<String, EnvValue>),
    /// A secret or content source.
    Source(Source),
    /// A resource outside the document.
    Selector(Selector),
    /// Opaque JSON.
    Blob(serde_json::Value),
}

/// One field with its position.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The value.
    pub value: Value,
    /// Where the field's key is written.
    pub span: Span,
}
