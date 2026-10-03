//! The generated request contracts say what each operation accepts.

use dokploy_api::{BodyShape, request_contract};

#[test]
fn a_create_names_its_body_fields_and_which_are_required() {
    let contract = request_contract("registry.create").expect("registry.create is in the contract");
    assert!(contract.query().is_empty());
    let BodyShape::Object(fields) = contract.body() else {
        panic!("registry.create takes an object body");
    };
    let required: Vec<_> = fields
        .iter()
        .filter(|field| field.required())
        .map(|field| field.name())
        .collect();
    assert_eq!(
        required,
        [
            "imagePrefix",
            "password",
            "registryName",
            "registryType",
            "registryUrl",
            "username"
        ]
    );
    assert_eq!(
        contract
            .body_field("serverId")
            .map(|field| field.required()),
        Some(false)
    );
    assert!(contract.body_field("registryId").is_none());
}

#[test]
fn a_read_names_its_query_parameters() {
    let contract = request_contract("registry.one").expect("registry.one is in the contract");
    assert_eq!(contract.body(), BodyShape::None);
    let query: Vec<_> = contract
        .query()
        .iter()
        .map(|field| (field.name(), field.required()))
        .collect();
    assert_eq!(query, [("registryId", true)]);
}

#[test]
fn a_listing_takes_nothing() {
    let contract = request_contract("tag.all").expect("tag.all is in the contract");
    assert!(contract.query().is_empty());
    assert_eq!(contract.body(), BodyShape::None);
}

#[test]
fn an_operation_outside_the_contract_has_none() {
    assert!(request_contract("registry.explode").is_none());
}
