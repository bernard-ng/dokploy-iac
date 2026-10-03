//! Faults a test can inject: the ways a real Dokploy and its network fail.

/// What goes wrong.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FaultKind {
    /// The connection drops before the request is processed. Nothing changed, but the client
    /// cannot tell: a mutation reports an unknown outcome.
    DropBefore,
    /// Dokploy applies the request and the response is lost. A mutation reports an unknown
    /// outcome.
    DropAfter,
    /// Dokploy rejects the request definitively without applying it.
    Reject {
        /// The HTTP status.
        status: u16,
    },
    /// Dokploy applies the request, then reports a failure anyway. A registry that fails its
    /// `docker login` after saving does this (captured on 0.30.6 and 0.30.7).
    RejectAfter {
        /// The HTTP status.
        status: u16,
    },
    /// A read is answered with a server error.
    Unavailable,
    /// A mutation is acknowledged as if it had succeeded but nothing is changed.
    Swallow,
    /// A create succeeds and a second, identical object appears too, as if another client had
    /// created the same thing at the same moment.
    Duplicate,
}

/// One injected fault: an operation, optionally a specific call of it, and what happens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fault {
    pub(crate) operation: String,
    pub(crate) call: Option<usize>,
    pub(crate) kind: FaultKind,
}

impl Fault {
    /// Fails the next call of `operation`.
    #[must_use]
    pub fn new(operation: impl Into<String>, kind: FaultKind) -> Self {
        Self {
            operation: operation.into(),
            call: None,
            kind,
        }
    }

    /// Restricts the fault to the `n`th call of the operation, counting from 1 across the
    /// simulator's life.
    #[must_use]
    pub fn on_call(mut self, n: usize) -> Self {
        self.call = Some(n);
        self
    }
}
