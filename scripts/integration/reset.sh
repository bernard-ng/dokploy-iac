#!/usr/bin/env bash

set -euo pipefail

script_directory="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# shellcheck source=common.sh
source "$script_directory/common.sh"

compose down --remove-orphans --volumes

if [[ -d "$runtime_directory" ]]; then
    find "$runtime_directory" -mindepth 1 -delete
fi

echo "Removed the disposable Dokploy integration data and credentials."
