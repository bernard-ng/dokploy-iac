#[path = "support/placement.rs"]
mod fake;

use dokploy_cli::desired::{ExternalExecutionId, compile_desired};
use dokploy_config::{DokployConfig, SelectorKind};
use dokploy_core::{
    ComparableValue, ConfigDigest, ExternalResolution, OwnedValue, PropertyPath, RemoteFailureKind,
    RemoteObservation, RemoteState,
};
use dokploy_state::{InstanceIdentity, RemoteId};
use fake::{KINDS, Kind, LOCAL, named};

const NAME_CANARY: &str = "selector-name-canary-5d0e";
const PROTECTED: &str = "        lifecycle:\n          protect: true\n";

fn digest() -> ConfigDigest {
    ConfigDigest::parse("a".repeat(64)).expect("valid test digest")
}

fn value(value: serde_json::Value) -> OwnedValue {
    OwnedValue::Value(ComparableValue::try_from_json(value).expect("non-null test value"))
}

fn compile(kind: &Kind, server: &str, extra: &str) -> dokploy_cli::desired::CompiledDesired {
    let config = DokployConfig::parse(&kind.quiet_config(server, &format!("{PROTECTED}{extra}")))
        .expect("valid configuration");
    compile_desired(&config, digest()).expect("configuration compiles")
}

#[test]
fn named_local_and_unmanaged_placements_compile_to_stable_values() {
    for kind in KINDS {
        let properties = |compiled: &dokploy_cli::desired::CompiledDesired| {
            compiled
                .desired_state()
                .resources()
                .get(&kind.address())
                .expect("service is desired")
                .properties()
                .clone()
        };

        let named_properties = properties(&compile(&kind, &named(NAME_CANARY), ""));
        assert_eq!(
            named_properties.get(&PropertyPath::Server),
            Some(&value(serde_json::json!({"name": NAME_CANARY}))),
            "{}",
            kind.name
        );

        let local_properties = properties(&compile(&kind, LOCAL, ""));
        assert_eq!(
            local_properties.get(&PropertyPath::Server),
            Some(&value(serde_json::json!({"local": true}))),
            "{}",
            kind.name
        );

        let plain = compile(&kind, "", "");
        assert!(!properties(&plain).contains_key(&PropertyPath::Server));
        assert_eq!(plain.bindings().external_selectors().count(), 0);
    }
}

#[test]
fn bindings_list_the_server_selector_and_redact_names_in_debug_output() {
    for kind in KINDS {
        let compiled = compile(&kind, &named(NAME_CANARY), "");

        let selectors: Vec<_> = compiled
            .bindings()
            .external_selectors()
            .map(|(address, path, selector_kind, selector)| {
                (
                    address.clone(),
                    path.clone(),
                    selector_kind,
                    selector.name().map(str::to_owned),
                )
            })
            .collect();

        assert_eq!(
            selectors,
            [(
                kind.address(),
                PropertyPath::Server,
                SelectorKind::Server,
                Some(NAME_CANARY.to_owned())
            )],
            "{}",
            kind.name
        );
        let debug = format!("{compiled:?} {:?}", compiled.bindings());
        assert!(!debug.contains(NAME_CANARY), "{}", kind.name);
    }
}

#[test]
fn an_ignored_server_is_still_listed_but_is_marked_ignored_for_discovery() {
    for kind in KINDS {
        let compiled = compile(
            &kind,
            &named("edge-1"),
            "          ignore_changes: [server]\n",
        );
        let resource = compiled
            .desired_state()
            .resources()
            .get(&kind.address())
            .unwrap();

        assert!(resource.ignored_changes().contains(&PropertyPath::Server));
        assert_eq!(compiled.bindings().external_selectors().count(), 1);
    }
}

#[test]
fn resolved_identities_enter_the_execution_sidecar_only_after_binding() {
    for kind in KINDS {
        let mut compiled = compile(&kind, &named("edge-1"), "");
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let state = |resolution| {
            RemoteState::try_new(
                instance.clone(),
                [(kind.address(), RemoteObservation::Missing)],
            )
            .expect("remote state")
            .with_external_resolutions([((kind.address(), PropertyPath::Server), resolution)])
            .expect("resolutions")
        };

        assert!(
            compiled
                .bindings()
                .external_id(&kind.address(), &PropertyPath::Server)
                .is_none()
        );
        compiled.bind_external_resolutions(&state(ExternalResolution::Resolved(
            RemoteId::new("server-secret-id").unwrap(),
        )));
        assert!(
            matches!(
                compiled.bindings().external_id(&kind.address(), &PropertyPath::Server),
                Some(ExternalExecutionId::Remote(id)) if id.as_str() == "server-secret-id"
            ),
            "{}",
            kind.name
        );
        let debug = format!(
            "{compiled:?} {:?} {:?}",
            compiled.bindings(),
            compiled
                .bindings()
                .external_id(&kind.address(), &PropertyPath::Server)
        );
        assert!(!debug.contains("secret-id"), "{}", kind.name);

        for blocked in [
            ExternalResolution::Ambiguous,
            ExternalResolution::Unmatched,
            ExternalResolution::Unavailable(RemoteFailureKind::Unavailable),
        ] {
            let mut fresh = compile(&kind, &named("edge-1"), "");
            fresh.bind_external_resolutions(&state(blocked));
            assert!(
                fresh
                    .bindings()
                    .external_id(&kind.address(), &PropertyPath::Server)
                    .is_none(),
                "{}: an unresolved selector never reaches execution",
                kind.name
            );
        }
    }
}

#[test]
fn a_local_resolution_binds_as_the_local_server() {
    for kind in KINDS {
        let mut compiled = compile(&kind, LOCAL, "");
        let instance = InstanceIdentity::parse("https://deploy.example.test").unwrap();
        let remote = RemoteState::try_new(instance, [(kind.address(), RemoteObservation::Missing)])
            .expect("remote state")
            .with_external_resolutions([(
                (kind.address(), PropertyPath::Server),
                ExternalResolution::Local,
            )])
            .expect("resolutions");

        compiled.bind_external_resolutions(&remote);

        assert!(matches!(
            compiled
                .bindings()
                .external_id(&kind.address(), &PropertyPath::Server),
            Some(ExternalExecutionId::Local)
        ));
    }
}
