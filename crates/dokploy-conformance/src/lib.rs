//! The conformance suite (ADR 0015): scenarios generated from a kind's spec and run against
//! the simulator.
//!
//! A kind that has a spec has a test suite, and no one writes it. [`Suite::run`] derives a
//! case from the spec (sample values for each field, the write group each belongs to, the
//! secrets), then runs, each in a world of its own with a fresh simulator and workspace:
//!
//! - create, create with only the required fields, and convergence (the next plan is empty);
//! - one in-place update per field, checking that exactly the field's write group is sent;
//! - one replacement per `create_only` field, and one drift per field;
//! - delete, and delete of a protected resource;
//! - an unmanaged resource with the same identity, two of them, and an unreadable remote;
//! - a removal Dokploy acknowledges and does not apply, a create whose identity is ambiguous
//!   because another client made the same thing, and a collection or a direct read that names
//!   another parent;
//! - a selector that names nothing, or two things;
//! - a rejected create, a declined plan, and a create whose follow-up write (for the fields the
//!   create operation does not accept) is rejected or lost;
//! - a mutation interrupted before and after Dokploy applies it, for create, update, and
//!   delete, each settled by recovery without repeating it.
//!
//! Every scenario finishes by scanning the workspace and everything it printed for the
//! suite's canary secrets. A scenario that does not apply to the kind (no `create_only`
//! field, an authoritative collection) is skipped with its reason, never silently.

mod case;
mod scenarios;
mod world;

use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use dokploy_spec::{Mutability, SpecRegistry};

use case::Case;
use scenarios::{FollowUp, Step, Verdict};
use world::{Check, World};

/// How one scenario ended.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Outcome {
    /// The engine and the simulator behaved as the spec says.
    Pass,
    /// The scenario does not apply to this kind.
    Skip(String),
    /// They did not.
    Fail(String),
}

/// One scenario of one kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScenarioResult {
    /// The kind.
    pub kind: String,
    /// The scenario, such as `update:username`.
    pub scenario: String,
    /// How it ended.
    pub outcome: Outcome,
}

/// The suite over a set of specs.
pub struct Suite {
    specs: Arc<SpecRegistry>,
    fixtures: PathBuf,
    arms: std::collections::BTreeMap<String, String>,
}

impl Suite {
    /// Runs scenarios against `specs`, with the simulator's response shapes taken from the
    /// fixtures in `fixtures` (`fixtures/api/live/<version>`).
    #[must_use]
    pub fn new(specs: SpecRegistry, fixtures: PathBuf) -> Self {
        Self {
            specs: Arc::new(specs),
            fixtures,
            arms: std::collections::BTreeMap::new(),
        }
    }

    /// Exercises the union `field` through `arm` instead of the first arm the spec names, in
    /// every kind that has it. The spec is untouched, so the columns of the other arms exist.
    #[must_use]
    pub fn exercising(mut self, field: &str, arm: &str) -> Self {
        self.arms.insert(field.to_owned(), arm.to_owned());
        self
    }

    /// The kinds the suite can exercise: those with full coverage.
    #[must_use]
    pub fn kinds(&self) -> Vec<String> {
        self.specs
            .kinds()
            .filter(|spec| spec.coverage == dokploy_spec::Coverage::Full)
            .map(|spec| spec.kind.clone())
            .collect()
    }

    /// Runs every scenario for one kind.
    ///
    /// # Panics
    ///
    /// When `kind` has no spec.
    pub async fn run(&self, kind: &str) -> Vec<ScenarioResult> {
        let spec = self.specs.get(kind).expect("a kind with a spec");
        if spec.parents.len() > 1 {
            // A kind that lives under several parents is exercised under each of them.
            let mut results = Vec::new();
            for parent in &spec.parents {
                results.extend(self.run_under(kind, Some(parent)).await);
            }

            return results;
        }

        self.run_under(kind, None).await
    }

    async fn run_under(&self, kind: &str, parent: Option<&str>) -> Vec<ScenarioResult> {
        let spec = self.specs.get(kind).expect("a kind with a spec");
        let label = parent.map_or_else(|| kind.to_owned(), |parent| format!("{kind}@{parent}"));
        let kind = label.as_str();
        let case = match Case::new(spec, &self.specs, parent, &self.arms) {
            Ok(case) => case,
            Err(reason) => {
                return vec![ScenarioResult {
                    kind: kind.to_owned(),
                    scenario: "all".to_owned(),
                    outcome: Outcome::Skip(reason),
                }];
            }
        };

        let mut results = Vec::new();
        let mut record = |scenario: String, outcome: Check<Verdict>, world: &World<'_>| {
            let outcome = match outcome.and_then(|verdict| world.scan().map(|()| verdict)) {
                Ok(Verdict::Pass) => Outcome::Pass,
                Ok(Verdict::Skip(reason)) => Outcome::Skip(reason),
                Err(reason) => Outcome::Fail(reason),
            };
            results.push(ScenarioResult {
                kind: kind.to_owned(),
                scenario,
                outcome,
            });
        };
        macro_rules! scenario {
            ($name:expr, |$w:ident| $call:expr) => {{
                let $w = World::new(&case, &self.specs, &self.fixtures);
                let outcome = $call.await;
                record($name.to_owned(), outcome, &$w);
            }};
        }

        scenario!("create", |w| scenarios::create(&w));
        scenario!("create_minimal", |w| scenarios::create_minimal(&w));
        scenario!("converges", |w| scenarios::converges(&w));
        for field in &case.fields {
            let name = field.name.clone();
            if field.mutability == Mutability::CreateOnly {
                scenario!(format!("replace:{name}"), |w| scenarios::replace(&w, &name));
            } else {
                scenario!(format!("update:{name}"), |w| scenarios::update(&w, &name));
            }
            if !field.secret {
                scenario!(format!("drift:{name}"), |w| scenarios::drift(&w, &name));
            }
        }
        scenario!("delete", |w| scenarios::delete(&w));
        scenario!("protected_delete", |w| scenarios::protected_delete(&w));
        scenario!("unmanaged_collision", |w| scenarios::unmanaged_collision(
            &w, 1
        ));
        scenario!("ambiguous_collision", |w| scenarios::unmanaged_collision(
            &w, 2
        ));
        scenario!("partial_authority", |w| scenarios::partial_authority(&w));
        scenario!("read_failure", |w| scenarios::read_failure(&w));
        scenario!("removal_not_applied", |w| scenarios::removal_not_applied(
            &w
        ));
        scenario!("identity_ambiguous", |w| scenarios::identity_ambiguous(&w));
        scenario!("foreign_collection_item", |w| scenarios::foreign_parent(
            &w, false
        ));
        scenario!("foreign_direct_read", |w| scenarios::foreign_parent(
            &w, true
        ));
        scenario!("declined", |w| scenarios::declined(&w));
        scenario!("rejected_create", |w| scenarios::rejected_create(&w));
        scenario!("selector_unmatched", |w| scenarios::selector_unresolved(
            &w, false
        ));
        scenario!("selector_ambiguous", |w| scenarios::selector_unresolved(
            &w, true
        ));
        for (name, how) in [
            ("follow_up_rejected", FollowUp::Rejected),
            ("follow_up_lost_before", FollowUp::LostBefore),
            ("follow_up_lost_after", FollowUp::LostAfter),
        ] {
            scenario!(name, |w| scenarios::follow_up(&w, how));
        }
        for (label, step) in [
            ("create", Step::Create),
            ("update", Step::Update),
            ("delete", Step::Delete),
        ] {
            for (when, after) in [("before", false), ("after", true)] {
                scenario!(format!("unknown_{label}_{when}"), |w| {
                    scenarios::interrupted(&w, step, after)
                });
            }
        }

        results
    }

    /// Runs every kind the suite can exercise.
    pub async fn run_all(&self) -> Vec<ScenarioResult> {
        let mut results = Vec::new();
        for kind in self.kinds() {
            results.extend(self.run(&kind).await);
        }

        results
    }
}

/// The results as a table, one row per kind, for a log or a report.
#[must_use]
pub fn report(results: &[ScenarioResult]) -> String {
    let mut kinds: Vec<&str> = results.iter().map(|r| r.kind.as_str()).collect();
    kinds.dedup();
    let mut text = String::from("kind            pass  skip  fail\n");
    for kind in kinds {
        let count = |wanted: fn(&Outcome) -> bool| {
            results
                .iter()
                .filter(|r| r.kind == kind && wanted(&r.outcome))
                .count()
        };
        let _ = writeln!(
            text,
            "{kind:<15} {:>4}  {:>4}  {:>4}",
            count(|o| matches!(o, Outcome::Pass)),
            count(|o| matches!(o, Outcome::Skip(_))),
            count(|o| matches!(o, Outcome::Fail(_))),
        );
    }
    for result in results {
        match &result.outcome {
            Outcome::Fail(reason) => {
                let _ = writeln!(text, "FAIL {} {}: {reason}", result.kind, result.scenario);
            }
            Outcome::Skip(reason) => {
                let _ = writeln!(text, "skip {} {}: {reason}", result.kind, result.scenario);
            }
            Outcome::Pass => {}
        }
    }

    text
}
