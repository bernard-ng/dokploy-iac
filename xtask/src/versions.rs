//! `cargo xtask versions`: the supported Dokploy versions (`specs/versions.yaml`).
//!
//! `--check` ties that file to the places that repeat its default pin, so the
//! integration scripts, the compose file, and the spec cannot drift apart.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use dokploy_spec::{VersionStatus, Versions};

/// The outcome of a versions check.
#[derive(Debug)]
pub struct VersionsReport {
    /// A printable table of the known versions.
    pub table: String,
    /// Failures; empty when the check passed.
    pub failures: Vec<String>,
}

/// Loads `specs/versions.yaml` below `root`.
pub fn load_versions(root: &Path) -> Result<Versions, Box<dyn std::error::Error>> {
    Ok(Versions::load(&root.join("specs"))?)
}

/// The digest-pinned image of `requested` (`0.30.7` or `v0.30.7`).
pub fn version_image(root: &Path, requested: &str) -> Result<String, Box<dyn std::error::Error>> {
    let versions = load_versions(root)?;
    versions
        .find(requested)
        .map(|version| version.image.clone())
        .ok_or_else(|| format!("`{requested}` is not listed in specs/versions.yaml").into())
}

/// Checks `specs/versions.yaml` against the repository.
pub fn run_versions_check(root: &Path) -> Result<VersionsReport, Box<dyn std::error::Error>> {
    let versions = match load_versions(root) {
        Ok(versions) => versions,
        Err(error) => {
            return Ok(VersionsReport {
                table: String::new(),
                failures: vec![error.to_string()],
            });
        }
    };

    let mut table = String::new();
    writeln!(table, "{:<10} {:<10} fixtures", "version", "status")?;
    let mut failures = Vec::new();
    for version in versions.all() {
        let fixtures = root.join(version.fixture_directory());
        let has_fixtures = fixtures.is_dir();
        writeln!(
            table,
            "{:<10} {:<10} {}",
            version.version,
            version.status,
            if has_fixtures { "captured" } else { "none" }
        )?;
        if let Some(openapi) = &version.openapi
            && !root.join(openapi).is_file()
        {
            failures.push(format!(
                "{}: openapi `{openapi}` does not exist",
                version.version
            ));
        }
        if version.status == VersionStatus::Supported && !has_fixtures {
            failures.push(format!(
                "{}: supported but {} has no captured fixtures",
                version.version,
                version.fixture_directory()
            ));
        }
    }

    if let Some(default) = versions.default_version() {
        for relative in ["compose.integration.yaml", "scripts/integration/version.sh"] {
            let text = fs::read_to_string(root.join(relative))?;
            if !text.contains(&default.image) {
                failures.push(format!(
                    "{relative}: does not repeat the default image `{}`",
                    default.image
                ));
            }
        }
        let script = fs::read_to_string(root.join("scripts/integration/version.sh"))?;
        if !script.contains(&format!("dokploy_default_version=\"{}\"", default.tag())) {
            failures.push(format!(
                "scripts/integration/version.sh: dokploy_default_version is not {}",
                default.tag()
            ));
        }
    }

    Ok(VersionsReport { table, failures })
}
