use std::collections::BTreeMap;

use dokploy_core::{
    ComparableValue, PropertyObservation, PropertyPath, RemoteObservation, RemoteResource,
    RemoteState,
};
use dokploy_state::{InstanceIdentity, RemoteId, ResourceAddress};

#[test]
fn remote_binding_receipt_is_deterministic_and_binds_key_identity_and_values() {
    let first = snapshot("remote-1", "stable");
    let same = snapshot("remote-1", "stable");
    let changed_id = snapshot("remote-2", "stable");
    let changed_value = snapshot("remote-1", "changed");

    let receipt = first.binding_receipt(&[7; 32]);

    assert_eq!(receipt, same.binding_receipt(&[7; 32]));
    assert_ne!(receipt, first.binding_receipt(&[8; 32]));
    assert_ne!(receipt, changed_id.binding_receipt(&[7; 32]));
    assert_ne!(receipt, changed_value.binding_receipt(&[7; 32]));
}

fn snapshot(remote_id: &str, description: &str) -> RemoteState {
    let address: ResourceAddress = "project.platform".parse().expect("address is valid");
    let properties = BTreeMap::from([(
        PropertyPath::Description,
        PropertyObservation::Known(
            ComparableValue::try_from_json(serde_json::json!(description))
                .expect("description is comparable"),
        ),
    )]);

    RemoteState::try_new(
        InstanceIdentity::parse("https://deploy.example.test").expect("instance is valid"),
        [(
            address,
            RemoteObservation::Present(RemoteResource::new(
                RemoteId::new(remote_id).expect("remote ID is valid"),
                properties,
            )),
        )],
    )
    .expect("remote snapshot is valid")
}
