//! Problems found in a document, each with a code, a position, and a path.

use std::fmt;

use thiserror::Error;

use crate::Span;

/// What kind of problem a diagnostic reports. Codes are stable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticCode {
    /// The text is not valid YAML or exceeds a budget.
    Syntax,
    /// The `version` field is missing or not 2.
    Version,
    /// The document root is not one of the supported shapes.
    Root,
    /// A field, section, or member that the kind does not have.
    UnknownField,
    /// A value of the wrong shape.
    Type,
    /// A value of the right shape that breaks a rule (range, length, enum, pattern).
    Value,
    /// A resource key or collection key that is not allowed.
    Key,
    /// A secret or content field written as a literal instead of a source.
    SourceRequired,
    /// A source (`env`, `file`, `vault`) that is malformed.
    Source,
    /// `null` where the field cannot be cleared.
    Null,
    /// A duplicate entry where entries must be distinct.
    Duplicate,
    /// A `lifecycle` or `depends_on` entry that is malformed.
    Directive,
}

impl DiagnosticCode {
    /// The stable code, such as `DOKDOC004`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Syntax => "DOKDOC001",
            Self::Version => "DOKDOC002",
            Self::Root => "DOKDOC003",
            Self::UnknownField => "DOKDOC004",
            Self::Type => "DOKDOC005",
            Self::Value => "DOKDOC006",
            Self::Key => "DOKDOC007",
            Self::SourceRequired => "DOKDOC008",
            Self::Source => "DOKDOC009",
            Self::Null => "DOKDOC010",
            Self::Duplicate => "DOKDOC011",
            Self::Directive => "DOKDOC012",
        }
    }
}

impl fmt::Display for DiagnosticCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// One problem in one place.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    /// What is wrong, as a stable code.
    pub code: DiagnosticCode,
    /// Where, as a dotted path through the document (`settings.registries.ghcr.url`).
    pub path: String,
    /// Where, in the source text.
    pub span: Span,
    /// A sentence saying what is wrong and, where possible, what is allowed.
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}: {}: {}",
            self.span.line, self.span.column, self.code, self.path, self.message
        )
    }
}

/// Every problem found in one document. A document with any is rejected.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n"))]
pub struct Diagnostics(pub Vec<Diagnostic>);

impl Diagnostics {
    /// The diagnostics in source order.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.0.iter()
    }

    /// How many problems were found.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether there are none.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}
