use std::path::PathBuf;

use dokploy_sdk::ImperativeRequest;
use miette::Diagnostic;
use serde_json::{Map, Number, Value};
use thiserror::Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OperationMethod {
    Get,
    Post,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum BodyKind {
    Json,
    Multipart,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum InputKind {
    String,
    Number,
    Integer,
    Boolean,
    Array,
    Object,
    Json,
    File,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InputSpec {
    pub wire_name: &'static str,
    pub cli_name: &'static str,
    pub kind: InputKind,
    pub required: bool,
    pub nullable: bool,
    pub enum_values: &'static [&'static str],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct OperationSpec {
    pub resource: &'static str,
    pub command: &'static str,
    pub operation_id: &'static str,
    pub path: &'static str,
    pub method: OperationMethod,
    pub query: &'static [InputSpec],
    pub body_kind: Option<BodyKind>,
    pub body: &'static [InputSpec],
}

/// One parsed imperative operation ready for the SDK transport.
pub struct ImperativeInvocation {
    operation: &'static OperationSpec,
    request: ImperativeRequest,
}

impl ImperativeInvocation {
    #[must_use]
    pub fn operation_id(&self) -> &'static str {
        self.operation.operation_id
    }

    #[must_use]
    pub fn path(&self) -> &'static str {
        self.operation.path
    }

    #[must_use]
    pub fn into_request(self) -> ImperativeRequest {
        self.request
    }
}

#[derive(Debug, Diagnostic, Error)]
pub enum ImperativeInputError {
    #[error("invalid value for `--{argument}`; expected {expected}")]
    InvalidValue {
        argument: &'static str,
        expected: &'static str,
    },

    #[error("`--body-json` must contain a JSON object")]
    RawBodyNotObject,

    #[error("generated command input `{wire_name}` is missing from the operation catalog")]
    GeneratedInputMismatch { wire_name: &'static str },
}

pub(crate) fn build_invocation(
    operation: &'static OperationSpec,
    raw_body: Option<String>,
    query_values: Vec<(&'static str, Option<String>)>,
    body_values: Vec<(&'static str, Option<String>)>,
) -> Result<ImperativeInvocation, ImperativeInputError> {
    let mut request = match operation.method {
        OperationMethod::Get => ImperativeRequest::get(operation.path),
        OperationMethod::Post => ImperativeRequest::post(operation.path),
    };

    for (wire_name, raw_value) in query_values {
        let Some(raw_value) = raw_value else {
            continue;
        };
        let input = find_input(operation.query, wire_name)?;
        let value = parse_value(input, &raw_value)?;
        if value.is_null() {
            return Err(ImperativeInputError::InvalidValue {
                argument: input.cli_name,
                expected: "a non-null query value (omit the option for no value)",
            });
        }
        request = request.query(input.wire_name, value);
    }

    match operation.body_kind {
        None => {}
        Some(BodyKind::Json) => {
            let body = match raw_body {
                Some(raw_body) => parse_raw_body(&raw_body)?,
                None => build_json_body(operation.body, body_values)?,
            };
            request = request.body(Value::Object(body));
        }
        Some(BodyKind::Multipart) => {
            for (wire_name, raw_value) in body_values {
                let Some(raw_value) = raw_value else {
                    continue;
                };
                let input = find_input(operation.body, wire_name)?;
                request = match input.kind {
                    InputKind::File => {
                        request.multipart_file(input.wire_name, PathBuf::from(raw_value))
                    }
                    _ => request.multipart_text(input.wire_name, raw_value),
                };
            }
        }
    }

    Ok(ImperativeInvocation { operation, request })
}

fn find_input(
    inputs: &'static [InputSpec],
    wire_name: &'static str,
) -> Result<&'static InputSpec, ImperativeInputError> {
    inputs
        .iter()
        .find(|input| input.wire_name == wire_name)
        .ok_or(ImperativeInputError::GeneratedInputMismatch { wire_name })
}

fn parse_raw_body(raw_body: &str) -> Result<Map<String, Value>, ImperativeInputError> {
    serde_json::from_str::<Value>(raw_body)
        .ok()
        .and_then(|value| value.as_object().cloned())
        .ok_or(ImperativeInputError::RawBodyNotObject)
}

fn build_json_body(
    inputs: &'static [InputSpec],
    values: Vec<(&'static str, Option<String>)>,
) -> Result<Map<String, Value>, ImperativeInputError> {
    let mut body = Map::new();
    for (wire_name, raw_value) in values {
        let Some(raw_value) = raw_value else {
            continue;
        };
        let input = find_input(inputs, wire_name)?;
        body.insert(input.wire_name.to_owned(), parse_value(input, &raw_value)?);
    }

    Ok(body)
}

fn parse_value(input: &InputSpec, raw: &str) -> Result<Value, ImperativeInputError> {
    if !input.enum_values.is_empty() && !input.enum_values.contains(&raw) {
        return Err(ImperativeInputError::InvalidValue {
            argument: input.cli_name,
            expected: "one of the allowed values shown in `--help`",
        });
    }
    if input.nullable && raw == "null" {
        return Ok(Value::Null);
    }

    let parsed = match input.kind {
        InputKind::String => Some(Value::String(raw.to_owned())),
        InputKind::Number => raw.parse::<Number>().ok().map(Value::Number),
        InputKind::Integer => raw
            .parse::<Number>()
            .ok()
            .filter(|number| number.is_i64() || number.is_u64())
            .map(Value::Number),
        InputKind::Boolean => raw.parse::<bool>().ok().map(Value::Bool),
        InputKind::Array => parse_json_kind(raw, Value::is_array),
        InputKind::Object => parse_json_kind(raw, Value::is_object),
        InputKind::Json => serde_json::from_str(raw).ok(),
        InputKind::File => None,
    };

    parsed.ok_or(ImperativeInputError::InvalidValue {
        argument: input.cli_name,
        expected: expected_value(input.kind),
    })
}

fn parse_json_kind(raw: &str, predicate: impl FnOnce(&Value) -> bool) -> Option<Value> {
    serde_json::from_str(raw)
        .ok()
        .filter(|value| predicate(value))
}

fn expected_value(kind: InputKind) -> &'static str {
    match kind {
        InputKind::String => "a string",
        InputKind::Number => "a JSON number",
        InputKind::Integer => "a JSON integer",
        InputKind::Boolean => "`true` or `false`",
        InputKind::Array => "a JSON array",
        InputKind::Object => "a JSON object",
        InputKind::Json => "valid JSON",
        InputKind::File => "a file path",
    }
}

#[cfg(test)]
mod tests {
    use super::super::imperative_generated::OPERATIONS;

    #[test]
    fn generated_catalog_contains_every_pinned_operation() {
        assert_eq!(OPERATIONS.len(), 604);
        assert!(OPERATIONS.iter().any(|operation| {
            operation.resource == "application"
                && operation.command == "create"
                && operation.path == "application.create"
        }));
        assert!(OPERATIONS.iter().any(|operation| {
            operation.resource == "project"
                && operation.command == "all"
                && operation.path == "project.all"
        }));
    }
}
