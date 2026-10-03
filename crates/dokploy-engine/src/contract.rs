//! What creating a kind really needs: the spec says which fields have no default, and the
//! create operation's own contract says which of them it will not accept being left out.

use dokploy_api::{BodyShape, request_contract};
use dokploy_core::{MutationContract, PropertyPath};
use dokploy_spec::{KindSpec, PropertyInfo};

/// Whether the property must be given to create the kind: the spec has no default for it and the
/// create operation requires it. A property the create operation does not require (it does not
/// accept it, or takes it optionally) is written after the create, or left to Dokploy's own
/// default, when the document leaves it out.
#[must_use]
pub fn required_at_creation(spec: &KindSpec, info: &PropertyInfo) -> bool {
    info.is_required_on_create() && create_requires(spec, info)
}

fn create_requires(spec: &KindSpec, info: &PropertyInfo) -> bool {
    let Some(create) = &spec.api.create else {
        return true;
    };
    match request_contract(&create.op) {
        Some(contract) if matches!(contract.body(), BodyShape::Object(_)) => contract
            .body_field(info.request_key())
            .is_some_and(|field| field.required()),
        _ => true,
    }
}

/// The planner's mutation contract for a kind: the spec's, with creation requirements narrowed
/// to what the create operation needs.
pub(crate) fn mutation_contract(spec: &KindSpec) -> MutationContract {
    let mut contract = MutationContract::from_spec(spec);
    for info in spec.properties() {
        if info.is_required_on_create() && !create_requires(spec, &info) {
            contract = contract.optional_on_create(&PropertyPath::from_property_info(info));
        }
    }

    contract
}
