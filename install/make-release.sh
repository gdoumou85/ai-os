#!/usr/bin/env bash
# Builds dist/ai-os-linux-amd64.tar.gz (desktop design §9.3). Run on Ubuntu 26.04 — the GitHub build
# job does, inside an ubuntu:26.04 container: the rail needs GTK 4.18, and binaries built there
# promise nothing on an older release.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
(cd "$root/runtime" && cargo build --release)
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
install -d "$stage/ai-os/bin"
for b in ai-os-engine ai-os-chat ai-os-rail; do install -m 0755 "$root/runtime/target/release/$b" "$stage/ai-os/bin/$b"; done
install -m 0755 "$root/install/install.sh" "$root/install/check.sh" "$root/runtime/admin/ai-os-admin" "$stage/ai-os/"
install -m 0644 "$root/install/ai-os-engine.service.in" "$root/runtime/rail/org.aios.Rail.desktop" "$stage/ai-os/"
git -C "$root" describe --tags --always --dirty > "$stage/ai-os/VERSION"
sed -i 's/\r$//' "$stage/ai-os/"*.sh "$stage/ai-os/ai-os-admin" "$stage/ai-os/"*.in "$stage/ai-os/"*.desktop
install -d "$root/dist"
tar -C "$stage" --owner=0 --group=0 -czf "$root/dist/ai-os-linux-amd64.tar.gz" ai-os
echo "built dist/ai-os-linux-amd64.tar.gz ($(cat "$stage/ai-os/VERSION"))"
