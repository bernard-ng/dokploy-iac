//! The field type expression language (ADR 0004).

use thiserror::Error;

/// A parsed type expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FieldType {
    /// Text.
    Text,
    /// Integer.
    Int,
    /// Any number.
    Number,
    /// Boolean.
    Bool,
    /// An environment block.
    Env,
    /// Content from a workspace file.
    File,
    /// A closed set of text values.
    Enum(Vec<String>),
    /// An ordered list.
    List(Box<FieldType>),
    /// An unordered set.
    Set(Box<FieldType>),
    /// A map from text to a value type.
    Map(Box<FieldType>),
    /// A struct whose members are declared beside it.
    Struct,
    /// A tagged union whose tag has the given field name.
    Union {
        /// The name of the discriminating field.
        tag: String,
    },
    /// Opaque JSON validated against a named JSON Schema.
    Blob(String),
    /// A reference to another resource in the document.
    Ref(String),
    /// A name resolved against fresh remote state.
    Selector(String),
    /// A shared type defined in `specs/types.yaml`.
    Shared(String),
}

impl FieldType {
    /// The kind a set of selectors selects (`set<selector(network)>`), or a map from a key to such
    /// sets (`map<text, set<selector(network)>>`) selects: the types planned per member, each
    /// resource an entry of its own.
    #[must_use]
    pub fn selector_set_kind(&self) -> Option<&str> {
        match self {
            Self::Set(item) => match &**item {
                Self::Selector(kind) => Some(kind),
                _ => None,
            },
            Self::Map(item) => item
                .selector_set_kind()
                .filter(|_| matches!(**item, Self::Set(_))),
            _ => None,
        }
    }

    /// Whether this is a map from a key to a set of selectors.
    #[must_use]
    pub fn is_map_of_selector_sets(&self) -> bool {
        matches!(self, Self::Map(item) if matches!(**item, Self::Set(_)) && item.selector_set_kind().is_some())
    }
}

/// A malformed type expression.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("invalid type `{expression}`: {reason}")]
pub struct TypeError {
    /// The text that failed to parse.
    pub expression: String,
    /// What is wrong.
    pub reason: &'static str,
}

fn error(expression: &str, reason: &'static str) -> TypeError {
    TypeError {
        expression: expression.to_owned(),
        reason,
    }
}

fn is_identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters.next().is_some_and(|c| c.is_ascii_lowercase())
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

fn is_enum_value(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | '@'))
}

fn inner(text: &str, open: char, close: char) -> Option<&str> {
    let rest = text.strip_suffix(close)?;
    let (_, inside) = rest.split_once(open)?;
    Some(inside)
}

/// Splits `a, b<c, d>, e` at top-level commas.
fn split_top_level(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0_i32;
    let mut start = 0;
    for (index, character) in text.char_indices() {
        match character {
            '<' | '[' | '(' => depth += 1,
            '>' | ']' | ')' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(text[start..index].trim());
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim());
    parts
}

/// Parses a type expression.
pub fn parse_type(expression: &str) -> Result<FieldType, TypeError> {
    let text = expression.trim();
    match text {
        "text" => return Ok(FieldType::Text),
        "int" => return Ok(FieldType::Int),
        "number" => return Ok(FieldType::Number),
        "bool" => return Ok(FieldType::Bool),
        "env" => return Ok(FieldType::Env),
        "file" => return Ok(FieldType::File),
        "struct" => return Ok(FieldType::Struct),
        _ => {}
    }

    if let Some(values) = text.strip_prefix("enum[").and_then(|r| r.strip_suffix(']')) {
        let values: Vec<String> = values.split(',').map(|v| v.trim().to_owned()).collect();
        if values.iter().any(|value| !is_enum_value(value)) {
            return Err(error(
                expression,
                "enum values must be non-empty identifiers",
            ));
        }
        let mut sorted = values.clone();
        sorted.sort();
        sorted.dedup();
        if sorted.len() != values.len() {
            return Err(error(expression, "enum values must be distinct"));
        }
        return Ok(FieldType::Enum(values));
    }
    if let Some(item) = text.strip_prefix("list<").and_then(|r| r.strip_suffix('>')) {
        return Ok(FieldType::List(Box::new(parse_type(item)?)));
    }
    if let Some(item) = text.strip_prefix("set<").and_then(|r| r.strip_suffix('>')) {
        return Ok(FieldType::Set(Box::new(parse_type(item)?)));
    }
    if let Some(body) = text.strip_prefix("map<").and_then(|r| r.strip_suffix('>')) {
        let parts = split_top_level(body);
        if parts.len() != 2 || parts[0] != "text" {
            return Err(error(expression, "a map is written map<text, V>"));
        }
        return Ok(FieldType::Map(Box::new(parse_type(parts[1])?)));
    }
    for (prefix, build) in [
        (
            "union(",
            (|name: String| FieldType::Union { tag: name }) as fn(String) -> FieldType,
        ),
        ("blob(", FieldType::Blob as fn(String) -> FieldType),
        ("ref(", FieldType::Ref as fn(String) -> FieldType),
        ("selector(", FieldType::Selector as fn(String) -> FieldType),
    ] {
        if text.starts_with(prefix) {
            let name = inner(text, '(', ')')
                .ok_or_else(|| error(expression, "missing closing parenthesis"))?;
            if !is_identifier(name) {
                return Err(error(
                    expression,
                    "expected a lower-case identifier in parentheses",
                ));
            }
            return Ok(build(name.to_owned()));
        }
    }
    if is_identifier(text) {
        return Ok(FieldType::Shared(text.to_owned()));
    }
    Err(error(expression, "unrecognised type"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_form() {
        assert_eq!(parse_type("text"), Ok(FieldType::Text));
        assert_eq!(
            parse_type("enum[push, tag]"),
            Ok(FieldType::Enum(vec!["push".into(), "tag".into()]))
        );
        assert_eq!(
            parse_type("list<set<text>>"),
            Ok(FieldType::List(Box::new(FieldType::Set(Box::new(
                FieldType::Text
            )))))
        );
        assert_eq!(
            parse_type("map<text, list<int>>"),
            Ok(FieldType::Map(Box::new(FieldType::List(Box::new(
                FieldType::Int
            )))))
        );
        assert_eq!(
            parse_type("union(type)"),
            Ok(FieldType::Union { tag: "type".into() })
        );
        assert_eq!(
            parse_type("selector(server)"),
            Ok(FieldType::Selector("server".into()))
        );
        assert_eq!(
            parse_type("ref(ssh_key)"),
            Ok(FieldType::Ref("ssh_key".into()))
        );
        assert_eq!(parse_type("swarm"), Ok(FieldType::Shared("swarm".into())));
    }

    #[test]
    fn rejects_malformed_expressions() {
        for bad in [
            "",
            "Text",
            "enum[]",
            "enum[a, a]",
            "map<int, text>",
            "list<",
            "ref()",
            "union(Type)",
            "a b",
        ] {
            assert!(parse_type(bad).is_err(), "{bad:?} should be rejected");
        }
    }
}
