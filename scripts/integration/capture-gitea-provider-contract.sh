#!/usr/bin/env bash
# Records the Gitea provider lifecycle through the shared Git provider collection.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"
# shellcheck source=capture-recipe-lib.sh
source "$script_directory/capture-recipe-lib.sh"

capture_recipe gitea-provider
