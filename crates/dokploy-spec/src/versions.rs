//! The Dokploy versions the tool knows (`specs/versions.yaml`, ADR 0014).

use std::fmt;
use std::fs;
use std::path::Path;

use serde::Deserialize;

use crate::error::LoadError;
use crate::load::parse_strict;

/// How far a Dokploy version is trusted.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VersionStatus {
    /// Fixtures are captured and the live suite passes; mutations are allowed.
    Supported,
    /// Captures may be taken against it; it is not yet trusted for mutations.
    Candidate,
}

impl fmt::Display for VersionStatus {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Supported => "supported",
            Self::Candidate => "candidate",
        })
    }
}

/// One Dokploy release and where its evidence lives.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DokployVersion {
    /// `major.minor.patch`, without a leading `v`.
    pub version: String,
    /// How far the version is trusted.
    pub status: VersionStatus,
    /// The digest-pinned image, `dokploy/dokploy:v<version>@sha256:<64 hex>`.
    pub image: String,
    /// The vendored OpenAPI document, relative to the repository, when there is one.
    #[serde(default)]
    pub openapi: Option<String>,
}

impl DokployVersion {
    /// The git-tag spelling, `v<version>`.
    pub fn tag(&self) -> String {
        format!("v{}", self.version)
    }

    /// The repository-relative directory of this version's live fixtures.
    pub fn fixture_directory(&self) -> String {
        format!("fixtures/api/live/{}", self.tag())
    }
}

/// The parsed `specs/versions.yaml`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Versions {
    versions: Vec<DokployVersion>,
}

impl Versions {
    /// Every version, in file order.
    pub fn all(&self) -> &[DokployVersion] {
        &self.versions
    }

    /// The version a script uses when none is requested: the first supported one.
    pub fn default_version(&self) -> Option<&DokployVersion> {
        self.versions
            .iter()
            .find(|version| version.status == VersionStatus::Supported)
    }

    /// Finds a version by `0.30.6` or `v0.30.6`.
    pub fn find(&self, requested: &str) -> Option<&DokployVersion> {
        let requested = requested.strip_prefix('v').unwrap_or(requested);
        self.versions
            .iter()
            .find(|version| version.version == requested)
    }

    /// Parses and validates the contents of `versions.yaml`.
    pub fn parse(source: &str) -> Result<Self, String> {
        let versions: Self = parse_strict(source)?;
        let mut problems = Vec::new();
        for (index, version) in versions.versions.iter().enumerate() {
            problems.extend(version.problems());
            if versions.versions[..index]
                .iter()
                .any(|earlier| earlier.version == version.version)
            {
                problems.push(format!("{}: listed twice", version.version));
            }
        }
        if versions.default_version().is_none() {
            problems.push("no version is supported".to_owned());
        }
        if problems.is_empty() {
            Ok(versions)
        } else {
            Err(problems.join("; "))
        }
    }

    /// Loads `versions.yaml` from a specs directory.
    pub fn load(specs: &Path) -> Result<Self, LoadError> {
        let path = specs.join("versions.yaml");
        let source = fs::read_to_string(&path).map_err(|source| LoadError::Io {
            path: path.clone(),
            source,
        })?;
        Self::parse(&source).map_err(|message| LoadError::Parse { path, message })
    }
}

impl DokployVersion {
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let parts: Vec<&str> = self.version.split('.').collect();
        if parts.len() != 3
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
        {
            problems.push(format!(
                "{}: version must be major.minor.patch without a leading `v`",
                self.version
            ));
        }
        let expected = format!("dokploy/dokploy:{}@sha256:", self.tag());
        match self.image.strip_prefix(&expected) {
            Some(digest)
                if digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) => {}
            _ => problems.push(format!(
                "{}: image must be `{expected}<64 lowercase hex digits>`",
                self.version
            )),
        }
        if self.status == VersionStatus::Supported && self.openapi.is_none() {
            problems.push(format!(
                "{}: a supported version needs a vendored `openapi` document",
                self.version
            ));
        }
        problems
    }
}
