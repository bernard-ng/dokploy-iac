//! Resolution of external server, registry, and backup-destination selectors.
//!
//! Servers, registries, and destinations are external infrastructure. The
//! workspace stores only a stable local-or-named selector; this seam resolves
//! it against a fresh, minimal, bounded `*.all` collection read. Zero or
//! multiple exact-name matches never select a record. Resolved physical
//! identities stay inside non-serializable types with redacted `Debug` output
//! and are bound into saved plans only through keyed receipts.
//!
//! The directory is deliberately kind-agnostic so Backup destinations and
//! Schedule server scopes reuse the same resolution seam.

use std::collections::BTreeSet;
use std::fmt;

use dokploy_config::{ExternalSelector, SelectorKind};
use dokploy_core::{
    ComparableValue, ExternalResolution, PropertyObservation, PropertyUnknownReason,
    RemoteFailureKind,
};
use dokploy_sdk::{Dokploy, Error as SdkError, ResponseField};
use dokploy_state::RemoteId;

/// One external record reduced to the only fields selector resolution needs.
#[derive(Clone)]
struct ExternalRecord {
    id: String,
    name: String,
}

impl fmt::Debug for ExternalRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExternalRecord([REDACTED])")
    }
}

type Collection = Result<Vec<ExternalRecord>, RemoteFailureKind>;

/// Fresh minimal external collections, read at most once per discovery.
///
/// A collection that was not required is `None`, so configurations without
/// selectors perform no additional requests.
#[derive(Default)]
pub struct ExternalDirectory {
    servers: Option<Collection>,
    registries: Option<Collection>,
    destinations: Option<Collection>,
}

impl fmt::Debug for ExternalDirectory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ExternalDirectory([REDACTED])")
    }
}

impl ExternalDirectory {
    /// Reads exactly the requested collections from the authoritative `*.all` endpoints.
    pub async fn load(client: &Dokploy, kinds: &BTreeSet<SelectorKind>) -> Self {
        let mut directory = Self::default();
        if kinds.contains(&SelectorKind::Server) {
            directory.servers = Some(
                client
                    .servers()
                    .all()
                    .await
                    .map(|collection| {
                        collection
                            .servers()
                            .iter()
                            .map(|server| ExternalRecord {
                                id: server.server_id.as_str().to_owned(),
                                name: server.name.clone(),
                            })
                            .collect()
                    })
                    .map_err(|error| classify(&error)),
            );
        }
        if kinds.contains(&SelectorKind::Registry) {
            directory.registries = Some(
                client
                    .registries()
                    .all()
                    .await
                    .map(|collection| {
                        collection
                            .registries()
                            .iter()
                            .map(|registry| ExternalRecord {
                                id: registry.registry_id.as_str().to_owned(),
                                name: registry.registry_name.clone(),
                            })
                            .collect()
                    })
                    .map_err(|error| classify(&error)),
            );
        }
        if kinds.contains(&SelectorKind::Destination) {
            directory.destinations = Some(
                client
                    .destinations()
                    .all()
                    .await
                    .map(|collection| {
                        collection
                            .destinations()
                            .iter()
                            .map(|destination| ExternalRecord {
                                id: destination.destination_id.as_str().to_owned(),
                                name: destination.name.clone(),
                            })
                            .collect()
                    })
                    .map_err(|error| classify(&error)),
            );
        }

        directory
    }

    fn collection(&self, kind: SelectorKind) -> Option<&Collection> {
        match kind {
            SelectorKind::Server => self.servers.as_ref(),
            SelectorKind::Registry => self.registries.as_ref(),
            SelectorKind::Destination => self.destinations.as_ref(),
        }
    }

    /// Resolves one configured selector by exact, case-sensitive name.
    pub fn resolve(&self, kind: SelectorKind, selector: &ExternalSelector) -> ExternalResolution {
        let name = match selector {
            ExternalSelector::Local if kind.allows_local() => return ExternalResolution::Local,
            ExternalSelector::Local => {
                return ExternalResolution::Unavailable(RemoteFailureKind::InvalidResponse);
            }
            ExternalSelector::Named(name) => name.as_str(),
        };
        let records = match self.collection(kind) {
            Some(Ok(records)) => records,
            Some(Err(failure)) => return ExternalResolution::Unavailable(*failure),
            None => return ExternalResolution::Unavailable(RemoteFailureKind::InvalidResponse),
        };
        let mut matches = records.iter().filter(|record| record.name == name);
        match (matches.next(), matches.next()) {
            (None, _) => ExternalResolution::Unmatched,
            (Some(_), Some(_)) => ExternalResolution::Ambiguous,
            (Some(record), None) => RemoteId::new(&record.id).map_or(
                ExternalResolution::Unavailable(RemoteFailureKind::InvalidResponse),
                ExternalResolution::Resolved,
            ),
        }
    }

    /// Maps one observed physical association back to its stable name selector.
    ///
    /// Duplicate names are preserved here: the observation stays truthful and
    /// the desired selector's own resolution reports the ambiguity. An identity
    /// absent from the fresh collection, or a name outside the selector
    /// grammar, is an unknown observation and blocks planning.
    ///
    /// An explicit `null` is the local server only for primary server placement
    /// (`null_is_local`); every other nullable association observes absence.
    pub fn observe_association(
        &self,
        kind: SelectorKind,
        null_is_local: bool,
        field: ResponseField<&str>,
    ) -> PropertyObservation {
        match field {
            ResponseField::NotReturned => {
                PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
            }
            ResponseField::Null if null_is_local && kind.allows_local() => {
                PropertyObservation::Known(selector_value(&ExternalSelector::Local))
            }
            ResponseField::Null => PropertyObservation::KnownAbsent,
            ResponseField::Value(id) => match self.name_of(kind, id) {
                Some(name) => {
                    PropertyObservation::Known(selector_value(&ExternalSelector::named(name)))
                }
                None => PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse),
            },
        }
    }

    /// Returns the grammar-valid name of one physical identity, if it is present.
    pub fn name_of(&self, kind: SelectorKind, id: &str) -> Option<&str> {
        let Some(Ok(records)) = self.collection(kind) else {
            return None;
        };
        let mut matches = records.iter().filter(|record| record.id == id);
        let record = matches.next()?;
        if matches.next().is_some() || !valid_name(&record.name) {
            return None;
        }

        Some(record.name.as_str())
    }

    /// Returns the exact name of an identity only when that name selects it uniquely.
    ///
    /// Import uses this stricter form: a name shared by another record could not
    /// be written back as a stable selector.
    pub fn unique_name_of(&self, kind: SelectorKind, id: &str) -> Option<&str> {
        let name = self.name_of(kind, id)?;
        let Some(Ok(records)) = self.collection(kind) else {
            return None;
        };
        (records.iter().filter(|record| record.name == name).count() == 1).then_some(name)
    }
}

/// Builds the stable JSON representation stored in desired state and plans' checkpoints.
pub fn selector_value(selector: &ExternalSelector) -> ComparableValue {
    ComparableValue::try_from_json(match selector {
        ExternalSelector::Local => serde_json::json!({ "local": true }),
        ExternalSelector::Named(name) => serde_json::json!({ "name": name.as_str() }),
    })
    .expect("selector objects are never null")
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= dokploy_config::ExternalName::MAX_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
}

fn classify(error: &SdkError) -> RemoteFailureKind {
    match error {
        SdkError::Api(error) if matches!(error.status(), 401 | 403) => {
            RemoteFailureKind::Unauthorized
        }
        SdkError::Api(error)
            if matches!(error.status(), 408 | 425 | 429) || error.status() >= 500 =>
        {
            RemoteFailureKind::Unavailable
        }
        SdkError::Request { .. } | SdkError::OutcomeUnknown { .. } => {
            RemoteFailureKind::Unavailable
        }
        SdkError::InvalidRequest { .. }
        | SdkError::Api(_)
        | SdkError::UnexpectedResponse { .. }
        | SdkError::Decode { .. } => RemoteFailureKind::InvalidResponse,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(kind: SelectorKind, rows: &[(&str, &str)]) -> ExternalDirectory {
        let records: Vec<_> = rows
            .iter()
            .map(|(id, name)| ExternalRecord {
                id: (*id).to_owned(),
                name: (*name).to_owned(),
            })
            .collect();
        let mut directory = ExternalDirectory::default();
        match kind {
            SelectorKind::Server => directory.servers = Some(Ok(records)),
            SelectorKind::Registry => directory.registries = Some(Ok(records)),
            SelectorKind::Destination => directory.destinations = Some(Ok(records)),
        }
        directory
    }

    fn named(name: &str) -> ExternalSelector {
        ExternalSelector::named(name)
    }

    #[test]
    fn resolution_requires_exactly_one_exact_name_match() {
        let directory = directory(
            SelectorKind::Registry,
            &[
                ("registry-1", "main"),
                ("registry-2", "Main"),
                ("registry-3", "dup"),
                ("registry-4", "dup"),
            ],
        );

        assert_eq!(
            directory.resolve(SelectorKind::Registry, &named("main")),
            ExternalResolution::Resolved(RemoteId::new("registry-1").unwrap())
        );
        assert_eq!(
            directory.resolve(SelectorKind::Registry, &named("Main")),
            ExternalResolution::Resolved(RemoteId::new("registry-2").unwrap())
        );
        assert_eq!(
            directory.resolve(SelectorKind::Registry, &named("missing")),
            ExternalResolution::Unmatched
        );
        assert_eq!(
            directory.resolve(SelectorKind::Registry, &named("dup")),
            ExternalResolution::Ambiguous
        );
    }

    #[test]
    fn local_resolves_only_for_servers_and_missing_collections_are_unavailable() {
        let directory = directory(SelectorKind::Server, &[("server-1", "edge")]);

        assert_eq!(
            directory.resolve(SelectorKind::Server, &ExternalSelector::Local),
            ExternalResolution::Local
        );
        assert_eq!(
            directory.resolve(SelectorKind::Registry, &ExternalSelector::Local),
            ExternalResolution::Unavailable(RemoteFailureKind::InvalidResponse)
        );
        assert_eq!(
            directory.resolve(SelectorKind::Registry, &named("main")),
            ExternalResolution::Unavailable(RemoteFailureKind::InvalidResponse),
            "an unread collection never resolves a name"
        );
        let failed = ExternalDirectory {
            destinations: Some(Err(RemoteFailureKind::Unauthorized)),
            ..ExternalDirectory::default()
        };
        assert_eq!(
            failed.resolve(SelectorKind::Destination, &named("bucket")),
            ExternalResolution::Unavailable(RemoteFailureKind::Unauthorized)
        );
    }

    #[test]
    fn observed_associations_map_to_truthful_name_selectors() {
        let directory = directory(
            SelectorKind::Server,
            &[
                ("server-1", "edge"),
                ("server-2", "dup"),
                ("server-3", "dup"),
                ("server-4", " padded"),
            ],
        );
        let name =
            |name: &str| PropertyObservation::Known(selector_value(&ExternalSelector::named(name)));

        assert_eq!(
            directory.observe_association(
                SelectorKind::Server,
                true,
                ResponseField::Value("server-1")
            ),
            name("edge")
        );
        // Duplicate names stay observable; the desired selector reports the ambiguity.
        assert_eq!(
            directory.observe_association(
                SelectorKind::Server,
                true,
                ResponseField::Value("server-2")
            ),
            name("dup")
        );
        assert_eq!(
            directory.observe_association(SelectorKind::Server, true, ResponseField::Null::<&str>),
            PropertyObservation::Known(selector_value(&ExternalSelector::Local))
        );
        assert_eq!(
            directory.observe_association(SelectorKind::Server, false, ResponseField::Null::<&str>),
            PropertyObservation::KnownAbsent
        );
        for field in [
            ResponseField::Value("server-9"),
            ResponseField::Value("server-4"),
        ] {
            assert_eq!(
                directory.observe_association(SelectorKind::Server, true, field),
                PropertyObservation::Unknown(PropertyUnknownReason::InvalidResponse)
            );
        }
        assert_eq!(
            directory.observe_association(
                SelectorKind::Server,
                true,
                ResponseField::NotReturned::<&str>
            ),
            PropertyObservation::Unknown(PropertyUnknownReason::NotReturned)
        );
    }

    #[test]
    fn unique_names_reject_shared_unknown_and_malformed_names() {
        let directory = directory(
            SelectorKind::Registry,
            &[
                ("registry-1", "main"),
                ("registry-2", "dup"),
                ("registry-3", "dup"),
                ("registry-4", ""),
            ],
        );

        assert_eq!(
            directory.unique_name_of(SelectorKind::Registry, "registry-1"),
            Some("main")
        );
        for id in ["registry-2", "registry-9", "registry-4"] {
            assert_eq!(directory.unique_name_of(SelectorKind::Registry, id), None);
        }
        assert_eq!(
            directory.unique_name_of(SelectorKind::Server, "registry-1"),
            None
        );
        assert!(!format!("{directory:?}").contains("main"));
    }
}
