//! `cargo xtask goldens`: the legacy-test ledger (ADR 0016).
//!
//! The first engine's tests define what "done" means for a ported kind. This module
//! mines them into `goldens/<bucket>.json`, one file per kind or concern, listing
//! every test function with the operations, expected request lines, fixtures, and
//! canned-data constants it uses. A human then classifies each scenario:
//!
//! - `pending`: not yet reproduced by the spec engine;
//! - `covered`: reproduced, with `covered_by` naming the conformance scenario;
//! - `dropped`: intentionally not carried over, with a `reason`.
//!
//! `--check` fails when a legacy test is not in the ledger, when a ledger entry
//! drifted from its source, and when a `pending` scenario's test was deleted, so
//! legacy code cannot be removed before its behavior is covered or dropped.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path, PathBuf};

use proc_macro2::{TokenStream, TokenTree};
use quote::ToTokens;
use serde::{Deserialize, Serialize};
use syn::{Item, LitStr};

const LEDGER_DIR: &str = "goldens";
const SOURCES_FILE: &str = "sources.json";

/// Conformance categories (ADR 0015), plus the concerns the legacy tests also cover.
const CATEGORIES: [&str; 20] = [
    "create",
    "noop",
    "update",
    "replace",
    "drift",
    "delete",
    "move",
    "outcome-unknown",
    "recovery",
    "ordering",
    "selector",
    "import",
    "canary",
    "read-model",
    "document",
    "wire",
    "state",
    "plan",
    "live",
    "uncategorized",
];

const LAYERS: [&str; 9] = [
    "cli-remote",
    "cli-desired",
    "cli-apply",
    "cli-recovery",
    "cli-import",
    "config",
    "sdk",
    "live",
    "unit",
];

const STATUSES: [&str; 3] = ["pending", "covered", "dropped"];

/// Which legacy files are mined, and which test-bearing files are deliberately not.
#[derive(Debug, Deserialize, Serialize)]
struct Sources {
    sources: Vec<SourceEntry>,
    excluded: Vec<Exclusion>,
}

#[derive(Debug, Deserialize, Serialize)]
struct SourceEntry {
    path: String,
    bucket: String,
    layer: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct Exclusion {
    /// A file, or a directory prefix ending in `/`.
    path: String,
    reason: String,
}

/// One bucket file: every scenario of one kind or concern.
#[derive(Debug, Deserialize, Serialize)]
struct Ledger {
    bucket: String,
    scenarios: Vec<Scenario>,
}

/// One legacy test. The first block of fields is mined and refreshed by every
/// run; `category` (initially a guess from the name) and the last block are
/// classified by a human and never overwritten.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct Scenario {
    id: String,
    file: String,
    test: String,
    layer: String,
    /// The test is `#[ignore]`d live acceptance.
    live: bool,
    /// Dokploy operations named in request literals.
    operations: Vec<String>,
    /// Request-shaped string literals (method, path, body) in the test body.
    requests: Vec<String>,
    /// Recorded fixtures the test loads, repo-relative.
    fixtures: Vec<String>,
    /// File-level constants the test reads (canned bodies and documents).
    data: Vec<String>,
    category: String,
    status: String,
    covered_by: Option<String>,
    reason: Option<String>,
}

impl Scenario {
    fn mined_fields_differ(&self, other: &Self) -> bool {
        (&self.file, &self.test, &self.layer, self.live)
            != (&other.file, &other.test, &other.layer, other.live)
            || self.operations != other.operations
            || self.requests != other.requests
            || self.fixtures != other.fixtures
            || self.data != other.data
    }
}

/// One row of the printable summary.
#[derive(Debug, Default)]
struct Tally {
    total: usize,
    pending: usize,
    covered: usize,
    dropped: usize,
    live: usize,
}

/// The outcome of an extraction run.
#[derive(Debug)]
pub struct GoldensExtractReport {
    pub scenarios: usize,
    pub added: usize,
    pub files: usize,
}

/// The outcome of a check run.
#[derive(Debug)]
pub struct GoldensReport {
    pub table: String,
    pub failures: Vec<String>,
}

/// Mines the sources listed in `goldens/sources.json` into `goldens/<bucket>.json`,
/// keeping every human classification. Scenarios whose test no longer exists are
/// kept when classified and are an error while `pending`.
pub fn run_goldens_extract(
    root: &Path,
) -> Result<GoldensExtractReport, Box<dyn std::error::Error>> {
    let sources = load_sources(root)?;
    let found = mine(root, &sources)?;
    let existing = load_ledgers(root)?;

    let mut by_id: BTreeMap<String, (String, Scenario)> = BTreeMap::new();
    for (bucket, scenario) in existing {
        by_id.insert(scenario.id.clone(), (bucket, scenario));
    }

    let mut added = 0;
    let mut seen = BTreeSet::new();
    for (bucket, mined) in found {
        seen.insert(mined.id.clone());
        match by_id.get_mut(&mined.id) {
            Some((_, kept)) => {
                *kept = Scenario {
                    category: kept.category.clone(),
                    status: kept.status.clone(),
                    covered_by: kept.covered_by.clone(),
                    reason: kept.reason.clone(),
                    ..mined
                };
            }
            None => {
                added += 1;
                by_id.insert(mined.id.clone(), (bucket, mined));
            }
        }
    }

    let orphaned: Vec<&str> = by_id
        .iter()
        .filter(|(id, (_, scenario))| !seen.contains(*id) && scenario.status == "pending")
        .map(|(id, _)| id.as_str())
        .collect();
    if !orphaned.is_empty() {
        return Err(format!(
            "{} pending scenario(s) lost their test; restore it or classify the scenario as dropped first: {}",
            orphaned.len(),
            orphaned.join(", ")
        )
        .into());
    }

    let mut buckets: BTreeMap<String, Vec<Scenario>> = BTreeMap::new();
    for (_, (bucket, scenario)) in by_id {
        buckets.entry(bucket).or_default().push(scenario);
    }
    let directory = root.join(LEDGER_DIR);
    let mut total = 0;
    let mut written = BTreeSet::new();
    for (bucket, mut scenarios) in buckets {
        scenarios.sort_by(|left, right| left.id.cmp(&right.id));
        total += scenarios.len();
        let ledger = Ledger {
            bucket: bucket.clone(),
            scenarios,
        };
        let mut text = serde_json::to_string_pretty(&ledger)?;
        text.push('\n');
        let file = format!("{bucket}.json");
        fs::write(directory.join(&file), text)?;
        written.insert(file);
    }
    for entry in fs::read_dir(&directory)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && name != SOURCES_FILE && !written.contains(&name) {
            fs::remove_file(directory.join(name))?;
        }
    }

    Ok(GoldensExtractReport {
        scenarios: total,
        added,
        files: written.len(),
    })
}

/// Verifies the ledger against the tree.
pub fn run_goldens_check(root: &Path) -> Result<GoldensReport, Box<dyn std::error::Error>> {
    let mut failures = Vec::new();
    let sources = load_sources(root)?;
    let ledgers = load_ledgers(root)?;

    for entry in &sources.sources {
        if !LAYERS.contains(&entry.layer.as_str()) {
            failures.push(format!("{}: unknown layer `{}`", entry.path, entry.layer));
        }
        if entry.bucket == "sources" {
            failures.push(format!("{}: `sources` is not a valid bucket", entry.path));
        }
    }
    for path in unmapped_test_files(root, &sources)? {
        failures.push(format!(
            "{path}: contains tests but is neither a source nor excluded in goldens/{SOURCES_FILE}"
        ));
    }

    let found = mine(root, &sources)?;
    let mut mined: BTreeMap<String, Scenario> = BTreeMap::new();
    for (_, scenario) in found {
        mined.insert(scenario.id.clone(), scenario);
    }

    let mut ids = BTreeSet::new();
    let mut tallies: BTreeMap<String, Tally> = BTreeMap::new();
    for (bucket, scenario) in &ledgers {
        let id = &scenario.id;
        if !ids.insert(id.clone()) {
            failures.push(format!("{id}: listed twice"));
        }
        validate_scenario(bucket, scenario, &mut failures);

        let tally = tallies.entry(bucket.clone()).or_default();
        tally.total += 1;
        tally.live += usize::from(scenario.live);
        match scenario.status.as_str() {
            "pending" => tally.pending += 1,
            "covered" => tally.covered += 1,
            "dropped" => tally.dropped += 1,
            _ => {}
        }

        match mined.get(id) {
            Some(source) if scenario.mined_fields_differ(source) => failures.push(format!(
                "{id}: out of date with its test; run `cargo xtask goldens`"
            )),
            Some(_) => {}
            None if scenario.status == "pending" => failures.push(format!(
                "{id}: its test is gone but the scenario is still pending; port it or classify it as dropped"
            )),
            None => {}
        }
    }
    for id in mined.keys() {
        if !ids.contains(id) {
            failures.push(format!(
                "{id}: a legacy test missing from the ledger; run `cargo xtask goldens`"
            ));
        }
    }

    let mut table = String::new();
    writeln!(
        table,
        "{:<14} {:>6} {:>8} {:>8} {:>8} {:>5}",
        "bucket", "total", "pending", "covered", "dropped", "live"
    )?;
    for (bucket, tally) in &tallies {
        writeln!(
            table,
            "{:<14} {:>6} {:>8} {:>8} {:>8} {:>5}",
            bucket, tally.total, tally.pending, tally.covered, tally.dropped, tally.live
        )?;
    }

    Ok(GoldensReport { table, failures })
}

fn validate_scenario(bucket: &str, scenario: &Scenario, failures: &mut Vec<String>) {
    let id = &scenario.id;
    if !CATEGORIES.contains(&scenario.category.as_str()) {
        failures.push(format!("{id}: unknown category `{}`", scenario.category));
    }
    if !LAYERS.contains(&scenario.layer.as_str()) {
        failures.push(format!("{id}: unknown layer `{}`", scenario.layer));
    }
    if !STATUSES.contains(&scenario.status.as_str()) {
        failures.push(format!("{id}: unknown status `{}`", scenario.status));
    }
    let blank = |value: &Option<String>| value.as_deref().is_none_or(|text| text.trim().is_empty());
    match scenario.status.as_str() {
        "covered" if blank(&scenario.covered_by) => {
            failures.push(format!("{id}: covered without `covered_by`"));
        }
        "dropped" if blank(&scenario.reason) => {
            failures.push(format!("{id}: dropped without a `reason`"));
        }
        "pending" if scenario.covered_by.is_some() || scenario.reason.is_some() => {
            failures.push(format!(
                "{id}: pending scenarios carry no `covered_by` or `reason`"
            ));
        }
        _ => {}
    }
    if bucket.is_empty() {
        failures.push(format!("{id}: empty bucket"));
    }
}

fn load_sources(root: &Path) -> Result<Sources, Box<dyn std::error::Error>> {
    let path = root.join(LEDGER_DIR).join(SOURCES_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    Ok(serde_json::from_str(&text)
        .map_err(|error| format!("cannot parse {}: {error}", path.display()))?)
}

/// Every ledger scenario with the bucket named by its file.
fn load_ledgers(root: &Path) -> Result<Vec<(String, Scenario)>, Box<dyn std::error::Error>> {
    let directory = root.join(LEDGER_DIR);
    let mut names = Vec::new();
    for entry in fs::read_dir(&directory)? {
        let name = entry?.file_name().to_string_lossy().into_owned();
        if name.ends_with(".json") && name != SOURCES_FILE {
            names.push(name);
        }
    }
    names.sort();

    let mut scenarios = Vec::new();
    for name in names {
        let path = directory.join(&name);
        let ledger: Ledger = serde_json::from_str(&fs::read_to_string(&path)?)
            .map_err(|error| format!("cannot parse {}: {error}", path.display()))?;
        let bucket = name.trim_end_matches(".json");
        if ledger.bucket != bucket {
            return Err(format!(
                "{}: declares bucket `{}` but the file is `{name}`",
                path.display(),
                ledger.bucket
            )
            .into());
        }
        for scenario in ledger.scenarios {
            scenarios.push((bucket.to_owned(), scenario));
        }
    }
    Ok(scenarios)
}

/// Mines every source file, returning `(bucket, scenario)` pairs.
fn mine(
    root: &Path,
    sources: &Sources,
) -> Result<Vec<(String, Scenario)>, Box<dyn std::error::Error>> {
    let mut scenarios = Vec::new();
    for entry in &sources.sources {
        let path = root.join(&entry.path);
        if !path.exists() {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        for found in mine_file(&entry.path, &text, &entry.layer)
            .map_err(|error| format!("{}: {error}", entry.path))?
        {
            scenarios.push((entry.bucket.clone(), found));
        }
    }
    Ok(scenarios)
}

/// Test-bearing files that are neither sources nor excluded.
fn unmapped_test_files(
    root: &Path,
    sources: &Sources,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mapped: BTreeSet<&str> = sources.sources.iter().map(|s| s.path.as_str()).collect();
    let mut unmapped = Vec::new();
    for top in ["crates", "xtask"] {
        let mut files = Vec::new();
        collect_rust_files(&root.join(top), &mut files)?;
        for file in files {
            let relative = file
                .strip_prefix(root)?
                .components()
                .map(|part| part.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if mapped.contains(relative.as_str())
                || sources.excluded.iter().any(|exclusion| {
                    relative == exclusion.path
                        || (exclusion.path.ends_with('/') && relative.starts_with(&exclusion.path))
                })
            {
                continue;
            }
            let text = fs::read_to_string(&file)?;
            if text.contains("#[test]") || text.contains("::test]") {
                unmapped.push(relative);
            }
        }
    }
    unmapped.sort();
    Ok(unmapped)
}

fn collect_rust_files(
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    if !directory.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if path.is_dir() {
            if name != "target" && name != "generated" {
                collect_rust_files(&path, files)?;
            }
        } else if name.ends_with(".rs") && name != "imperative_generated.rs" {
            files.push(path);
        }
    }
    Ok(())
}

/// Mines one source file.
fn mine_file(relative: &str, text: &str, layer: &str) -> Result<Vec<Scenario>, syn::Error> {
    let file = syn::parse_file(text)?;
    let directory = Path::new(relative)
        .parent()
        .unwrap_or_else(|| Path::new(""));

    let mut constants = BTreeMap::new();
    collect_constants(&file.items, directory, &mut constants);
    let mut helpers = BTreeMap::new();
    collect_helpers(&file.items, &mut helpers);

    let stem = relative
        .strip_prefix("crates/")
        .unwrap_or(relative)
        .trim_end_matches(".rs");
    let source = SourceFile {
        relative,
        layer,
        directory,
        constants,
        helpers,
    };
    let mut scenarios = Vec::new();
    source.collect_tests(&file.items, &mut vec![stem.to_owned()], &mut scenarios);
    Ok(scenarios)
}

/// A file-level constant: either an `include_str!` fixture or canned inline data.
enum Constant {
    Fixture(String),
    Data,
}

fn collect_constants(items: &[Item], directory: &Path, constants: &mut BTreeMap<String, Constant>) {
    for item in items {
        match item {
            Item::Const(item) => {
                let facts = Facts::of(item.expr.to_token_stream(), directory);
                let constant = match facts.includes.first() {
                    Some(path) => Constant::Fixture(path.clone()),
                    None => Constant::Data,
                };
                constants.insert(item.ident.to_string(), constant);
            }
            Item::Static(item) => {
                constants.insert(item.ident.to_string(), Constant::Data);
            }
            Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    collect_constants(items, directory, constants);
                }
            }
            _ => {}
        }
    }
}

/// Non-test functions and methods by name, so a test inherits what its helpers assert.
fn collect_helpers(items: &[Item], helpers: &mut BTreeMap<String, TokenStream>) {
    for item in items {
        match item {
            Item::Fn(function) if !is_test(&function.attrs) => {
                helpers.insert(
                    function.sig.ident.to_string(),
                    function.block.to_token_stream(),
                );
            }
            Item::Impl(implementation) => {
                for member in &implementation.items {
                    if let syn::ImplItem::Fn(method) = member {
                        helpers
                            .insert(method.sig.ident.to_string(), method.block.to_token_stream());
                    }
                }
            }
            Item::Mod(module) => {
                if let Some((_, items)) = &module.content {
                    collect_helpers(items, helpers);
                }
            }
            _ => {}
        }
    }
}

/// What one source file contributes while its tests are collected.
struct SourceFile<'a> {
    relative: &'a str,
    layer: &'a str,
    directory: &'a Path,
    constants: BTreeMap<String, Constant>,
    helpers: BTreeMap<String, TokenStream>,
}

impl SourceFile<'_> {
    fn collect_tests(&self, items: &[Item], path: &mut Vec<String>, scenarios: &mut Vec<Scenario>) {
        for item in items {
            match item {
                Item::Fn(function) if is_test(&function.attrs) => {
                    scenarios.push(self.scenario(function, path));
                }
                Item::Mod(module) => {
                    if let Some((_, items)) = &module.content {
                        path.push(module.ident.to_string());
                        self.collect_tests(items, path, scenarios);
                        path.pop();
                    }
                }
                _ => {}
            }
        }
    }

    fn scenario(&self, function: &syn::ItemFn, path: &[String]) -> Scenario {
        let facts = Facts::with_helpers(
            function.block.to_token_stream(),
            &self.helpers,
            self.directory,
        );
        let name = function.sig.ident.to_string();
        let live = function
            .attrs
            .iter()
            .any(|attribute| attribute.path().is_ident("ignore"));

        let mut fixtures: Vec<String> = facts.includes.clone();
        let mut data = Vec::new();
        for identifier in &facts.identifiers {
            match self.constants.get(identifier) {
                Some(Constant::Fixture(path)) => fixtures.push(path.clone()),
                Some(Constant::Data) => data.push(identifier.clone()),
                None => {}
            }
        }
        fixtures.sort();
        fixtures.dedup();

        let mut requests = Vec::new();
        for literal in &facts.literals {
            let literal = literal.trim_end().to_owned();
            if literal.contains("/api/") && !requests.contains(&literal) {
                requests.push(literal);
            }
        }
        let operations: BTreeSet<String> = requests
            .iter()
            .flat_map(|request| operations_in(request))
            .collect();

        Scenario {
            id: format!("{}::{name}", path.join("::")),
            file: self.relative.to_owned(),
            test: name.clone(),
            layer: self.layer.to_owned(),
            live,
            operations: operations.into_iter().collect(),
            requests,
            fixtures,
            data,
            category: infer_category(&name, live).to_owned(),
            status: "pending".to_owned(),
            covered_by: None,
            reason: None,
        }
    }
}

fn is_test(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute
            .path()
            .segments
            .last()
            .is_some_and(|s| s.ident == "test")
    })
}

/// The operation names after each `/api/` in `request`.
fn operations_in(request: &str) -> Vec<String> {
    request
        .match_indices("/api/")
        .map(|(index, marker)| {
            request[index + marker.len()..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
                .collect::<String>()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// A first-guess conformance category from a test name; a human refines it.
fn infer_category(name: &str, live: bool) -> &'static str {
    if live {
        return "live";
    }
    let words: BTreeSet<&str> = name.split('_').collect();
    let has = |candidates: &[&str]| candidates.iter().any(|word| words.contains(word));
    let rules: [(&[&str], &str); 17] = [
        (
            &[
                "canary", "secret", "secrets", "leak", "leaks", "redact", "redacts", "redacted",
            ],
            "canary",
        ),
        (
            &[
                "recover",
                "recovers",
                "recovery",
                "resume",
                "resumes",
                "journal",
                "interrupted",
            ],
            "recovery",
        ),
        (
            &["unknown", "uncertain", "outcome", "timeout"],
            "outcome-unknown",
        ),
        (
            &[
                "ambiguous",
                "ambiguity",
                "selector",
                "selectors",
                "collision",
                "collisions",
                "duplicate",
                "duplicates",
            ],
            "selector",
        ),
        (&["import", "imports", "imported"], "import"),
        (&["replace", "replaces", "replaced"], "replace"),
        (
            &[
                "delete",
                "deletes",
                "destroy",
                "destroys",
                "remove",
                "removes",
                "removal",
                "protected",
            ],
            "delete",
        ),
        (
            &["move", "moves", "moved", "rename", "renames", "renamed"],
            "move",
        ),
        (&["drift", "drifted", "diverge", "diverges"], "drift"),
        (
            &[
                "order",
                "ordering",
                "ordered",
                "dependency",
                "dependencies",
                "depends",
            ],
            "ordering",
        ),
        (
            &[
                "noop",
                "unchanged",
                "converge",
                "converges",
                "convergent",
                "idempotent",
            ],
            "noop",
        ),
        (
            &["update", "updates", "change", "changes", "changed"],
            "update",
        ),
        (&["create", "creates", "creation", "created"], "create"),
        (
            &[
                "absence",
                "authoritative",
                "unavailable",
                "partial",
                "agreement",
                "fails",
            ],
            "read-model",
        ),
        (
            &[
                "parse",
                "parses",
                "parsed",
                "rejects",
                "reject",
                "invalid",
                "validates",
                "renders",
                "render",
                "writer",
                "schema",
            ],
            "document",
        ),
        (
            &["state", "checkpoint", "lineage", "serial", "lock"],
            "state",
        ),
        (
            &[
                "request",
                "requests",
                "body",
                "serializes",
                "deserializes",
                "wire",
                "sends",
                "payload",
            ],
            "wire",
        ),
    ];
    for (candidates, category) in rules {
        if has(candidates) {
            return category;
        }
    }
    if has(&["plan", "plans", "planned", "planner"]) {
        return "plan";
    }
    "uncategorized"
}

/// What a token stream mentions: string values, identifiers, and `include_str!` paths.
#[derive(Default)]
struct Facts {
    literals: Vec<String>,
    identifiers: BTreeSet<String>,
    includes: Vec<String>,
}

impl Facts {
    fn of(tokens: TokenStream, directory: &Path) -> Self {
        let mut facts = Self::default();
        facts.walk(tokens, directory);
        facts
    }

    /// Facts of `tokens` plus those of every same-file helper they call, transitively.
    fn with_helpers(
        tokens: TokenStream,
        helpers: &BTreeMap<String, TokenStream>,
        directory: &Path,
    ) -> Self {
        let mut facts = Self::of(tokens, directory);
        let mut visited = BTreeSet::new();
        loop {
            let next: Vec<String> = facts
                .identifiers
                .iter()
                .filter(|name| helpers.contains_key(*name) && !visited.contains(*name))
                .cloned()
                .collect();
            if next.is_empty() {
                return facts;
            }
            for name in next {
                let helper = Self::of(helpers[&name].clone(), directory);
                facts.literals.extend(helper.literals);
                facts.identifiers.extend(helper.identifiers);
                facts.includes.extend(helper.includes);
                visited.insert(name);
            }
        }
    }

    fn walk(&mut self, tokens: TokenStream, directory: &Path) {
        let trees: Vec<TokenTree> = tokens.into_iter().collect();
        let mut index = 0;
        while index < trees.len() {
            match &trees[index] {
                TokenTree::Group(group) => self.walk(group.stream(), directory),
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    if name == "include_str"
                        && let (Some(TokenTree::Punct(bang)), Some(TokenTree::Group(group))) =
                            (trees.get(index + 1), trees.get(index + 2))
                        && bang.as_char() == '!'
                    {
                        if let Some(path) = first_string(group.stream()) {
                            self.includes.push(normalize(directory, &path));
                        }
                        index += 3;
                        continue;
                    }
                    self.identifiers.insert(name);
                }
                TokenTree::Literal(literal) => {
                    if let Ok(value) = syn::parse_str::<LitStr>(&literal.to_string()) {
                        self.literals.push(value.value());
                    }
                }
                TokenTree::Punct(_) => {}
            }
            index += 1;
        }
    }
}

fn first_string(tokens: TokenStream) -> Option<String> {
    tokens.into_iter().find_map(|tree| match tree {
        TokenTree::Literal(literal) => syn::parse_str::<LitStr>(&literal.to_string())
            .ok()
            .map(|value| value.value()),
        _ => None,
    })
}

/// Resolves `relative` against `directory` lexically, to a repo-relative path.
fn normalize(directory: &Path, relative: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    for component in directory.join(relative).components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}
