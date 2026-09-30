use dokploy_sdk::{ApplicationId, CreateSecurity, Dokploy, SecurityId, UpdateSecurity};
use zeroize::Zeroizing;

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Security test"))
}

#[tokio::test]
#[ignore = "mutates a disposable application on the pinned local Dokploy instance"]
async fn live_security_adapter_converges_without_deploying_its_application() {
    assert_eq!(
        std::env::var("DOKPLOY_SECURITY_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-security-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let application_id =
        ApplicationId::new(required_environment("DOKPLOY_SECURITY_APPLICATION_ID"));

    let before = client
        .security()
        .by_application(application_id.clone())
        .await
        .expect("preflight Security collection is readable");
    assert!(
        before.entries().is_empty(),
        "disposable target must be empty"
    );

    let created = client
        .security()
        .create(CreateSecurity::new(
            application_id.clone(),
            "owner",
            Zeroizing::new("live-security-password-canary".to_owned()),
        ))
        .await
        .expect("live Security creation succeeds");
    let security_id = created.security_id().clone();

    let created_details = client
        .security()
        .get(security_id.clone())
        .await
        .expect("created Security entry agrees with its parent collection");
    assert_eq!(created_details.application_id, application_id);
    assert_eq!(created_details.username, "owner");
    assert!(created_details.password_present);

    client
        .security()
        .update(UpdateSecurity::new(
            security_id.clone(),
            "operator",
            Zeroizing::new("updated-live-security-password-canary".to_owned()),
        ))
        .await
        .expect("live complete Security update succeeds");
    let updated = client
        .security()
        .get(security_id.clone())
        .await
        .expect("updated Security entry agrees with its parent collection");
    assert_eq!(updated.application_id, application_id);
    assert_eq!(updated.username, "operator");
    assert!(updated.password_present);

    client
        .security()
        .delete(security_id.clone())
        .await
        .expect("live Security deletion succeeds");
    let removed = client
        .security()
        .get(SecurityId::new(security_id.as_str()))
        .await
        .expect_err("removed Security entry must not be directly readable");
    assert!(
        removed.dokploy().is_some(),
        "Dokploy must reject the removed direct identity"
    );
    let after = client
        .security()
        .by_application(application_id)
        .await
        .expect("cleanup parent collection is readable");
    assert!(after.entries().is_empty());
}
