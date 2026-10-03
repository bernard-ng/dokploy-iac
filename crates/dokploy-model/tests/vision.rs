//! The vision documents (`docs/vision/`) are the target of milestone M3 and M5: every field they
//! show is meant to become a valid version 2 document. This is the ratchet that measures it.
//!
//! Each vision document is parsed against the repository's own specs, and what the parser
//! rejects is compared with the checked-in gap list in `docs/vision/gaps/`. The test fails when
//! the two differ, in either direction:
//!
//! - a gap that is **gone** means a spec learned a field: regenerate the list, and the diff is
//!   the progress;
//! - a gap that is **new** means a spec or the parser lost a field, or the vision document
//!   changed: either is a decision, and the list records it.
//!
//! Regenerate with `UPDATE_VISION_GAPS=1 cargo test -p dokploy-model --test vision`. The goal is
//! an empty list for both documents.

mod common;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use dokploy_model::Document;

fn vision(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../docs/vision")
        .join(name)
}

/// One line per distinct problem: the stable code and the dotted path, never a line number.
fn gaps(document: &str) -> String {
    let text = std::fs::read_to_string(vision(document)).expect("the vision document is readable");
    let lines: BTreeSet<String> = match Document::parse(&text, common::repository()) {
        Ok(_) => BTreeSet::new(),
        Err(diagnostics) => diagnostics
            .iter()
            .map(|diagnostic| format!("{} {}", diagnostic.code, diagnostic.path))
            .collect(),
    };
    let mut out = String::new();
    for line in lines {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

fn ratchet(document: &str, list: &str) {
    let actual = gaps(document);
    let path = vision("gaps").join(list);
    if std::env::var_os("UPDATE_VISION_GAPS").is_some() {
        std::fs::write(&path, &actual).expect("the gap list is writable");
        return;
    }
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    if recorded == actual {
        return;
    }
    let recorded: BTreeSet<&str> = recorded.lines().collect();
    let actual_set: BTreeSet<&str> = actual.lines().collect();
    let closed: Vec<_> = recorded.difference(&actual_set).collect();
    let opened: Vec<_> = actual_set.difference(&recorded).collect();
    panic!(
        "{document} no longer matches docs/vision/gaps/{list}.\n\
         closed ({}): {closed:#?}\nopened ({}): {opened:#?}\n\
         Run `UPDATE_VISION_GAPS=1 cargo test -p dokploy-model --test vision` and commit the list.",
        closed.len(),
        opened.len(),
    );
}

#[test]
fn the_project_vision_gap_list_is_current() {
    ratchet("dokploy.full.yaml", "project.txt");
}

#[test]
fn the_settings_vision_gap_list_is_current() {
    ratchet("dokploy.settings.full.yaml", "settings.txt");
}
