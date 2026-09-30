def normalized_scalar:
  if .key == "projectId" then
    if .value == null then . else .value = "project-1" end
  elif .key == "environmentId" then
    if .value == null then . else .value = "environment-1" end
  elif .key == "applicationId" then
    if .value == null then . else .value = "application-1" end
  elif .key == "postgresId" then
    if .value == null then . else .value = "postgres-1" end
  elif .key == "redisId" then
    if .value == null then . else .value = "redis-1" end
  elif .key == "domainId" then
    if .value == null then . else .value = "domain-1" end
  elif .key == "uniqueConfigKey" then
    if .value == null then . else .value = 1 end
  elif .key == "host" then
    if .value == null then
      .
    elif (.value | startswith("iac-domain-contract-created-")) then
      .value = "created.domain.example.test"
    elif (.value | startswith("iac-domain-contract-updated-")) then
      .value = "updated.domain.example.test"
    else
      .
    end
  elif .key == "mountId" then
    if .value == null then . else .value = "mount-1" end
  elif .key == "volumeName" then
    if .value == null then . else .value = "volume-1" end
  elif .key == "organizationId" then
    if .value == null then . else .value = "organization-1" end
  elif .key == "ownerId" or .key == "userId" then
    if .value == null then . else .value = "user-1" end
  elif .key == "createdAt" or .key == "updatedAt" then
    if .value == null then . else .value = "2026-09-28T00:00:00.000Z" end
  elif .key == "env"
    or .key == "previewEnv"
    or .key == "buildArgs"
    or .key == "previewBuildArgs"
    or .key == "buildSecrets"
    or .key == "previewBuildSecrets"
  then
    if .value == null or .value == "" then . else .value = "<redacted>" end
  elif (.key | test("(?i)(password|secret|token|privatekey|accesskey)")) then
    if .value == null then . else .value = "<redacted>" end
  else
    .
  end;

walk(
  if type == "object" then
    with_entries(normalized_scalar)
    | if has("applicationId") and has("appName") then
        .appName = "application-contract-test"
      elif has("postgresId") and has("appName") then
        .appName = "postgres-contract-test"
      elif has("redisId") then
        (if has("appName") then .appName = "redis-contract-test" else . end)
        | (if has("name") then .name = "Redis Contract Test" else . end)
        | (if has("description") and .description != null then
            .description = "Disposable Redis contract capture"
          else
            .
          end)
      else
        .
      end
  else
    .
  end
)
