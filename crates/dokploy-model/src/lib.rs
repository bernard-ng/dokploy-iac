//! Documents: parse, validate, render, and describe them by kind specs (ADR 0005).
//!
//! A *document* is one YAML file with `version: 2` and one root, `project:` or
//! `settings:`. What a document may contain is decided entirely by the kind specs in
//! `specs/`: this crate has no knowledge of any kind. [`Document::parse`] validates a
//! document and reports every problem with its position; [`Document::render`] writes the
//! canonical text; [`json_schema`] describes the format for editors.

mod diagnostic;
mod document;
mod parse;
mod raw;
mod read;
mod render;
mod schema;
mod span;
mod value;

pub use diagnostic::{Diagnostic, DiagnosticCode, Diagnostics};
pub use document::{Document, Lifecycle, Resource, Root, Sections};
pub use parse::MAX_DOCUMENT_BYTES;
pub use read::FORMAT_VERSION;
pub use schema::json_schema;
pub use span::Span;
pub use value::{EnvValue, Field, Selector, Source, Value};
