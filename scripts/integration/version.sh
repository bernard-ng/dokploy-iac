#!/usr/bin/env bash
#
# Selects the Dokploy release the integration scripts run against. Sourced by
# common.sh and check-fixtures.sh.
#
# DOKPLOY_VERSION (`v0.30.6` or `0.30.6`) and DOKPLOY_IMAGE override the default.
# Any other version needs its digest-pinned image, which
# `cargo xtask versions --image <version>` prints from specs/versions.yaml.
# `cargo xtask versions --check` keeps the defaults below equal to that file.

dokploy_default_version="v0.30.6"
dokploy_default_image="dokploy/dokploy:v0.30.6@sha256:1d6bd69ba58c1b4e305a9a33d77d8c3e0ee34707169f680cc600ab7ef1c3e6d8"

dokploy_version="${DOKPLOY_VERSION:-$dokploy_default_version}"
dokploy_version="v${dokploy_version#v}"

if [[ -n "${DOKPLOY_IMAGE:-}" ]]; then
    dokploy_image="$DOKPLOY_IMAGE"
elif [[ "$dokploy_version" == "$dokploy_default_version" ]]; then
    dokploy_image="$dokploy_default_image"
else
    echo "Set DOKPLOY_IMAGE for Dokploy $dokploy_version (cargo xtask versions --image ${dokploy_version#v})." >&2
    exit 1
fi

if [[ "$dokploy_image" != "dokploy/dokploy:$dokploy_version@sha256:"* ]]; then
    echo "DOKPLOY_IMAGE must be dokploy/dokploy:$dokploy_version@sha256:<digest>, got $dokploy_image." >&2
    exit 1
fi

# Compose reads the image from the environment.
export DOKPLOY_IMAGE="$dokploy_image"
dokploy_fixture_directory="$repository_root/fixtures/api/live/$dokploy_version"

# Recorded in capture metadata; set DOKPLOY_CAPTURED_AT to reproduce a capture's date.
captured_at="${DOKPLOY_CAPTURED_AT:-$(date -u +%F)}"
