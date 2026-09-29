use super::*;

#[cfg(unix)]
#[test]
fn secure_reader_rejects_traversal_symlinks_and_non_regular_files() {
    use std::os::unix::fs::symlink;

    let parent = tempfile::tempdir().unwrap();
    let workspace = parent.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    std::fs::write(parent.path().join("outside"), b"outside-secret").unwrap();
    std::fs::create_dir(workspace.join("directory")).unwrap();
    symlink(parent.path().join("outside"), workspace.join("final-link")).unwrap();
    symlink(parent.path(), workspace.join("directory-link")).unwrap();

    for path in [
        "../outside",
        "/absolute",
        "directory",
        "final-link",
        "directory-link/outside",
        "directory/../outside",
    ] {
        assert!(matches!(
            read_sensitive_file(&workspace, path),
            Err(SensitiveFileReadError::Unavailable)
        ));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn secure_reader_rejects_fifos_without_blocking() {
    use rustix::fs::{Mode, OFlags, mkfifoat, open};

    let workspace = tempfile::tempdir().unwrap();
    let directory = open(
        workspace.path(),
        OFlags::RDONLY | OFlags::DIRECTORY,
        Mode::empty(),
    )
    .unwrap();
    mkfifoat(&directory, "pipe", Mode::RUSR | Mode::WUSR).unwrap();

    assert!(matches!(
        read_sensitive_file(workspace.path(), "pipe"),
        Err(SensitiveFileReadError::Unavailable)
    ));
}

#[cfg(unix)]
#[test]
fn secure_reader_enforces_inclusive_one_mib_bound_and_exact_utf8_bytes() {
    let workspace = tempfile::tempdir().unwrap();
    let at_limit = vec![b'x'; MAX_SENSITIVE_FILE_BYTES];
    std::fs::write(workspace.path().join("at-limit"), &at_limit).unwrap();
    std::fs::write(
        workspace.path().join("over-limit"),
        vec![b'x'; MAX_SENSITIVE_FILE_BYTES + 1],
    )
    .unwrap();
    std::fs::write(workspace.path().join("exact"), b" value\r\n").unwrap();

    assert_eq!(
        read_sensitive_file(workspace.path(), "at-limit")
            .unwrap()
            .as_slice(),
        at_limit
    );
    assert!(matches!(
        read_sensitive_file(workspace.path(), "over-limit"),
        Err(SensitiveFileReadError::TooLarge)
    ));
    assert_eq!(
        read_sensitive_file(workspace.path(), "exact")
            .unwrap()
            .as_slice(),
        b" value\r\n"
    );
}
