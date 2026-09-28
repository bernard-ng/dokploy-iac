#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

prepare_runtime
compose up --detach --wait --wait-timeout 240

"$script_directory/bootstrap.sh"

echo "Dokploy integration instance is healthy at $base_url"
