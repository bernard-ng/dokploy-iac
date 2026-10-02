#!/usr/bin/env bash
#
# Records the live response shapes of the Dokploy tag endpoints.
#
# The SDK tag adapter pins its response shapes with tests, because the generated
# contract types them as untyped JSON and no live fixture exists yet. This script
# exercises every tag endpoint against the local integration instance and writes
# value-free shape evidence (types only) under the integration state directory.
# It never publishes fixtures; review the shapes, then add sanitized fixtures
# deliberately. It deletes everything it creates.

set -euo pipefail

if [[ "$-" == *x* ]]; then set +x; fi

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

api_key_file="$state_directory/api-key"
if [[ ! -s "$api_key_file" ]]; then
    echo "Run scripts/integration/up.sh before capturing the tag contract." >&2
    exit 1
fi

umask 077
run_id="$(date -u +%Y%m%dT%H%M%SZ)-$(openssl rand -hex 6)"
workspace="$state_directory/tag-contract-$run_id"
mkdir -p "$workspace"
chmod 700 "$workspace"

auth_header_file="$workspace/api-header"
{
    printf 'x-api-key: '
    tr -d '\r\n' <"$api_key_file"
    printf '\n'
} >"$auth_header_file"
chmod 600 "$auth_header_file"

tag_name="iac-tag-contract-$run_id"
project_name="iac-tag-contract-project-$run_id"
tag_id=""
project_id=""

api() {
    local method="$1" endpoint="$2" destination="$3" body="${4:-}"
    local arguments=(
        --silent --show-error --output "$destination" --write-out '%{http_code}'
        --header "@$auth_header_file" --request "$method"
    )
    if [[ -n "$body" ]]; then
        arguments+=(--header 'Content-Type: application/json' --data-binary "$body")
    fi
    curl "${arguments[@]}" "$base_url/api/$endpoint"
}

# Replaces every scalar with its JSON type so no identifier or value is recorded.
shape() { jq --sort-keys --indent 2 'walk(if type == "object" or type == "array" then . else type end)' "$1"; }

record() {
    local name="$1" status="$2" file="$3"
    {
        printf '// status %s\n' "$status"
        shape "$file" 2>/dev/null || printf '// non-JSON body\n'
    } >"$workspace/$name.shape.json"
}

cleanup() {
    local status=$?
    set +e
    if [[ -n "$project_id" ]]; then
        api POST project.remove "$workspace/cleanup-project.json" \
            "$(jq -nc --arg id "$project_id" '{projectId: $id}')" >/dev/null
    fi
    if [[ -n "$tag_id" ]]; then
        api POST tag.remove "$workspace/cleanup-tag.json" \
            "$(jq -nc --arg id "$tag_id" '{tagId: $id}')" >/dev/null
    fi
    if ((status != 0)); then
        echo "Tag contract capture failed; partial evidence is in $workspace" >&2
    fi
    exit "$status"
}
trap cleanup EXIT

status="$(api POST tag.create "$workspace/create.json" \
    "$(jq -nc --arg name "$tag_name" '{name: $name, color: "#e11d48"}')")"
record tag-create "$status" "$workspace/create.json"
tag_id="$(jq -r '.tagId // empty' "$workspace/create.json")"
if [[ -z "$tag_id" ]]; then
    echo "tag.create did not return a tagId; the SDK would report an unknown outcome." >&2
    exit 1
fi

status="$(api GET tag.all "$workspace/all.json")"
record tag-all "$status" "$workspace/all.json"
status="$(api GET "tag.one?tagId=$(jq -nr --arg v "$tag_id" '$v|@uri')" "$workspace/one.json")"
record tag-one "$status" "$workspace/one.json"
status="$(api POST tag.update "$workspace/update.json" \
    "$(jq -nc --arg id "$tag_id" '{tagId: $id, color: "#16a34a"}')")"
record tag-update "$status" "$workspace/update.json"

status="$(api POST project.create "$workspace/project.json" \
    "$(jq -nc --arg name "$project_name" '{name: $name}')")"
record project-create "$status" "$workspace/project.json"
project_id="$(jq -r '.project.projectId // .projectId // empty' "$workspace/project.json")"
if [[ -z "$project_id" ]]; then
    echo "project.create did not return a projectId." >&2
    exit 1
fi

assign_body="$(jq -nc --arg p "$project_id" --arg t "$tag_id" '{projectId: $p, tagId: $t}')"
status="$(api POST tag.assignToProject "$workspace/assign.json" "$assign_body")"
record tag-assign "$status" "$workspace/assign.json"
status="$(api GET "project.one?projectId=$(jq -nr --arg v "$project_id" '$v|@uri')" "$workspace/project-one.json")"
record project-one-tagged "$status" "$workspace/project-one.json"
status="$(api POST tag.removeFromProject "$workspace/unassign.json" "$assign_body")"
record tag-unassign "$status" "$workspace/unassign.json"

echo "Tag response shapes recorded under $workspace (types only, no values)."
