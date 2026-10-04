# shellcheck shell=bash
recipe_kind=certificate
recipe_fixture=certificates
recipe_id=certificateId
recipe_id_label=certificate-1
recipe_list=certificates.all
recipe_one=certificates.one
recipe_create=certificates.create
recipe_update=certificates.update
recipe_remove=certificates.remove
recipe_secrets=".privateKey"

recipe_setup() {
    openssl req -x509 -newkey rsa:2048 -nodes -keyout "$workspace/key.pem" -out "$workspace/cert.pem" \
        -days 30 -subj "/CN=capture.invalid" 2>/dev/null
    api_request GET organization.active "$workspace/organization.json" >/dev/null
}

recipe_create_body() {
    jq -n --rawfile certificate "$workspace/cert.pem" --rawfile key "$workspace/key.pem" \
        --slurpfile organization "$workspace/organization.json" --arg name "$2" '
        {name:$name,certificateData:$certificate,privateKey:$key,organizationId:$organization[0].id}' >"$1"
}

recipe_partial_body() {
    jq -n --arg id "$2" --arg name "$3" '{certificateId:$id,name:$name}' >"$1"
}

recipe_full_body() {
    jq -n --rawfile certificate "$workspace/cert.pem" --rawfile key "$workspace/key.pem" \
        --arg id "$2" --arg name "$3" '
        {certificateId:$id,name:$name,certificateData:$certificate,privateKey:$key}' >"$1"
}
