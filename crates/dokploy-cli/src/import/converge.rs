//! Offline convergence proof for a freshly built import.
//!
//! The first plan after an import must be empty. That can be decided before
//! anything is written: the rendered configuration is parsed and compiled exactly
//! as `plan` will, and everything it would own must equal what the imported state
//! records. Because the state was derived from the very remote records the
//! configuration was written from, equality here means the first plan is clean.
//! A builder that records an input it does not declare, or declares one it does
//! not record, fails the import instead of producing a workspace that plans
//! changes against an unchanged Dokploy.

use dokploy_config::DokployConfig;
use dokploy_core::{ConfigDigest, ProtectionIntent, StoredState};
use dokploy_state::StateFile;

use super::ImportError;
use crate::desired::compile_desired;

/// Proves that planning `rendered` against `state` has nothing to change.
pub(super) fn verify(rendered: &str, state: &StateFile) -> Result<(), ImportError> {
    let config = DokployConfig::parse(rendered).map_err(|_| not_convergent("the document"))?;
    // The digest only labels a plan, so a fixed one keeps the proof deterministic.
    let digest = ConfigDigest::parse("0".repeat(64)).expect("a zero digest is canonical");
    let desired =
        compile_desired(&config, digest).map_err(|_| not_convergent("the compiled document"))?;
    let stored =
        StoredState::try_from_state(state).map_err(|_| not_convergent("the imported state"))?;

    let desired_resources = desired.desired_state().resources();
    if desired_resources.len() != state.resources().len() {
        return Err(not_convergent("the resource set"));
    }
    for (address, resource) in state.resources() {
        let Some(wanted) = desired_resources.get(address) else {
            return Err(not_convergent(address));
        };
        let owned = stored
            .properties(address)
            .ok_or_else(|| not_convergent(address))?;

        let protection_agrees = match wanted.protection() {
            ProtectionIntent::Set(protect) => protect == resource.is_protected(),
            ProtectionIntent::Unmanaged => true,
        };
        let mut dependencies = resource.dependencies().to_vec();
        dependencies.sort();
        if wanted.properties() != owned
            || wanted.containment() != resource.containment()
            || wanted.dependencies() != dependencies
            || !protection_agrees
        {
            return Err(not_convergent(address));
        }
    }

    Ok(())
}

/// Reports only a logical address or a fixed phrase; values never appear.
fn not_convergent(subject: impl ToString) -> ImportError {
    ImportError::NotConvergent {
        subject: subject.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use dokploy_config::{ApplicationDocument, ConfigDocument, EnvironmentDocument, Field};
    use dokploy_state::{
        InstanceIdentity, ManagedInputs, RemoteId, ResourceAddress, ResourceKind, ResourceName,
        ResourceState,
    };

    use super::*;

    fn name(value: &str) -> ResourceName {
        ResourceName::new(value).expect("test name is valid")
    }

    fn address(kind: ResourceKind, value: &str) -> ResourceAddress {
        ResourceAddress::new(kind, name(value))
    }

    fn state_of(
        id: &str,
        kind: ResourceKind,
        inputs: serde_json::Value,
        containment: Option<ResourceAddress>,
    ) -> ResourceState {
        ResourceState::new(
            kind,
            RemoteId::new(id).unwrap(),
            false,
            ManagedInputs::try_from_json(inputs).unwrap(),
            containment,
            Vec::new(),
        )
    }

    /// A project, an environment, and an application with a description.
    fn document() -> String {
        let application = ApplicationDocument {
            description: Field::Set("API".to_owned()),
            ..ApplicationDocument::default()
        };
        let mut environment = EnvironmentDocument::default();
        environment
            .add_application(name("api"), application)
            .unwrap();
        let mut document = ConfigDocument::new(name("platform"));
        document
            .add_environment(name("production"), environment)
            .unwrap();
        document
            .add_environment(name("staging"), EnvironmentDocument::default())
            .unwrap();
        document.render().unwrap()
    }

    fn state(application_inputs: serde_json::Value, containment: ResourceAddress) -> StateFile {
        let resources = BTreeMap::from([
            (
                address(ResourceKind::Project, "platform"),
                state_of(
                    "project-1",
                    ResourceKind::Project,
                    serde_json::json!({}),
                    None,
                ),
            ),
            (
                address(ResourceKind::Environment, "production"),
                state_of(
                    "env-1",
                    ResourceKind::Environment,
                    serde_json::json!({}),
                    Some(address(ResourceKind::Project, "platform")),
                ),
            ),
            (
                address(ResourceKind::Environment, "staging"),
                state_of(
                    "env-2",
                    ResourceKind::Environment,
                    serde_json::json!({}),
                    Some(address(ResourceKind::Project, "platform")),
                ),
            ),
            (
                address(ResourceKind::Application, "api"),
                state_of(
                    "app-1",
                    ResourceKind::Application,
                    application_inputs,
                    Some(containment),
                ),
            ),
        ]);

        StateFile::new_with_resources(
            "0.1.0".parse().unwrap(),
            InstanceIdentity::parse("http://127.0.0.1:3000").unwrap(),
            resources,
        )
        .unwrap()
    }

    fn production() -> ResourceAddress {
        address(ResourceKind::Environment, "production")
    }

    #[test]
    fn state_that_matches_the_document_is_convergent() {
        let state = state(serde_json::json!({"description": "API"}), production());

        verify(&document(), &state).expect("matching state converges");
    }

    #[test]
    fn a_builder_that_records_an_input_the_document_does_not_declare_is_caught() {
        // The state owns `replicas`, but the document leaves it unmanaged, so the
        // first plan would try to relinquish it.
        let state = state(
            serde_json::json!({"description": "API", "replicas": 1}),
            production(),
        );

        let error = verify(&document(), &state).expect_err("extra state input");

        assert!(
            matches!(&error, ImportError::NotConvergent { subject } if subject == "application.api"),
            "{error}"
        );
    }

    #[test]
    fn a_builder_that_declares_a_value_the_state_does_not_record_is_caught() {
        let state = state(serde_json::json!({}), production());

        let error = verify(&document(), &state).expect_err("missing state input");

        assert!(
            matches!(error, ImportError::NotConvergent { .. }),
            "{error}"
        );
    }

    #[test]
    fn a_builder_that_records_a_different_value_is_caught() {
        let state = state(serde_json::json!({"description": "Other"}), production());

        assert!(verify(&document(), &state).is_err());
    }

    #[test]
    fn a_builder_that_records_the_wrong_containment_is_caught() {
        // A real environment, but not the one the document nests the application in.
        let state = state(
            serde_json::json!({"description": "API"}),
            address(ResourceKind::Environment, "staging"),
        );

        let error = verify(&document(), &state).expect_err("wrong containment");

        assert!(
            matches!(&error, ImportError::NotConvergent { subject } if subject == "application.api"),
            "{error}"
        );
    }

    #[test]
    fn a_resource_missing_from_state_is_caught() {
        let resources = BTreeMap::from([(
            address(ResourceKind::Project, "platform"),
            state_of(
                "project-1",
                ResourceKind::Project,
                serde_json::json!({}),
                None,
            ),
        )]);
        let state = StateFile::new_with_resources(
            "0.1.0".parse().unwrap(),
            InstanceIdentity::parse("http://127.0.0.1:3000").unwrap(),
            resources,
        )
        .unwrap();

        let error = verify(&document(), &state).expect_err("resource sets differ");

        assert!(
            matches!(&error, ImportError::NotConvergent { subject } if subject == "the resource set"),
            "{error}"
        );
    }
}
