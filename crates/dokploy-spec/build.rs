//! Embeds the kind specs in `specs/` so the binary carries them (ADR 0002): the specs are the
//! one source of truth, and the tool must not depend on finding them on disk.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const NON_KIND_FILES: [&str; 2] = ["versions.yaml", "types.yaml"];

fn collect(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, files);
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
}

fn main() {
    let root =
        Path::new(&std::env::var("CARGO_MANIFEST_DIR").expect("cargo sets the manifest dir"))
            .join("../../specs");
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = Vec::new();
    collect(&root, &mut files);
    files.sort();

    let mut source = String::from("pub(crate) const FILES: &[(&str, &str)] = &[\n");
    for path in &files {
        let relative = path.strip_prefix(&root).expect("below the specs directory");
        let _ = writeln!(
            source,
            "    ({:?}, include_str!({:?})),",
            relative.to_string_lossy(),
            path.to_string_lossy()
        );
    }
    source.push_str("];\n");
    let out =
        Path::new(&std::env::var("OUT_DIR").expect("cargo sets the out dir")).join("embedded.rs");
    std::fs::write(out, source).expect("the generated file is written");
}
