#!/usr/bin/env bash
#
# Records the live contract of the Dokploy ssh-key endpoints with the recipe runner
# (capture-recipe-lib.sh, recipes/ssh-key.sh) as sanitised fixtures for the selected version.

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=common.sh
source "$script_directory/common.sh"
# shellcheck source=capture-lib.sh
source "$script_directory/capture-lib.sh"
# shellcheck source=capture-recipe-lib.sh
source "$script_directory/capture-recipe-lib.sh"

capture_recipe ssh-key
