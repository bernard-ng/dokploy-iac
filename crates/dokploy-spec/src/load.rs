//! Parsing and loading specs from YAML.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy};

use crate::error::LoadError;
use crate::model::KindSpec;
use crate::registry::SpecRegistry;

const MAX_SPEC_BYTES: usize = 256 * 1024;

/// Files in the specs directory that are not kind specs.
const NON_KIND_FILES: [&str; 2] = ["versions.yaml", "types.yaml"];

/// Parses one spec from YAML, rejecting duplicate keys, merge keys, and anchors.
pub fn parse_spec(source: &str) -> Result<KindSpec, String> {
    parse_strict(source)
}

/// Parses a YAML document with the loader's strict options: no duplicate keys,
/// merge keys, anchors, or unsupported tags, and bounded size.
pub(crate) fn parse_strict<T: serde::de::DeserializeOwned>(source: &str) -> Result<T, String> {
    if source.len() > MAX_SPEC_BYTES {
        return Err(format!("document is larger than {MAX_SPEC_BYTES} bytes"));
    }
    let options = serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::Error,
        merge_keys: MergeKeyPolicy::Error,
        strict_booleans: true,
        reject_unsupported_tags: true,
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_SPEC_BYTES),
            max_events: 200_000,
            max_aliases: 0,
            max_anchors: 0,
            max_recorded_anchor_events: 0,
            max_recorded_anchor_bytes: 0,
            max_depth: 24,
            max_inclusion_depth: 0,
            max_documents: 1,
            max_nodes: 100_000,
            max_total_scalar_bytes: 200 * 1024,
            max_total_comment_bytes: 200 * 1024,
            max_merge_keys: 0,
        },
    };
    serde_saphyr::from_str_with_options(source, options).map_err(|error| {
        error.location().map_or_else(
            || error.to_string(),
            |location| {
                format!(
                    "line {}, column {}: {error}",
                    location.line(),
                    location.column()
                )
            },
        )
    })
}

fn collect_files(directory: &Path, files: &mut Vec<PathBuf>) -> Result<(), LoadError> {
    let entries = fs::read_dir(directory).map_err(|source| LoadError::Io {
        path: directory.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry.map_err(|source| LoadError::Io {
            path: directory.to_path_buf(),
            source,
        })?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "yaml")
            && !path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| NON_KIND_FILES.contains(&name))
        {
            files.push(path);
        }
    }
    Ok(())
}

/// Loads, validates, and cross-checks every `*.yaml` kind spec below `root`.
pub fn load_dir(root: &Path) -> Result<SpecRegistry, LoadError> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();

    let mut specs = Vec::new();
    for path in files {
        let source = fs::read_to_string(&path).map_err(|source| LoadError::Io {
            path: path.clone(),
            source,
        })?;
        let spec = parse_spec(&source).map_err(|message| LoadError::Parse {
            path: path.clone(),
            message,
        })?;
        specs.push(spec);
    }
    SpecRegistry::from_specs(specs, &BTreeSet::new()).map_err(LoadError::Invalid)
}
