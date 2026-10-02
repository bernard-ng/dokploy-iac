//! Cross-spec consistency.

use std::collections::{BTreeMap, BTreeSet};

use crate::error::Issue;
use crate::model::{KindSpec, Scope};
use crate::validate::validate_spec;

/// Every kind spec, validated and cross-checked.
#[derive(Clone, Debug)]
pub struct SpecRegistry {
    kinds: BTreeMap<String, KindSpec>,
}

impl SpecRegistry {
    /// Validates each spec and the relations between them.
    pub fn from_specs(
        specs: Vec<KindSpec>,
        shared_types: &BTreeSet<String>,
    ) -> Result<Self, Vec<Issue>> {
        let mut issues = Vec::new();
        let mut kinds: BTreeMap<String, KindSpec> = BTreeMap::new();
        for spec in specs {
            issues.extend(validate_spec(&spec, shared_types));
            if kinds.contains_key(&spec.kind) {
                issues.push(Issue::new(
                    &spec.kind,
                    "kind",
                    "defined by more than one spec",
                ));
                continue;
            }
            kinds.insert(spec.kind.clone(), spec);
        }
        issues.extend(check_relations(&kinds));
        if issues.is_empty() {
            Ok(Self { kinds })
        } else {
            issues.sort();
            Err(issues)
        }
    }

    /// The spec for one kind.
    #[must_use]
    pub fn get(&self, kind: &str) -> Option<&KindSpec> {
        self.kinds.get(kind)
    }

    /// Every spec, ordered by kind id.
    pub fn kinds(&self) -> impl Iterator<Item = &KindSpec> {
        self.kinds.values()
    }

    /// The kinds nested directly under `kind`.
    #[must_use]
    pub fn children_of(&self, kind: &str) -> Vec<&KindSpec> {
        self.kinds
            .values()
            .filter(|spec| spec.parent.as_deref() == Some(kind))
            .collect()
    }

    /// The top-level kinds of a document scope.
    #[must_use]
    pub fn roots(&self, scope: Scope) -> Vec<&KindSpec> {
        self.kinds
            .values()
            .filter(|spec| spec.scope == scope && spec.parent.is_none())
            .collect()
    }
}

fn check_relations(kinds: &BTreeMap<String, KindSpec>) -> Vec<Issue> {
    let mut issues = Vec::new();
    let mut root_sections: BTreeSet<(Scope_, &str)> = BTreeSet::new();
    for spec in kinds.values() {
        match &spec.parent {
            None => {
                if !root_sections.insert((Scope_::from(spec.scope), spec.section.as_str())) {
                    issues.push(Issue::new(
                        &spec.kind,
                        "section",
                        format!(
                            "`{}` is already a top-level section in this scope",
                            spec.section
                        ),
                    ));
                }
            }
            Some(parent) => match kinds.get(parent) {
                None => issues.push(Issue::new(
                    &spec.kind,
                    "parent",
                    format!("unknown kind `{parent}`"),
                )),
                Some(parent_spec) => {
                    if parent_spec.scope != spec.scope {
                        issues.push(Issue::new(
                            &spec.kind,
                            "scope",
                            "differs from its parent's scope",
                        ));
                    }
                    let listed = parent_spec
                        .children
                        .iter()
                        .find(|child| child.kind == spec.kind);
                    match listed {
                        None => issues.push(Issue::new(
                            &spec.kind,
                            "parent",
                            format!("`{parent}` does not list it under children"),
                        )),
                        Some(child) if child.section != spec.section => issues.push(Issue::new(
                            &spec.kind,
                            "section",
                            format!(
                                "`{parent}` nests it under `{}`, the spec says `{}`",
                                child.section, spec.section
                            ),
                        )),
                        Some(_) => {}
                    }
                }
            },
        }
        for child in &spec.children {
            match kinds.get(&child.kind) {
                None => issues.push(Issue::new(
                    &spec.kind,
                    "children",
                    format!("unknown child kind `{}`", child.kind),
                )),
                Some(child_spec) if child_spec.parent.as_deref() != Some(spec.kind.as_str()) => {
                    issues.push(Issue::new(
                        &spec.kind,
                        "children",
                        format!("`{}` does not name it as parent", child.kind),
                    ));
                }
                Some(_) => {}
            }
        }
        // Parent chains must end.
        let mut seen = BTreeSet::new();
        let mut current = spec.parent.as_deref();
        while let Some(name) = current {
            if !seen.insert(name) || name == spec.kind {
                issues.push(Issue::new(&spec.kind, "parent", "containment cycle"));
                break;
            }
            current = kinds.get(name).and_then(|parent| parent.parent.as_deref());
        }
    }
    issues
}

/// `Scope` is not `Ord`; this local mirror lets root sections live in a set.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[allow(non_camel_case_types)]
enum Scope_ {
    Project,
    Settings,
}

impl From<Scope> for Scope_ {
    fn from(scope: Scope) -> Self {
        match scope {
            Scope::Project => Self::Project,
            Scope::Settings => Self::Settings,
        }
    }
}
