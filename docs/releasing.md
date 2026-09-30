# Releasing the CLI

The repository uses cargo-dist 0.33.0 to build and publish `dokploy-cli`. Its
version and release policy live in `dist-workspace.toml`; the generated GitHub
Actions workflow lives in `.github/workflows/release.yml`.

## Published artifacts

Each release contains archives for:

- Linux x86_64 and ARM64;
- macOS Intel and Apple Silicon;
- Windows x86_64.

Unix archives use `.tar.xz` and Windows uses `.zip`. cargo-dist publishes a
SHA-256 file beside every archive, a combined checksum file, and shell and
PowerShell installers. Release pull requests run the cargo-dist planning job
without compiling or uploading release binaries. A matching version tag runs
the complete build and publishes a GitHub Release.

All third-party GitHub Actions in the generated workflow are pinned to full
commit hashes. The cargo-dist installer URL is pinned to version 0.33.0.

## Prepare and publish

1. Update the crate version and changelog in the release commit.
2. Regenerate and verify the release workflow with the pinned cargo-dist
   version.
3. Merge the release commit, then push a version tag such as `v0.2.0` whose
   version matches `dokploy-cli`.
4. Confirm that every archive, checksum, and installer is attached to the
   resulting GitHub Release before announcing it.

When changing release configuration, install cargo-dist 0.33.0 from its
official release and verify its published checksum before running:

```bash
dist generate --mode=ci
dist generate --mode=ci --check
dist plan
```

`dist generate --check` is the repository drift check: it fails when the
committed workflow does not match `dist-workspace.toml`. Do not edit the
generated workflow directly. Change the distribution configuration and
regenerate it instead.

The release workflow downloads cargo-dist through the upstream versioned
installer. Review a cargo-dist version update, its installer, and regenerated
workflow as one supply-chain change.
