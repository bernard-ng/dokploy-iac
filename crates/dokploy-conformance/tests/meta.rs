//! The suite itself is tested: a spec that is wrong in the ways specs go wrong must fail it,
//! and a spec with a `create_only` field must pass the replacement scenarios.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dokploy_conformance::{Outcome, ScenarioResult, Suite, report};
use dokploy_spec::{SpecRegistry, parse_spec};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn registry_spec() -> String {
    std::fs::read_to_string(root().join("specs/settings/registry.yaml")).expect("the spec exists")
}

async fn run(text: &str, kind: &str) -> Vec<ScenarioResult> {
    let spec = parse_spec(text).expect("the spec parses");
    let specs =
        SpecRegistry::from_specs(vec![spec], &BTreeSet::new()).expect("the spec is consistent");
    Suite::new(specs, root().join("fixtures/api/live/v0.30.6"))
        .run(kind)
        .await
}

fn failed<'a>(results: &'a [ScenarioResult], scenario: &str) -> Option<&'a str> {
    results
        .iter()
        .find(|r| r.scenario == scenario)
        .and_then(|r| match &r.outcome {
            Outcome::Fail(reason) => Some(reason.as_str()),
            _ => None,
        })
}

fn passed(results: &[ScenarioResult], scenario: &str) -> bool {
    results
        .iter()
        .any(|r| r.scenario == scenario && r.outcome == Outcome::Pass)
}

#[tokio::test]
async fn the_unmodified_spec_passes() {
    let results = run(&registry_spec(), "registry").await;
    assert!(
        results
            .iter()
            .all(|r| !matches!(r.outcome, Outcome::Fail(_))),
        "{}",
        report(&results)
    );
}

#[tokio::test]
async fn a_wrong_identity_pointer_is_caught() {
    let broken = registry_spec().replace("from_response: /registryId", "from_response: /nope");
    let results = run(&broken, "registry").await;

    assert!(failed(&results, "create").is_some(), "{}", report(&results));
    assert!(
        failed(&results, "unknown_create_after").is_some()
            || failed(&results, "unknown_create_before").is_some()
            || !passed(&results, "converges")
    );
}

#[tokio::test]
async fn a_write_group_on_the_wrong_operation_is_caught() {
    // The lint cannot tell that `registry.remove` does not take these fields; the contract can.
    let broken = registry_spec().replace("  - op: registry.update", "  - op: registry.remove");
    let results = run(&broken, "registry").await;

    assert!(
        failed(&results, "update:username").is_some(),
        "{}",
        report(&results)
    );
    assert!(
        passed(&results, "create"),
        "creating does not use the group"
    );
}

#[tokio::test]
async fn a_field_mapped_to_a_name_the_contract_does_not_have_is_caught() {
    let broken = registry_spec().replace("api: registryUrl", "api: registryAddress");
    let results = run(&broken, "registry").await;

    let reason = failed(&results, "create");
    assert!(
        reason.is_some_and(|reason| reason.contains("rejected") || reason.contains("failed")),
        "{}",
        report(&results)
    );
}

#[tokio::test]
async fn every_secret_field_is_rotated_and_scanned() {
    let results = run(&registry_spec(), "registry").await;
    assert!(passed(&results, "update:password"), "{}", report(&results));
}

#[tokio::test]
async fn a_create_only_field_is_exercised_by_the_replacement_scenarios() {
    let gadget = "\
kind: gadget
scope: settings
section: gadgets
title: Gadget
class: managed
identity:
  key: name
  collision: [name]
  address: \"gadget.{key}\"
api:
  id: tagId
  create: { op: tag.create }
  update: { op: tag.update }
  remove: { op: tag.remove }
  read:
    list: { op: tag.all, authority: authoritative }
    one: { op: tag.one, id_param: tagId, agree: true }
  create_identity:
    from_response: /tagId
fields:
  name:
    type: text
    min_len: 1
    default: key
  color:
    type: text
    mutability: create_only
    nullable: true
write:
  - op: tag.update
    fields: [name]
    shape: partial
ledger:
  readonly: [createdAt, organizationId]
";
    let results = run(gadget, "gadget").await;

    assert!(passed(&results, "replace:color"), "{}", report(&results));
    assert!(
        results
            .iter()
            .all(|r| !matches!(r.outcome, Outcome::Fail(_))),
        "{}",
        report(&results)
    );
}

#[tokio::test]
async fn a_partial_collection_is_not_proof_of_absence() {
    let partial = tag_like().replace("authority: authoritative", "authority: partial");
    let results = run(&partial, "gadget").await;

    assert!(passed(&results, "partial_authority"), "{}", report(&results));
    // Absence can never be proven, so nothing can be created: the apply is blocked, which is
    // the correct outcome and exactly what fails the scenarios that need a create to start.
    let reason = failed(&results, "create").expect("a create cannot be proven necessary");
    assert!(reason.contains("DOKPLAN003"), "{reason}");
}

/// A settings kind on the tag operations, for tests that need a variation of one.
fn tag_like() -> String {
    "\
kind: gadget
scope: settings
section: gadgets
title: Gadget
class: managed
identity:
  key: name
  collision: [name]
  address: \"gadget.{key}\"
api:
  id: tagId
  create: { op: tag.create }
  update: { op: tag.update }
  remove: { op: tag.remove }
  read:
    list: { op: tag.all, authority: authoritative }
    one: { op: tag.one, id_param: tagId, agree: true }
  create_identity:
    from_response: /tagId
fields:
  name:
    type: text
    min_len: 1
    default: key
  color:
    type: text
    nullable: true
write:
  - op: tag.update
    fields: [name, color]
    shape: partial
ledger:
  readonly: [createdAt, organizationId]
"
    .to_owned()
}
