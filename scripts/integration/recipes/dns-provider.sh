# shellcheck shell=bash
recipe_kind=dns_provider
recipe_fixture=dnsProvider
recipe_id=dnsProviderId
recipe_id_label=dns-provider-1
recipe_list=dnsProvider.all
recipe_one=dnsProvider.one
recipe_create=dnsProvider.create
recipe_update=dnsProvider.update
recipe_remove=dnsProvider.remove
recipe_secrets=".config.apiToken"

recipe_create_body() {
    jq -n --arg name "$2" '{name:$name,config:{providerType:"cloudflare",apiToken:"capture-api-token-1"}}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{dnsProviderId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg name "$3" '
        {dnsProviderId:$id,name:$name,config:{providerType:"cloudflare",apiToken:"capture-api-token-2"}}' >"$1"
}
