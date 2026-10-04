# shellcheck shell=bash
recipe_kind=ai_provider
recipe_fixture=ai
recipe_id=aiId
recipe_id_label=ai-1
recipe_list=ai.getAll
recipe_one=ai.one
recipe_create=ai.create
recipe_update=ai.update
recipe_remove=ai.delete
recipe_secrets="apiKey"

recipe_create_body() {
    jq -n --arg name "$2" '
        {name:$name,apiUrl:"https://api.openai.invalid/v1",apiKey:"capture-api-key-1",model:"gpt-4.1",isEnabled:true}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{aiId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --arg id "$2" --arg name "$3" '
        {aiId:$id,name:$name,apiUrl:"https://api.openai.invalid/v1",apiKey:"capture-api-key-2",model:"gpt-5",isEnabled:false}' >"$1"
}
