use serde_json::{Map, Value};

const REDACTED: &str = "[REDACTED]";
const SENSITIVE_KEY_SUFFIXES: &[&str] = &[
    "password",
    "apikey",
    "accesskey",
    "privatekey",
    "secret",
    "buildsecrets",
    "token",
    "refreshtoken",
    "env",
    "previewenv",
    "buildargs",
    "previewbuildargs",
];

pub(crate) fn redact_response(response: Value) -> Value {
    match response {
        Value::Object(fields) => Value::Object(redact_object(fields)),
        Value::Array(values) => Value::Array(values.into_iter().map(redact_response).collect()),
        value => value,
    }
}

fn redact_object(fields: Map<String, Value>) -> Map<String, Value> {
    fields
        .into_iter()
        .map(|(key, value)| {
            let value = if is_sensitive_key(&key) {
                Value::String(REDACTED.to_owned())
            } else {
                redact_response(value)
            };

            (key, value)
        })
        .collect()
}

fn is_sensitive_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase();

    SENSITIVE_KEY_SUFFIXES
        .iter()
        .any(|suffix| normalized.ends_with(suffix))
}
