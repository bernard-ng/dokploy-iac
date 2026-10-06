# shellcheck shell=bash
recipe_kind=server
recipe_fixture=server
recipe_id=serverId
recipe_id_label=server-1
recipe_list=server.all
recipe_one=server.one
recipe_create=server.create
recipe_update=server.update
recipe_remove=server.remove
recipe_secrets=""

ssh_key_id=""

# A server names an SSH key, which this makes and removes.
recipe_setup() {
    api_request POST sshKey.generate "$workspace/generated.json" <(printf '{"type":"ed25519"}') >/dev/null
    api_request GET organization.active "$workspace/organization.json" >/dev/null
    jq -n --slurpfile generated "$workspace/generated.json" --slurpfile organization "$workspace/organization.json" '
        {name:"server-contract-key",privateKey:$generated[0].privateKey,publicKey:$generated[0].publicKey,
         organizationId:$organization[0].id}' >"$workspace/ssh-key.request.json"
    api_request POST sshKey.create "$workspace/ssh-key-create.json" "$workspace/ssh-key.request.json" >/dev/null
    api_request GET sshKey.all "$workspace/ssh-key-all.json" >/dev/null
    ssh_key_id="$(jq -er '.[0].sshKeyId' "$workspace/ssh-key-all.json")"
}

recipe_teardown() {
    [[ -n "$ssh_key_id" ]] || return 0
    request_body "$workspace/ssh-key-remove.request.json" --arg id "$ssh_key_id" '{sshKeyId:$id}'
    api_request POST sshKey.remove "$workspace/ssh-key-remove.json" "$workspace/ssh-key-remove.request.json" >/dev/null
}

recipe_create_body() {
    jq -n --arg name "$2" --arg key "$ssh_key_id" '
        {name:$name,description:"capture",ipAddress:"203.0.113.9",port:22,username:"root",sshKeyId:$key,serverType:"deploy"}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{serverId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg name "$3" --arg key "$ssh_key_id" '
        {serverId:$id,name:$name,description:null,ipAddress:"203.0.113.10",port:2222,username:"deploy",
         sshKeyId:$key,serverType:"build",enableDockerCleanup:false,command:"echo capture"}' >"$1"
}
