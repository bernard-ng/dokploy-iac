use std::collections::BTreeSet;

use dokploy_cli::external::ExternalDirectory;
use dokploy_config::{ExternalSelector, SelectorKind};
use dokploy_core::ExternalResolution;
use dokploy_sdk::Dokploy;

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live selector test"))
}

/// Resolves disposable inert records through the real selector seam.
///
/// The records are created and removed by `test-external-selectors-apply.sh`; this
/// test never mutates Dokploy and never prints an external identity.
#[tokio::test]
#[ignore = "performs inert selector reads against local Dokploy"]
async fn live_selectors_resolve_exact_unique_names_and_reject_ambiguity() {
    assert_eq!(
        std::env::var("DOKPLOY_EXTERNAL_SELECTOR_RESOLUTION_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-external-selectors-apply.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let server_name = required_environment("LIVE_SELECTOR_SERVER_NAME");
    let registry_name = required_environment("LIVE_SELECTOR_REGISTRY_NAME");
    let duplicate_name = required_environment("LIVE_SELECTOR_DUPLICATE_REGISTRY_NAME");
    let destination_name = required_environment("LIVE_SELECTOR_DESTINATION_NAME");

    let kinds = BTreeSet::from([
        SelectorKind::Server,
        SelectorKind::Registry,
        SelectorKind::Destination,
    ]);
    let directory = ExternalDirectory::load(&client, &kinds).await;
    let servers = client.servers().all().await.expect("servers are readable");
    let registries = client
        .registries()
        .all()
        .await
        .expect("registries are readable");
    let destinations = client
        .destinations()
        .all()
        .await
        .expect("destinations are readable");

    // A named, non-local server resolves to the one record with that exact name.
    let server_id = servers
        .servers()
        .iter()
        .filter(|server| server.name == server_name)
        .map(|server| server.server_id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(server_id.len(), 1, "the disposable server is unique");
    let resolution =
        directory.resolve(SelectorKind::Server, &ExternalSelector::named(&server_name));
    assert_eq!(
        resolution.remote_id().map(|id| id.as_str()),
        Some(server_id[0].as_str())
    );
    assert_eq!(
        directory.unique_name_of(SelectorKind::Server, &server_id[0]),
        Some(server_name.as_str())
    );
    assert_eq!(
        directory.resolve(SelectorKind::Server, &ExternalSelector::Local),
        ExternalResolution::Local
    );

    let registry_id = registries
        .registries()
        .iter()
        .filter(|registry| registry.registry_name == registry_name)
        .map(|registry| registry.registry_id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(registry_id.len(), 1, "the disposable registry is unique");
    assert_eq!(
        directory
            .resolve(
                SelectorKind::Registry,
                &ExternalSelector::named(&registry_name)
            )
            .remote_id()
            .map(|id| id.as_str()),
        Some(registry_id[0].as_str())
    );

    let destination_id = destinations
        .destinations()
        .iter()
        .filter(|destination| destination.name == destination_name)
        .map(|destination| destination.destination_id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        destination_id.len(),
        1,
        "the disposable destination is unique"
    );
    assert_eq!(
        directory
            .resolve(
                SelectorKind::Destination,
                &ExternalSelector::named(&destination_name)
            )
            .remote_id()
            .map(|id| id.as_str()),
        Some(destination_id[0].as_str())
    );

    // Exact matching: a different case or an absent name never resolves.
    assert_eq!(
        directory.resolve(
            SelectorKind::Registry,
            &ExternalSelector::named(registry_name.to_uppercase())
        ),
        ExternalResolution::Unmatched
    );
    assert_eq!(
        directory.resolve(
            SelectorKind::Server,
            &ExternalSelector::named("phase8-selector-absent")
        ),
        ExternalResolution::Unmatched
    );
    assert_eq!(
        directory.resolve(SelectorKind::Registry, &ExternalSelector::Local),
        ExternalResolution::Unavailable(dokploy_core::RemoteFailureKind::InvalidResponse)
    );

    // Two records sharing one name are ambiguous and cannot be written back as a selector.
    let duplicates = registries
        .registries()
        .iter()
        .filter(|registry| registry.registry_name == duplicate_name)
        .map(|registry| registry.registry_id.as_str().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(duplicates.len(), 2, "the duplicate-name fixtures exist");
    assert_eq!(
        directory.resolve(
            SelectorKind::Registry,
            &ExternalSelector::named(&duplicate_name)
        ),
        ExternalResolution::Ambiguous
    );
    for identity in &duplicates {
        assert_eq!(
            directory.name_of(SelectorKind::Registry, identity),
            Some(duplicate_name.as_str())
        );
        assert_eq!(
            directory.unique_name_of(SelectorKind::Registry, identity),
            None
        );
    }

    let debug = format!("{directory:?}");
    for identity in server_id.iter().chain(&registry_id).chain(&destination_id) {
        assert!(!debug.contains(identity));
    }
    assert!(!debug.contains(&server_name));
}
