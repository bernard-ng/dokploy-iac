#!/usr/bin/env bash
#
# The runner for the settings kinds that follow one pattern (ssh_key, ai_provider, certificate, ...),
# so a kind is a recipe and not a script of its own. Sourced by the thin capture-<kind>-contract.sh
# wrappers after common.sh and capture-lib.sh; `capture_recipe <recipe>` runs one.
#
# A recipe (recipes/<name>.sh) defines these variables:
#   recipe_kind        the kind, for messages (ssh_key)
#   recipe_fixture     the resource part of the operation ids and of the fixture names (sshKey)
#   recipe_id          the id key of the object (sshKeyId)
#   recipe_id_label    what the sanitiser makes of an id (ssh-key-1)
#   recipe_name        the key the object is named by (default: name)
#   recipe_one_name    dot-separated path to the name in a direct read (default: recipe_name)
#   recipe_list / recipe_one / recipe_create / recipe_update / recipe_remove   operation ids
#   recipe_one_param   the query parameter of the direct read (default: the id key)
#   recipe_one_id      dot-separated path to the direct-read id in a list item (default: id key)
#   recipe_remove_action   the fixture suffix of the remove (default: the operation's action)
#   recipe_secrets     space-separated jq paths of the object that carry a secret (.privateKey)
# and these functions:
#   recipe_setup                       optional; prepares anything the bodies need
#   recipe_teardown                    optional; removes what setup made
#   recipe_create_body FILE NAME       writes the create body
#   recipe_partial_body FILE ID NAME   writes an update body that carries only the name
#   recipe_full_body FILE ID NAME      writes an update body with every field
# What the runner proves is recorded in `<fixture>-recipe.metadata.json`: the shape of each answer,
# whether an update is a patch, whether names are unique, and whether the secrets are listed.

capture_recipe() {
    local recipe="$1"
    # shellcheck source=/dev/null
    source "$script_directory/recipes/$recipe.sh"
    recipe_name="${recipe_name:-name}"
    recipe_one_name="${recipe_one_name:-$recipe_name}"
    recipe_one_param="${recipe_one_param:-$recipe_id}"
    recipe_one_id="${recipe_one_id:-$recipe_id}"
    recipe_remove_action="${recipe_remove_action:-${recipe_remove#*.}}"

    capture_begin "$recipe_fixture"
    created_ids=()
    published=()

    capture_cleanup() {
        local id
        for id in "${created_ids[@]:-}"; do
            [[ -n "$id" ]] || continue
            request_body "$workspace/cleanup.request.json" --arg id "$id" "{($(printf '"%s"' "$recipe_id")): \$id}"
            api_request POST "$recipe_remove" "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
        done
        if declare -F recipe_teardown >/dev/null; then recipe_teardown || true; fi
    }

    require_runtime_version
    status="$(api_request GET "$recipe_list" "$workspace/preflight.json")"
    require_status "$status" 200 "$recipe_list before create"
    if [[ "$(jq 'length' "$workspace/preflight.json")" != 0 ]]; then
        echo "The instance already has ${recipe_kind}s; run scripts/integration/reset.sh and up.sh first." >&2
        exit 1
    fi
    if declare -F recipe_setup >/dev/null; then recipe_setup; fi

    local name="$recipe_fixture-contract" renamed="$recipe_fixture-contract-renamed"
    local one_url_prefix="$recipe_one?$recipe_one_param="

    # create
    recipe_create_body "$workspace/create.request.json" "$name"
    create_status="$(api_request POST "$recipe_create" "$workspace/create.json" "$workspace/create.request.json")"
    require_status "$create_status" 200 "$recipe_create"
    create_returns=empty
    if [[ -s "$workspace/create.json" ]]; then
        if jq -e --arg key "$recipe_id" 'type == "object" and has($key)' "$workspace/create.json" >/dev/null 2>&1; then
            create_returns=object
        else
            create_returns="$(jq -r 'if type == "object" then "object_without_id" else type end' "$workspace/create.json" 2>/dev/null || echo other)"
        fi
    fi
    api_request GET "$recipe_list" "$workspace/all-created.json" >/dev/null
    jq -e 'length == 1' "$workspace/all-created.json" >/dev/null
    object_id="$(jq -er --arg key "$recipe_id" '.[0][$key]' "$workspace/all-created.json")"
    one_object_id="$(jq -er --arg path "$recipe_one_id" '.[0] | getpath($path | split("."))' "$workspace/all-created.json")"
    created_ids+=("$object_id")
    if [[ "$create_returns" == object ]]; then
        jq -e --arg key "$recipe_id" --arg id "$object_id" '.[$key] == $id' "$workspace/create.json" >/dev/null
        cp "$workspace/create.json" "$workspace/$recipe_fixture-create.json"
        published+=("$recipe_fixture-create")
    fi
    one_status="$(api_request GET "$one_url_prefix$(urlencode "$one_object_id")" "$workspace/one-created.json")"
    require_status "$one_status" 200 "$recipe_one after create"
    cp "$workspace/all-created.json" "$workspace/$recipe_fixture-all.created.json"
    cp "$workspace/one-created.json" "$workspace/$recipe_fixture-one.created.json"
    published+=("$recipe_fixture-all.created" "$recipe_fixture-one.created")

    # whether the secrets are listed in clear or masked
    lists_secrets=false
    masked_secrets=false
    local key value
    for key in $recipe_secrets; do
        value="$(jq -r ".[0]$key // empty" "$workspace/all-created.json")"
        if [[ -n "$value" ]]; then
            if [[ "$value" =~ ^\*+$ ]]; then masked_secrets=true; else lists_secrets=true; fi
        fi
    done
    detail_secrets=false
    for key in $recipe_secrets; do
        value="$(jq -r "$key // empty" "$workspace/one-created.json")"
        if [[ -n "$value" && ! "$value" =~ ^\*+$ ]]; then detail_secrets=true; fi
    done

    # a partial update: only the id and the name
    recipe_partial_body "$workspace/partial.request.json" "$object_id" "$renamed"
    partial_status="$(api_request POST "$recipe_update" "$workspace/partial.json" "$workspace/partial.request.json")"
    update_is_patch=false
    if [[ "$partial_status" == 200 ]]; then
        api_request GET "$one_url_prefix$(urlencode "$one_object_id")" "$workspace/one-partial.json" >/dev/null
        if jq -e --arg path "$recipe_one_name" --arg name "$renamed" 'getpath($path | split(".")) == $name' "$workspace/one-partial.json" >/dev/null; then
            update_is_patch=true
        fi
        if [[ -s "$workspace/partial.json" ]] && jq -e . "$workspace/partial.json" >/dev/null 2>&1; then
            cp "$workspace/partial.json" "$workspace/$recipe_fixture-update.partial.json"
            published+=("$recipe_fixture-update.partial")
        fi
        cp "$workspace/one-partial.json" "$workspace/$recipe_fixture-one.partial-updated.json"
        published+=("$recipe_fixture-one.partial-updated")
    else
        cp "$workspace/partial.json" "$workspace/$recipe_fixture-update.partial-rejected.json"
        published+=("$recipe_fixture-update.partial-rejected")
    fi

    # a full update
    recipe_full_body "$workspace/full.request.json" "$object_id" "$renamed"
    update_status="$(api_request POST "$recipe_update" "$workspace/update.json" "$workspace/full.request.json")"
    require_status "$update_status" 200 "$recipe_update (full)"
    update_returns=empty
    if [[ -s "$workspace/update.json" ]]; then
        update_returns="$(jq -r 'if type == "object" then "object" else (type | tostring) end' "$workspace/update.json" 2>/dev/null || echo other)"
        if jq -e . "$workspace/update.json" >/dev/null 2>&1; then
            cp "$workspace/update.json" "$workspace/$recipe_fixture-update.json"
            published+=("$recipe_fixture-update")
        fi
    fi
    api_request GET "$one_url_prefix$(urlencode "$one_object_id")" "$workspace/one-updated.json" >/dev/null
    jq -e --arg path "$recipe_one_name" --arg name "$renamed" 'getpath($path | split(".")) == $name' "$workspace/one-updated.json" >/dev/null
    cp "$workspace/one-updated.json" "$workspace/$recipe_fixture-one.updated.json"
    published+=("$recipe_fixture-one.updated")

    # names are not necessarily unique
    recipe_create_body "$workspace/duplicate.request.json" "$renamed"
    duplicate_status="$(api_request POST "$recipe_create" "$workspace/duplicate.json" "$workspace/duplicate.request.json")"
    if [[ "$duplicate_status" == 200 ]]; then
        api_request GET "$recipe_list" "$workspace/all-duplicate.json" >/dev/null
        while IFS= read -r id; do
            [[ "$id" == "$object_id" ]] || created_ids+=("$id")
        done < <(jq -r --arg key "$recipe_id" '.[][$key]' "$workspace/all-duplicate.json")
    fi

    # remove
    request_body "$workspace/remove.request.json" --arg id "$object_id" "{($(printf '"%s"' "$recipe_id")): \$id}"
    remove_status="$(api_request POST "$recipe_remove" "$workspace/remove.json" "$workspace/remove.request.json")"
    require_status "$remove_status" 200 "$recipe_remove"
    remove_returns=empty
    if [[ -s "$workspace/remove.json" ]]; then
        remove_returns="$(jq -r 'if type == "object" then "object" else (type | tostring) end' "$workspace/remove.json" 2>/dev/null || echo other)"
        if jq -e . "$workspace/remove.json" >/dev/null 2>&1; then
            cp "$workspace/remove.json" "$workspace/$recipe_fixture-$recipe_remove_action.json"
            published+=("$recipe_fixture-$recipe_remove_action")
        fi
    fi
    removed_one_status="$(api_request GET "$one_url_prefix$(urlencode "$one_object_id")" "$workspace/one-removed.json")"
    cp "$workspace/one-removed.json" "$workspace/$recipe_fixture-one.removed.json"
    published+=("$recipe_fixture-one.removed")
    # what is left (a duplicate) goes too
    for id in "${created_ids[@]}"; do
        [[ "$id" == "$object_id" ]] && continue
        request_body "$workspace/cleanup.request.json" --arg id "$id" "{($(printf '"%s"' "$recipe_id")): \$id}"
        api_request POST "$recipe_remove" "$workspace/cleanup.json" "$workspace/cleanup.request.json" >/dev/null
    done
    created_ids=()
    api_request GET "$recipe_list" "$workspace/all-removed.json" >/dev/null
    jq -e 'length == 0' "$workspace/all-removed.json" >/dev/null
    cp "$workspace/all-removed.json" "$workspace/$recipe_fixture-all.removed.json"
    published+=("$recipe_fixture-all.removed")

    local files
    files="$(printf '%s\n' "${published[@]}" | jq -R . | jq -s 'map(. + ".owner.json")')"
    jq -n --sort-keys --indent 2 \
        --arg capturedAt "$captured_at" --arg version "$runtime_version" --arg image "$dokploy_image" \
        --arg kind "$recipe_kind" --arg createReturns "$create_returns" --arg updateReturns "$update_returns" \
        --arg removeReturns "$remove_returns" --argjson createStatus "$create_status" \
        --argjson partialStatus "$partial_status" --argjson updateIsPatch "$update_is_patch" \
        --argjson duplicateStatus "$duplicate_status" --argjson removeStatus "$remove_status" \
        --argjson removedOneStatus "$removed_one_status" --argjson listsCredentials "$lists_secrets" \
        --argjson masksCredentials "$masked_secrets" --argjson detailExposesCredentials "$detail_secrets" \
        --argjson fixtures "$files" '
        {
            capturedAt:$capturedAt, role:"owner", version:$version, image:$image, sanitized:true, kind:$kind,
            createReturns:$createReturns, updateReturns:$updateReturns, removeReturns:$removeReturns,
            updateIsPatch:$updateIsPatch, partialUpdateStatus:$partialStatus,
            duplicateNamesAllowed:($duplicateStatus == 200), duplicateStatus:$duplicateStatus,
            listingExposesCredentials:$listsCredentials, listingMasksCredentials:$masksCredentials,
            detailExposesCredentials:$detailExposesCredentials,
            createStatus:$createStatus, removeStatus:$removeStatus, removedOneStatus:$removedOneStatus,
            fixtures:$fixtures
        }' >"$publish_directory/$recipe_fixture-recipe.metadata.json"

    CAPTURE_ID_KEYS="$recipe_id:$recipe_id_label" capture_publish "${published[@]}"
    capture_succeeded=true
    echo "Captured the sanitised $recipe_kind contract."
}
