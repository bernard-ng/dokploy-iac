//! Kind specs: the single source of truth for every Dokploy resource kind.
//!
//! A spec describes a kind completely as data (identity, operations, fields, write
//! groups, children). This crate parses and lints specs, cross-checks them as a
//! registry, and verifies them against the Dokploy OpenAPI document (the coverage
//! ledger). It performs no I/O against Dokploy and has no engine dependency.
//!
//! The grammar is documented in `docs/design/spec-format.md`.

mod error;
mod ledger;
mod load;
mod model;
mod paths;
mod registry;
mod types;
mod validate;
mod versions;

pub use error::{Issue, LoadError};
pub use ledger::{KindLedger, LedgerReport, OperationIndex, check_ledger};
pub use load::{embedded, load_dir, parse_spec};
pub use model::{
    Api, Authority, Child, Coverage, CreateIdentity, Deploy, Embedded, Field, Granularity, Hook,
    Identity, Ignored, KindClass, KindSpec, Ledger, ListRead, ListScope, Membership, MembershipOp,
    Mutability, OneRead, Operation, Read, Scope, Shape, ValueClass, WriteGroup,
};
pub use paths::{PathError, PathShape, PropertyInfo, ValueRules};
pub use registry::SpecRegistry;
pub use types::{FieldType, TypeError, parse_type};
pub use validate::validate_spec;
pub use versions::{DokployVersion, VersionStatus, Versions};
