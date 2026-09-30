use dokploy_sdk::{
    ApplicationId, ComposeId, CreateSchedule, Dokploy, ScheduleId, ScheduleTarget, ShellType,
    UpdateSchedule,
};
use zeroize::Zeroizing;

fn required_environment(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is required for the live Schedule test"))
}

async fn exercise_target(client: &Dokploy, target: ScheduleTarget, name: &str) {
    let before = client
        .schedules()
        .by_target(target.clone())
        .await
        .expect("preflight Schedule collection is readable");
    assert!(before.schedules().is_empty());

    let created = client
        .schedules()
        .create(CreateSchedule::new(
            target.clone(),
            name,
            Some("initial disabled schedule".to_owned()),
            "7 4 * * 0",
            ShellType::Bash,
            Zeroizing::new("live-schedule-command-canary".to_owned()),
            Some(Zeroizing::new("live-schedule-script-canary".to_owned())),
            false,
            Some("UTC".to_owned()),
        ))
        .await
        .expect("live Schedule creation succeeds");
    let schedule_id = created.schedule_id().clone();

    let details = client
        .schedules()
        .get(schedule_id.clone())
        .await
        .expect("created Schedule agrees with its target collection");
    assert_eq!(details.target, target);
    assert_eq!(details.name, name);
    assert_eq!(details.cron_expression, "7 4 * * 0");
    assert_eq!(details.shell_type, ShellType::Bash);
    assert!(details.command_present);
    assert!(details.script_present);
    assert!(!details.enabled);

    let updated_name = format!("{name}-updated");
    client
        .schedules()
        .update(UpdateSchedule::new(
            schedule_id.clone(),
            target.clone(),
            updated_name.clone(),
            Some("updated disabled schedule".to_owned()),
            "13 5 * * 1",
            ShellType::Sh,
            Zeroizing::new("updated-live-schedule-command-canary".to_owned()),
            Some(Zeroizing::new(
                "updated-live-schedule-script-canary".to_owned(),
            )),
            false,
            Some("Africa/Lubumbashi".to_owned()),
        ))
        .await
        .expect("live complete Schedule update succeeds");
    let updated = client
        .schedules()
        .get(schedule_id.clone())
        .await
        .expect("updated Schedule agrees with its target collection");
    assert_eq!(updated.target, target);
    assert_eq!(updated.name, updated_name);
    assert_eq!(
        updated.description.as_deref(),
        Some("updated disabled schedule")
    );
    assert_eq!(updated.cron_expression, "13 5 * * 1");
    assert_eq!(updated.shell_type, ShellType::Sh);
    assert!(updated.command_present);
    assert!(updated.script_present);
    assert!(!updated.enabled);
    assert_eq!(updated.timezone.as_deref(), Some("Africa/Lubumbashi"));

    client
        .schedules()
        .delete(schedule_id.clone())
        .await
        .expect("live Schedule deletion succeeds");
    let removed = client
        .schedules()
        .get(ScheduleId::new(schedule_id.as_str()))
        .await
        .expect_err("removed Schedule must not be directly readable");
    assert!(removed.dokploy().is_some());
    let after = client
        .schedules()
        .by_target(target)
        .await
        .expect("cleanup target collection is readable");
    assert!(after.schedules().is_empty());
}

#[tokio::test]
#[ignore = "mutates disposable application and Compose targets on local Dokploy"]
async fn live_schedule_adapter_converges_without_deploying_or_executing_targets() {
    assert_eq!(
        std::env::var("DOKPLOY_SCHEDULE_LIVE_TEST").as_deref(),
        Ok("1"),
        "run this test through scripts/integration/test-schedule-sdk.sh"
    );
    let client = Dokploy::builder()
        .url(required_environment("DOKPLOY_URL"))
        .api_key(required_environment("DOKPLOY_API_KEY"))
        .build()
        .expect("live client configuration is valid");

    exercise_target(
        &client,
        ScheduleTarget::Application(ApplicationId::new(required_environment(
            "DOKPLOY_SCHEDULE_APPLICATION_ID",
        ))),
        "application-job",
    )
    .await;
    exercise_target(
        &client,
        ScheduleTarget::Compose {
            compose_id: ComposeId::new(required_environment("DOKPLOY_SCHEDULE_COMPOSE_ID")),
            service_name: "worker".to_owned(),
        },
        "compose-job",
    )
    .await;
}
