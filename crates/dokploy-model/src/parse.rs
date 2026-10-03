//! Parsing a document from YAML text.

use dokploy_spec::SpecRegistry;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy, Spanned};

use crate::diagnostic::{Diagnostic, DiagnosticCode, Diagnostics};
use crate::raw::Wire;
use crate::read::Reader;
use crate::{Document, Span};

/// The largest document accepted, in bytes.
pub const MAX_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

impl Document {
    /// Parses and validates a document against the kind specs.
    ///
    /// Unknown fields are rejected everywhere. YAML anchors, aliases, merge keys, tags,
    /// duplicate keys, and multiple documents are rejected, and size, node, and depth
    /// budgets apply. Every problem found is reported, in source order, and a document with
    /// any is rejected.
    pub fn parse(source: &str, registry: &SpecRegistry) -> Result<Self, Diagnostics> {
        if source.len() > MAX_DOCUMENT_BYTES {
            return Err(Diagnostics(vec![Diagnostic {
                code: DiagnosticCode::Syntax,
                path: String::new(),
                span: Span::default(),
                message: format!("the document is larger than {MAX_DOCUMENT_BYTES} bytes"),
            }]));
        }
        let wire: Spanned<Wire> =
            serde_saphyr::from_str_with_options(source, options()).map_err(|error| {
                let (line, column) = error
                    .location()
                    .map_or((0, 0), |location| (location.line(), location.column()));
                Diagnostics(vec![Diagnostic {
                    code: DiagnosticCode::Syntax,
                    path: String::new(),
                    span: Span {
                        offset: 0,
                        len: 0,
                        line: u32::try_from(line).unwrap_or(u32::MAX),
                        column: u32::try_from(column).unwrap_or(u32::MAX),
                    },
                    message: syntax_message(&error),
                }])
            })?;

        let node = Wire::into_node(wire);
        let mut reader = Reader::new(registry);
        let document = reader.document(&node);
        if reader.diagnostics.is_empty() {
            document.ok_or_else(|| {
                Diagnostics(vec![Diagnostic {
                    code: DiagnosticCode::Root,
                    path: String::new(),
                    span: node.span,
                    message: "the document could not be read".to_owned(),
                }])
            })
        } else {
            let mut diagnostics = reader.diagnostics;
            diagnostics.sort_by_key(|d| (d.span.line, d.span.column));
            Err(Diagnostics(diagnostics))
        }
    }
}

/// A YAML error's text without any excerpt of the source, which could hold a value.
fn syntax_message(error: &serde_saphyr::Error) -> String {
    let text = error.to_string();
    text.lines()
        .next()
        .unwrap_or("invalid YAML")
        .trim()
        .to_owned()
}

fn options() -> serde_saphyr::Options {
    serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::Error,
        merge_keys: MergeKeyPolicy::Error,
        strict_booleans: true,
        reject_unsupported_tags: true,
        emit_comments: false,
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_DOCUMENT_BYTES),
            max_events: 200_000,
            max_aliases: 0,
            max_anchors: 0,
            max_recorded_anchor_events: 0,
            max_recorded_anchor_bytes: 0,
            max_depth: 32,
            max_inclusion_depth: 0,
            max_documents: 1,
            max_nodes: 100_000,
            max_total_scalar_bytes: 1024 * 1024,
            max_total_comment_bytes: 1024 * 1024,
            max_merge_keys: 0,
        },
    }
}
