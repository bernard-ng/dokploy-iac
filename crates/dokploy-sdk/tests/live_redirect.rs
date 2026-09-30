use dokploy_sdk::{ApplicationId, CreateRedirect, Dokploy, RedirectId, UpdateRedirect};

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Redirect test"))
}

#[tokio::test]
#[ignore = "mutates a disposable application on the pinned local Dokploy instance"]
async fn live_redirect_adapter_converges_without_deploying_its_application() {
    assert_eq!(
        std::env::var("DOKPLOY_REDIRECT_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-redirect-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");
    let application_id =
        ApplicationId::new(required_environment("DOKPLOY_REDIRECT_APPLICATION_ID"));

    let before = client
        .redirects()
        .by_application(application_id.clone())
        .await
        .expect("preflight application collection is readable");
    assert!(
        before.redirects().is_empty(),
        "disposable target must be empty"
    );

    let created = client
        .redirects()
        .create(CreateRedirect::new(
            application_id.clone(),
            "^/legacy/(.*)$",
            "/current/$1",
            false,
        ))
        .await
        .expect("live Redirect creation succeeds");
    let redirect_id = created.redirect_id().clone();

    let created_details = client
        .redirects()
        .get(redirect_id.clone())
        .await
        .expect("created Redirect agrees with its parent collection");
    assert_eq!(created_details.application_id, application_id);
    assert_eq!(created_details.regex, "^/legacy/(.*)$");
    assert_eq!(created_details.replacement, "/current/$1");
    assert!(!created_details.permanent);

    client
        .redirects()
        .update(UpdateRedirect::new(
            redirect_id.clone(),
            "^/old/(.*)$",
            "/new/$1",
            true,
        ))
        .await
        .expect("live all-field Redirect update succeeds");
    let updated = client
        .redirects()
        .get(redirect_id.clone())
        .await
        .expect("updated Redirect agrees with its parent collection");
    assert_eq!(updated.application_id, application_id);
    assert_eq!(updated.regex, "^/old/(.*)$");
    assert_eq!(updated.replacement, "/new/$1");
    assert!(updated.permanent);

    client
        .redirects()
        .delete(redirect_id.clone())
        .await
        .expect("live Redirect deletion succeeds");
    let removed = client
        .redirects()
        .get(RedirectId::new(redirect_id.as_str()))
        .await
        .expect_err("removed Redirect must not be directly readable");
    assert!(
        removed.dokploy().is_some(),
        "Dokploy must reject the removed direct identity"
    );
    let after = client
        .redirects()
        .by_application(application_id)
        .await
        .expect("cleanup parent collection is readable");
    assert!(after.redirects().is_empty());
}
