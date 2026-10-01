use dokploy_cli::desired::{ExternalExecutionId, compile_desired};
use dokploy_config::{DokployConfig, SelectorKind};
use dokploy_core::{
    ComparableValue, ConfigDigest, ExternalResolution, OwnedValue, PropertyPath, RemoteFailureKind,
    RemoteObservation, RemoteState,
};
use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress};

const NAME_CANARY: &str = "selector-name-canary-5d0e";

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("valid test digest")
}

fn value(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(ComparableValue::try_from_json(value).expect("non-null test value"))
}

fn api() -> ResourceAddress {
    "application.api".parse().unwrap()
}

fn compile(application: &str) -> dokploy_cli::desired::CompiledDesired {
    let config = DokployConfig::parse(&format!(
        "version: 1\nproject:\n  name: platform\nenvironments:\n  production:\n    applications:\n      api:\n{application}\n"
    ))
    .expect("valid configuration");
    compile_desired(&config, digest()).expect("configuration compiles")
}

#[test]
fn selectors_compile_to_stable_name_values_and_explicit_clears() {
    let compiled = compile(&format!(
        "        server:\n          name: {NAME_CANARY}\n        build_server:\n          name: builder\n        registry:\n          name: main\n        build_registry: null\n        rollback_registry:\n          name: archive"
    ));
    let properties = compiled
        .desired_state()
        .resources()
        .get(&api())
        .expect("application is desired")
        .properties();

    assert_eq!(
        properties.get(&PropertyPath::Server),
        Some(&value(serde_json::json!({"name": NAME_CANARY})))
    );
    assert_eq!(
        properties.get(&PropertyPath::BuildServer),
        Some(&value(serde_json::json!({"name": "builder"})))
    );
    assert_eq!(
        properties.get(&PropertyPath::Registry),
        Some(&value(serde_json::json!({"name": "main"})))
    );
    assert_eq!(
        properties.get(&PropertyPath::BuildRegistry),
        Some(&OwnedValue::Null)
    );
    assert_eq!(
        properties.get(&PropertyPath::RollbackRegistry),
        Some(&value(serde_json::json!({"name": "archive"})))
    );
}

#[test]
fn local_and_unmanaged_selectors_compile_without_external_state() {
    let compiled = compile("        server:\n          local: true");
    let properties = compiled
        .desired_state()
        .resources()
        .get(&api())
        .unwrap()
        .properties();
    assert_eq!(
        properties.get(&PropertyPath::Server),
        Some(&value(serde_json::json!({"local": true})))
    );
    assert!(!properties.contains_key(&PropertyPath::Registry));

    let plain = compile("        description: API");
    assert_eq!(plain.bindings().external_selectors().count(), 0);
}

#[test]
fn bindings_list_selectors_with_kinds_and_redact_names_in_debug_output() {
    let compiled = compile(&format!(
        "        server:\n          name: {NAME_CANARY}\n        registry:\n          name: main\n        build_registry: null"
    ));

    let selectors: Vec<_> = compiled
        .bindings()
        .external_selectors()
        .map(|(address, path, kind, selector)| {
            (
                address.clone(),
                path.clone(),
                kind,
                selector.name().map(str::to_owned),
            )
        })
        .collect();

    assert_eq!(
        selectors,
        [
            (
                api(),
                PropertyPath::Server,
                SelectorKind::Server,
                Some(NAME_CANARY.to_owned())
            ),
            (
                api(),
                PropertyPath::Registry,
                SelectorKind::Registry,
                Some("main".to_owned())
            ),
        ]
    );
    let debug = format!("{compiled:?} {:?}", compiled.bindings());
    assert!(!debug.contains(NAME_CANARY));
}

#[test]
fn resolved_identities_enter_the_execution_sidecar_only_after_binding() {
    let mut compiled = compile(
        "        server:\n          name: edge\n        build_server:\n          name: builder\n        registry:\n          name: main\n        rollback_registry:\n          name: archive",
    );
    let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
    let remote = RemoteState::try_new(instance, [(api(), RemoteObservation::Missing)])
        .expect("remote state")
        .with_external_resolutions([
            (
                (api(), PropertyPath::Server),
                ExternalResolution::Resolved(RemoteId::new("server-secret-id").unwrap()),
            ),
            (
                (api(), PropertyPath::BuildServer),
                ExternalResolution::Ambiguous,
            ),
            (
                (api(), PropertyPath::Registry),
                ExternalResolution::Resolved(RemoteId::new("registry-secret-id").unwrap()),
            ),
            (
                (api(), PropertyPath::RollbackRegistry),
                ExternalResolution::Unavailable(RemoteFailureKind::Unavailable),
            ),
        ])
        .expect("resolutions");

    assert!(
        compiled
            .bindings()
            .external_id(&api(), &PropertyPath::Server)
            .is_none()
    );
    compiled.bind_external_resolutions(&remote);

    assert!(matches!(
        compiled.bindings().external_id(&api(), &PropertyPath::Server),
        Some(ExternalExecutionId::Remote(id)) if id.as_str() == "server-secret-id"
    ));
    assert!(matches!(
        compiled.bindings().external_id(&api(), &PropertyPath::Registry),
        Some(ExternalExecutionId::Remote(id)) if id.as_str() == "registry-secret-id"
    ));
    assert!(
        compiled
            .bindings()
            .external_id(&api(), &PropertyPath::BuildServer)
            .is_none(),
        "an ambiguous selector never reaches execution"
    );
    assert!(
        compiled
            .bindings()
            .external_id(&api(), &PropertyPath::RollbackRegistry)
            .is_none()
    );
    let debug = format!(
        "{compiled:?} {:?} {:?}",
        compiled.bindings(),
        compiled
            .bindings()
            .external_id(&api(), &PropertyPath::Server)
    );
    assert!(!debug.contains("secret-id"));
}

#[test]
fn a_local_resolution_binds_as_the_local_server() {
    let mut compiled = compile("        server:\n          local: true");
    let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
    let remote = RemoteState::try_new(instance, [(api(), RemoteObservation::Missing)])
        .expect("remote state")
        .with_external_resolutions([((api(), PropertyPath::Server), ExternalResolution::Local)])
        .expect("resolutions");

    compiled.bind_external_resolutions(&remote);

    assert!(matches!(
        compiled
            .bindings()
            .external_id(&api(), &PropertyPath::Server),
        Some(ExternalExecutionId::Local)
    ));
}
