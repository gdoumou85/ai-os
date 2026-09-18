#!/usr/bin/env bash
# Downloads the latest AI OS release and runs its installer with the same options:
#   curl -fsSL https://raw.githubusercontent.com/gdoumou85/ai-os/master/install/get.sh | bash -s -- [options]
set -euo pipefail
REPO=${AI_OS_REPO_SLUG:-gdoumou85/ai-os}
dir=$(mktemp -d); trap 'rm -rf "$dir"' EXIT
curl -fSL "https://github.com/$REPO/releases/latest/download/ai-os-linux-amd64.tar.gz" | tar -xz -C "$dir"
bash "$dir/ai-os/install.sh" "$@"
