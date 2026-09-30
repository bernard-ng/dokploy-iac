use std::num::NonZeroU16;

use dokploy_sdk::{
    ApplicationId, CreatePort, Dokploy, PortId, PortProtocol, PublishMode, UpdatePort,
};

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Port test"))
}

fn port(value: u16) -> NonZeroU16 {
    NonZeroU16::new(value).expect("live test Port is nonzero")
}

#[tokio::test]
#[ignore = "mutates a disposable application on the pinned local Dokploy instance"]
async fn live_port_adapter_converges_without_deploying_its_application() {
    assert_eq!(
        std::env::var("DOKPLOY_PORT_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-port-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let application_id = ApplicationId::new(required_environment("DOKPLOY_PORT_APPLICATION_ID"));

    let before = client
        .ports()
        .by_application(application_id.clone())
        .await
        .expect("preflight application collection is readable");
    assert!(before.ports().is_empty(), "disposable target must be empty");

    let created = client
        .ports()
        .create(CreatePort::new(
            application_id.clone(),
            port(18080),
            port(8080),
            PublishMode::Ingress,
            PortProtocol::Tcp,
        ))
        .await
        .expect("live Port creation succeeds");
    let port_id = created.port_id().clone();

    let created_details = client
        .ports()
        .get(port_id.clone())
        .await
        .expect("created Port is directly readable");
    assert_eq!(created_details.application_id, application_id);
    assert_eq!(created_details.published_port, port(18080));
    assert_eq!(created_details.target_port, port(8080));
    assert_eq!(created_details.publish_mode, PublishMode::Ingress);
    assert_eq!(created_details.protocol, PortProtocol::Tcp);
    let created_parent = client
        .ports()
        .by_application(application_id.clone())
        .await
        .expect("created parent collection is readable");
    assert_eq!(created_parent.ports(), &[created_details]);

    client
        .ports()
        .update(UpdatePort::new(
            port_id.clone(),
            port(19090),
            port(9090),
            PublishMode::Host,
            PortProtocol::Udp,
        ))
        .await
        .expect("live all-field Port update succeeds");
    let updated = client
        .ports()
        .get(port_id.clone())
        .await
        .expect("updated Port is directly readable");
    assert_eq!(updated.application_id, application_id);
    assert_eq!(updated.published_port, port(19090));
    assert_eq!(updated.target_port, port(9090));
    assert_eq!(updated.publish_mode, PublishMode::Host);
    assert_eq!(updated.protocol, PortProtocol::Udp);
    let updated_parent = client
        .ports()
        .by_application(application_id.clone())
        .await
        .expect("updated parent collection is readable");
    assert_eq!(updated_parent.ports(), &[updated]);

    client
        .ports()
        .delete(port_id.clone())
        .await
        .expect("live Port deletion succeeds");
    let removed = client
        .ports()
        .get(PortId::new(port_id.as_str()))
        .await
        .expect_err("removed Port must be absent");
    assert!(
        removed.dokploy().is_some(),
        "Dokploy must reject the removed direct identity"
    );
    let after = client
        .ports()
        .by_application(application_id)
        .await
        .expect("cleanup parent collection is readable");
    assert!(after.ports().is_empty());
}
