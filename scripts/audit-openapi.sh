#!/usr/bin/env bash

set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
specification="$repository_root/openapi/dokploy.json"
expected_checksum="533101f9d106bce0b738ee12eb1111d26b2836e7cddacd80cac95e4863ea82e5"

actual_checksum="$(shasum -a 256 "$specification" | awk '{print $1}')"

if [[ "$actual_checksum" != "$expected_checksum" ]]; then
    echo "OpenAPI checksum mismatch." >&2
    echo "Expected: $expected_checksum" >&2
    echo "Actual:   $actual_checksum" >&2
    exit 1
fi

jq -e '.openapi == "3.1.0"' "$specification" >/dev/null

echo "OpenAPI version: $(jq -r '.openapi' "$specification")"
echo "Paths: $(jq -r '.paths | length' "$specification")"
echo "Configured API-key header: $(jq -r '.components.securitySchemes.apiKey.name' "$specification")"

echo
echo "Security scheme names referenced by operations:"
jq -r '[.paths[] | to_entries[] | .value.security[]? | keys[]] | unique[]' "$specification"

echo
echo "Selected operation contracts:"

for endpoint in \
    '/application.create' \
    '/application.one' \
    '/project.all' \
    '/project.one' \
    '/postgres.one'
do
    jq --arg endpoint "$endpoint" -r '
        .paths[$endpoint]
        | to_entries[]
        | [
            .value.operationId,
            (.key | ascii_upcase),
            $endpoint,
            ((.value.parameters // []) | length | tostring),
            (if .value.requestBody then "body" else "no-body" end),
            ((.value.responses["200"].content["application/json"].schema // {}) | tostring)
        ]
        | @tsv
    ' "$specification"
done

echo
echo "Operations whose HTTP 200 schema is a closed empty object:"
jq -r '
    [
        .paths
        | to_entries[] as $path_entry
        | $path_entry.value
        | to_entries[]
        | select(.key | IN("get", "post", "put", "patch", "delete"))
        | select(
            (.value.responses["200"].content["application/json"].schema // null)
            == {"type":"object", "properties":{}, "additionalProperties":false}
        )
    ]
    | length
' "$specification"
