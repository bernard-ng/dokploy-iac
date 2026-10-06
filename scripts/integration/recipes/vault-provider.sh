# shellcheck shell=bash
recipe_kind=vault_provider
recipe_fixture=vaultProvider
recipe_id=vaultProviderId
recipe_id_label=vault-provider-1
recipe_list=vaultProvider.all
recipe_one=vaultProvider.one
recipe_create=vaultProvider.create
recipe_update=vaultProvider.update
recipe_remove=vaultProvider.remove
recipe_secrets=".config.serviceToken"

recipe_setup() {
    capture_project_setup vault-provider
}

recipe_teardown() {
    capture_remove_project
}

recipe_create_body() {
    jq -n --arg name "$2" --arg project "$capture_project_id" --arg environment "$capture_environment_id" '
        {name:$name,
         config:{providerType:"doppler",serviceToken:"capture-vault-token-1",project:"capture",config:"staging"},
         assignments:[{projectId:$project,environmentIds:[$environment]}]}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{vaultProviderId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg name "$3" --arg project "$capture_project_id" --arg environment "$capture_environment_id" '
        {vaultProviderId:$id,name:$name,
         config:{providerType:"doppler",serviceToken:"capture-vault-token-2",project:"capture",config:"production"},
         assignments:[{projectId:$project,environmentIds:[$environment]}]}' >"$1"
}
