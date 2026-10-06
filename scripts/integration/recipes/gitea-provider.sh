# shellcheck shell=bash
recipe_kind=gitea_provider
recipe_fixture=gitea
recipe_id=gitProviderId
recipe_id_label=git-provider-1
recipe_one_name=gitProvider.name
recipe_list=gitProvider.getAll
recipe_one=gitea.one
recipe_one_param=giteaId
recipe_one_id=gitea.giteaId
recipe_create=gitea.create
recipe_update=gitea.update
recipe_remove=gitProvider.remove
recipe_secrets=".clientSecret .accessToken"

recipe_create_body() {
    jq -n --arg name "$2" '
        {name:$name,giteaUrl:"https://gitea.invalid",giteaInternalUrl:"http://gitea.internal",
         redirectUri:"https://dokploy.invalid/gitea/callback",clientId:"capture-client",
         clientSecret:"capture-gitea-secret-1",accessToken:"capture-gitea-token-1"}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{gitProviderId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg provider "$one_object_id" --arg name "$3" '
        {gitProviderId:$id,giteaId:$provider,name:$name,giteaUrl:"https://gitea-updated.invalid",
         giteaInternalUrl:"http://gitea-updated.internal",
         redirectUri:"https://dokploy.invalid/gitea/callback-2",clientId:"capture-client-2",
         clientSecret:"capture-gitea-secret-2",accessToken:"capture-gitea-token-2"}' >"$1"
}
