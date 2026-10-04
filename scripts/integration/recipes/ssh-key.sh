# shellcheck shell=bash
recipe_kind=ssh_key
recipe_fixture=sshKey
recipe_id=sshKeyId
recipe_id_label=ssh-key-1
recipe_list=sshKey.all
recipe_one=sshKey.one
recipe_create=sshKey.create
recipe_update=sshKey.update
recipe_remove=sshKey.remove
recipe_secrets=".privateKey"

recipe_setup() {
    api_request POST sshKey.generate "$workspace/generated.json" <(printf '{"type":"ed25519"}') >/dev/null
    api_request GET organization.active "$workspace/organization.json" >/dev/null
}

recipe_create_body() {
    jq -n --slurpfile generated "$workspace/generated.json" --slurpfile organization "$workspace/organization.json" \
        --arg name "$2" '
        {name:$name,description:"capture",privateKey:$generated[0].privateKey,
         publicKey:$generated[0].publicKey,organizationId:$organization[0].id}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{sshKeyId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg name "$3" '{sshKeyId:$id,name:$name,description:"updated"}' >"$1"
}
