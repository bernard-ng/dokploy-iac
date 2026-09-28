def normalized_scalar:
  if .key == "projectId" then
    .value = "project-1"
  elif .key == "environmentId" then
    .value = "environment-1"
  elif .key == "applicationId" then
    .value = "application-1"
  elif .key == "postgresId" then
    .value = "postgres-1"
  elif .key == "mountId" then
    .value = "mount-1"
  elif .key == "organizationId" then
    .value = "organization-1"
  elif .key == "ownerId" or .key == "userId" then
    .value = "user-1"
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
      else
        .
      end
  else
    .
  end
)
