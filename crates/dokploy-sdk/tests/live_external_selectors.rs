use std::collections::HashSet;

use dokploy_sdk::Dokploy;

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live selector test"))
}

#[tokio::test]
#[ignore = "performs inert selector reads against local Dokploy"]
async fn live_external_selector_reads_are_bounded_typed_and_unique() {
    assert_eq!(
        std::env::var("DOKPLOY_EXTERNAL_SELECTOR_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-external-selectors-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");

    let servers = client
        .servers()
        .all()
        .await
        .expect("live server selector collection is readable");
    assert_eq!(
        servers
            .servers()
            .iter()
            .map(|server| server.server_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        servers.servers().len()
    );

    let registries = client
        .registries()
        .all()
        .await
        .expect("live registry selector collection is readable");
    assert_eq!(
        registries
            .registries()
            .iter()
            .map(|registry| registry.registry_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        registries.registries().len()
    );

    let destinations = client
        .destinations()
        .all()
        .await
        .expect("live destination selector collection is readable");
    assert_eq!(
        destinations
            .destinations()
            .iter()
            .map(|destination| destination.destination_id.as_str())
            .collect::<HashSet<_>>()
            .len(),
        destinations.destinations().len()
    );
}
