# shellcheck shell=bash
recipe_kind=gitlab_provider
recipe_fixture=gitlab
recipe_id=gitProviderId
recipe_id_label=git-provider-1
recipe_one_name=gitProvider.name
recipe_list=gitProvider.getAll
recipe_one=gitlab.one
recipe_one_param=gitlabId
recipe_one_id=gitlab.gitlabId
recipe_create=gitlab.create
recipe_update=gitlab.update
recipe_remove=gitProvider.remove
recipe_secrets=".secret"

recipe_create_body() {
    jq -n --arg name "$2" '
        {authId:"declarative-capture",name:$name,gitlabUrl:"https://gitlab.invalid",
         gitlabInternalUrl:"http://gitlab.internal",applicationId:"capture-app",
         secret:"capture-gitlab-secret-1",groupName:"capture-group",
         redirectUri:"https://dokploy.invalid/gitlab/callback"}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{gitProviderId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg provider "$one_object_id" --arg name "$3" '
        {gitProviderId:$id,gitlabId:$provider,name:$name,gitlabUrl:"https://gitlab-updated.invalid",
         gitlabInternalUrl:"http://gitlab-updated.internal",applicationId:"capture-app-2",
         secret:"capture-gitlab-secret-2",groupName:"capture-group-2",
         redirectUri:"https://dokploy.invalid/gitlab/callback-2"}' >"$1"
}
