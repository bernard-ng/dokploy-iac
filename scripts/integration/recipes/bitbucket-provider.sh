# shellcheck shell=bash
recipe_kind=bitbucket_provider
recipe_fixture=bitbucket
recipe_id=gitProviderId
recipe_id_label=git-provider-1
recipe_one_name=gitProvider.name
recipe_list=gitProvider.getAll
recipe_one=bitbucket.one
recipe_one_param=bitbucketId
recipe_one_id=bitbucket.bitbucketId
recipe_create=bitbucket.create
recipe_update=bitbucket.update
recipe_remove=gitProvider.remove
recipe_secrets=".appPassword .apiToken"

recipe_create_body() {
    jq -n --arg name "$2" '
        {authId:"declarative-capture",name:$name,bitbucketUsername:"capture-ci",
         bitbucketEmail:"capture@example.com",appPassword:"capture-bitbucket-password-1",
         apiToken:"capture-bitbucket-token-1",bitbucketWorkspaceName:"capture-workspace"}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{gitProviderId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg provider "$one_object_id" --arg name "$3" '
        {gitProviderId:$id,bitbucketId:$provider,name:$name,bitbucketUsername:"capture-ci-2",
         bitbucketEmail:"capture-2@example.com",appPassword:"capture-bitbucket-password-2",
         apiToken:"capture-bitbucket-token-2",bitbucketWorkspaceName:"capture-workspace-2"}' >"$1"
}
