//! The generic YAML tree with a span on every node.

use std::fmt;

use serde::Deserialize;
use serde::de::{self, MapAccess, SeqAccess, Visitor};
use serde_saphyr::Spanned;

use crate::Span;

/// A YAML value with the position of every node.
#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub value: Raw,
    pub span: Span,
}

/// A mapping key and its position.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    pub name: String,
    pub span: Span,
}

/// One YAML value.
#[derive(Clone, Debug, PartialEq)]
pub enum Raw {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Seq(Vec<Node>),
    Map(Vec<(Key, Node)>),
}

impl Raw {
    /// A short word for what this value is, for diagnostics.
    pub fn describe(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "a boolean",
            Self::Int(_) => "an integer",
            Self::Float(_) => "a number",
            Self::Text(_) => "text",
            Self::Seq(_) => "a list",
            Self::Map(_) => "a mapping",
        }
    }
}

fn span_of(location: &serde_saphyr::Location) -> Span {
    let span = location.span();
    Span {
        offset: usize::try_from(span.byte_offset().unwrap_or_else(|| span.offset())).unwrap_or(0),
        len: usize::try_from(span.byte_len().unwrap_or_else(|| span.len())).unwrap_or(0),
        line: u32::try_from(location.line()).unwrap_or(u32::MAX),
        column: u32::try_from(location.column()).unwrap_or(u32::MAX),
    }
}

/// The serde form: a value whose nested values and keys are `Spanned`.
pub enum Wire {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Seq(Vec<Spanned<Wire>>),
    Map(Vec<(Spanned<String>, Spanned<Wire>)>),
}

impl Wire {
    pub fn into_node(wire: Spanned<Self>) -> Node {
        let span = span_of(&wire.referenced);
        let value = match wire.value {
            Self::Null => Raw::Null,
            Self::Bool(value) => Raw::Bool(value),
            Self::Int(value) => Raw::Int(value),
            Self::Float(value) => Raw::Float(value),
            Self::Text(value) => Raw::Text(value),
            Self::Seq(items) => Raw::Seq(items.into_iter().map(Self::into_node).collect()),
            Self::Map(entries) => Raw::Map(
                entries
                    .into_iter()
                    .map(|(key, value)| {
                        (
                            Key {
                                span: span_of(&key.referenced),
                                name: key.value,
                            },
                            Self::into_node(value),
                        )
                    })
                    .collect(),
            ),
        };
        Node { value, span }
    }
}

impl<'de> Deserialize<'de> for Wire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct WireVisitor;

        impl<'de> Visitor<'de> for WireVisitor {
            type Value = Wire;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("any YAML value")
            }

            fn visit_unit<E: de::Error>(self) -> Result<Wire, E> {
                Ok(Wire::Null)
            }

            fn visit_none<E: de::Error>(self) -> Result<Wire, E> {
                Ok(Wire::Null)
            }

            fn visit_some<D: serde::Deserializer<'de>>(
                self,
                deserializer: D,
            ) -> Result<Wire, D::Error> {
                deserializer.deserialize_any(self)
            }

            fn visit_bool<E: de::Error>(self, value: bool) -> Result<Wire, E> {
                Ok(Wire::Bool(value))
            }

            fn visit_i64<E: de::Error>(self, value: i64) -> Result<Wire, E> {
                Ok(Wire::Int(value))
            }

            fn visit_u64<E: de::Error>(self, value: u64) -> Result<Wire, E> {
                i64::try_from(value)
                    .map(Wire::Int)
                    .map_err(|_| E::custom("integer is too large"))
            }

            fn visit_f64<E: de::Error>(self, value: f64) -> Result<Wire, E> {
                Ok(Wire::Float(value))
            }

            fn visit_str<E: de::Error>(self, value: &str) -> Result<Wire, E> {
                Ok(Wire::Text(value.to_owned()))
            }

            fn visit_string<E: de::Error>(self, value: String) -> Result<Wire, E> {
                Ok(Wire::Text(value))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Wire, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Spanned<Wire>>()? {
                    items.push(item);
                }
                Ok(Wire::Seq(items))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Wire, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<Spanned<String>, Spanned<Wire>>()? {
                    entries.push(entry);
                }
                Ok(Wire::Map(entries))
            }
        }

        deserializer.deserialize_any(WireVisitor)
    }
}
