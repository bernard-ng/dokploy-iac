use super::*;

#[test]
fn content_key_and_descriptor_rotation_have_the_required_digest_effects() {
    let parse = |environment_name: &str| {
        DokployConfig::parse(&format!(
            r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          TOKEN:
            secret:
              env: {environment_name}
"#
        ))
        .unwrap()
    };
    let before_config = parse("SECRET_BEFORE");
    let renamed_config = parse("SECRET_RENAMED");
    let same_values = RecordingSourceResolver {
        environment: BTreeMap::from([
            ("SECRET_BEFORE".to_owned(), Ok(b"same-value".to_vec())),
            ("SECRET_RENAMED".to_owned(), Ok(b"same-value".to_vec())),
        ]),
        ..RecordingSourceResolver::default()
    };
    let changed_value = RecordingSourceResolver {
        environment: BTreeMap::from([("SECRET_BEFORE".to_owned(), Ok(b"changed-value".to_vec()))]),
        ..RecordingSourceResolver::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let compile = |config: &DokployConfig,
                   source_digest: ConfigDigest,
                   loader: &ExistingFingerprinterLoader,
                   resolver: &RecordingSourceResolver| {
        compile_for_instance_with(
            config,
            source_digest,
            instance(),
            workspace.path(),
            loader,
            resolver,
        )
        .unwrap()
    };
    let loader = existing_fingerprinter_loader();
    let mut before = compile(&before_config, digest('a'), &loader, &same_values);
    let mut renamed = compile(&renamed_config, digest('b'), &loader, &same_values);
    let mut content_rotated = compile(&before_config, digest('a'), &loader, &changed_value);
    let rotated_loader =
        existing_fingerprinter_loader_with("0199a0c8-2351-7c31-8899-2c8f81983ea6", [8; 32]);
    let mut key_rotated = compile(&before_config, digest('a'), &rotated_loader, &same_values);
    let address: ResourceAddress = "application.api".parse().unwrap();
    let path = PropertyPath::environment_variable("TOKEN").unwrap();
    let take_receipt = |compiled: &mut CompiledDesired| {
        serde_json::to_value(
            compiled
                .take_sensitive(&address, &path)
                .unwrap()
                .into_parts()
                .1,
        )
        .unwrap()
    };
    let before_receipt = take_receipt(&mut before);
    let renamed_receipt = take_receipt(&mut renamed);
    let content_receipt = take_receipt(&mut content_rotated);
    let key_receipt = take_receipt(&mut key_rotated);

    assert_eq!(before_receipt, renamed_receipt);
    assert_ne!(
        before.desired_state().digest(),
        renamed.desired_state().digest()
    );
    assert_ne!(before_receipt, content_receipt);
    assert_ne!(
        before.desired_state().digest(),
        content_rotated.desired_state().digest()
    );
    assert_ne!(before_receipt, key_receipt);
    assert_ne!(
        before.desired_state().digest(),
        key_rotated.desired_state().digest()
    );
}

#[test]
fn receipt_converges_and_raw_value_never_reaches_debug_plan_state_or_journal() {
    let canary = "cross-artifact-sensitive-canary";
    let config = DokployConfig::parse(&format!(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    applications:
      api:
        environment:
          TOKEN:
            value: {canary}
"#
    ))
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    let mut compiled = compile_for_instance_with(
        &config,
        digest('a'),
        instance(),
        workspace.path(),
        &existing_fingerprinter_loader(),
        &PanicSourceResolver,
    )
    .unwrap();
    assert!(!format!("{compiled:?}").contains(canary));

    let project: ResourceAddress = "project.platform".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let application: ResourceAddress = "application.api".parse().unwrap();
    let token_path = PropertyPath::environment_variable("TOKEN").unwrap();
    assert!(matches!(
        compiled.desired_state().resources()[&application]
            .properties()
            .get(&token_path),
        Some(OwnedValue::Sensitive(_))
    ));
    let (bytes, receipt) = compiled
        .take_sensitive(&application, &token_path)
        .unwrap()
        .into_parts();
    assert_eq!(bytes.as_slice(), canary.as_bytes());
    drop(bytes);

    let mut state = StateFile::new(Version::new(0, 1, 0), instance());
    state
        .upsert_resource(
            project.clone(),
            ResourceState::new(
                ResourceKind::Project,
                RemoteId::new("project-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                None,
                vec![],
            ),
        )
        .unwrap();
    state
        .upsert_resource(
            environment.clone(),
            ResourceState::new(
                ResourceKind::Environment,
                RemoteId::new("environment-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                Some(project.clone()),
                vec![],
            ),
        )
        .unwrap();
    state
        .upsert_resource(
            application.clone(),
            ResourceState::try_new(
                ResourceKind::Application,
                RemoteId::new("application-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                SensitiveInputs::try_from_entries([(
                    SensitivePropertyPath::parse("environment.TOKEN").unwrap(),
                    receipt,
                )])
                .unwrap(),
                Some(environment.clone()),
                vec![],
            )
            .unwrap(),
        )
        .unwrap();

    let stored = StoredState::try_from_state(&state).unwrap();
    let remote = RemoteState::try_new(
        instance(),
        [
            (
                project,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").unwrap(),
                    BTreeMap::new(),
                )),
            ),
            (
                environment,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("environment-1").unwrap(),
                    BTreeMap::new(),
                )),
            ),
            (
                application,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("application-1").unwrap(),
                    BTreeMap::from([(
                        token_path,
                        PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
                    )]),
                )),
            ),
        ],
    )
    .unwrap();
    let plan = plan(compiled.desired_state(), &stored, &remote);

    assert!(plan.changes().is_empty());
    assert!(
        !String::from_utf8(plan.to_json_bytes())
            .unwrap()
            .contains(canary)
    );
    let state_bytes = serde_json::to_vec(&state).unwrap();
    assert!(!String::from_utf8(state_bytes).unwrap().contains(canary));

    let store = StateStore::new(workspace.path(), instance()).unwrap();
    let journal_state = StateFile::new(Version::new(0, 1, 0), instance());
    store
        .begin_write()
        .unwrap()
        .checkpoint(ExpectedState::absent(), &journal_state)
        .unwrap();
    let mut session = store.begin_write().unwrap();
    let journal = OperationJournal::begin(
        &mut session,
        PlanDigest::parse(compiled.desired_state().digest().as_str()).unwrap(),
    )
    .unwrap();
    journal.commit().unwrap();
    drop(session);

    let journal_bytes: Vec<u8> = std::fs::read_dir(workspace.path().join(".dokploy/journal"))
        .unwrap()
        .flat_map(|entry| std::fs::read(entry.unwrap().path()).unwrap())
        .collect();
    assert!(!String::from_utf8(journal_bytes).unwrap().contains(canary));
}

#[test]
fn content_rotation_plans_exactly_one_sensitive_update() {
    let config = DokployConfig::parse(
        r#"
version: 1
project:
  name: platform
environments:
  production:
    redis:
      cache:
        password:
          env: CACHE_PASSWORD
"#,
    )
    .unwrap();
    let stable_resolver = RecordingSourceResolver {
        environment: BTreeMap::from([("CACHE_PASSWORD".to_owned(), Ok(b"stable-secret".to_vec()))]),
        ..RecordingSourceResolver::default()
    };
    let rotated_resolver = RecordingSourceResolver {
        environment: BTreeMap::from([(
            "CACHE_PASSWORD".to_owned(),
            Ok(b"rotated-secret".to_vec()),
        )]),
        ..RecordingSourceResolver::default()
    };
    let workspace = tempfile::tempdir().unwrap();
    let loader = existing_fingerprinter_loader();
    let compile = |resolver: &RecordingSourceResolver| {
        compile_for_instance_with(
            &config,
            digest('a'),
            instance(),
            workspace.path(),
            &loader,
            resolver,
        )
        .unwrap()
    };
    let mut stable = compile(&stable_resolver);
    let rotated = compile(&rotated_resolver);
    let project: ResourceAddress = "project.platform".parse().unwrap();
    let environment: ResourceAddress = "environment.production".parse().unwrap();
    let redis: ResourceAddress = "redis.cache".parse().unwrap();
    let receipt = stable
        .take_sensitive(&redis, &PropertyPath::Password)
        .unwrap()
        .into_parts()
        .1;
    let mut state = StateFile::new(Version::new(0, 1, 0), instance());

    for (address, kind, remote_id, containment) in [
        (project.clone(), ResourceKind::Project, "project-1", None),
        (
            environment.clone(),
            ResourceKind::Environment,
            "environment-1",
            Some(project.clone()),
        ),
    ] {
        state
            .upsert_resource(
                address,
                ResourceState::new(
                    kind,
                    RemoteId::new(remote_id).unwrap(),
                    false,
                    ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                    containment,
                    Vec::new(),
                ),
            )
            .unwrap();
    }
    state
        .upsert_resource(
            redis.clone(),
            ResourceState::try_new(
                ResourceKind::Redis,
                RemoteId::new("redis-1").unwrap(),
                false,
                ManagedInputs::try_from_json(serde_json::json!({})).unwrap(),
                SensitiveInputs::try_from_entries([(
                    SensitivePropertyPath::parse("password").unwrap(),
                    receipt,
                )])
                .unwrap(),
                Some(environment.clone()),
                vec![],
            )
            .unwrap(),
        )
        .unwrap();

    let stored = StoredState::try_from_state(&state).unwrap();
    let remote = RemoteState::try_new(
        instance(),
        [
            (
                project,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("project-1").unwrap(),
                    BTreeMap::new(),
                )),
            ),
            (
                environment,
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("environment-1").unwrap(),
                    BTreeMap::new(),
                )),
            ),
            (
                redis.clone(),
                RemoteObservation::Present(RemoteResource::new(
                    RemoteId::new("redis-1").unwrap(),
                    BTreeMap::from([(
                        PropertyPath::Password,
                        PropertyObservation::Unknown(PropertyUnknownReason::Sensitive),
                    )]),
                )),
            ),
        ],
    )
    .unwrap();

    assert!(
        plan(stable.desired_state(), &stored, &remote)
            .changes()
            .is_empty()
    );
    let changed = plan(rotated.desired_state(), &stored, &remote);
    assert_eq!(changed.changes().len(), 1);
    assert_eq!(changed.changes()[0].address(), &redis);
    assert_eq!(changed.changes()[0].kind(), ChangeKind::Update);
}
