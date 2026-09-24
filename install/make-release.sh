#!/usr/bin/env bash
# Builds dist/ai-os-linux-amd64.tar.gz (desktop design §9.3). Run on Ubuntu 26.04 — the GitHub build
# job does, inside an ubuntu:26.04 container: the rail needs GTK 4.18, and binaries built there
# promise nothing on an older release.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# The version is taken before the build so a failed build cannot leave a tarball describing a
# tree that was never compiled; --locked so the release uses the lockfile as committed.
version=$(git -C "$root" describe --tags --always --dirty)
(cd "$root/runtime" && cargo build --locked --release)
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
install -d "$stage/ai-os/bin"
for b in ai-os-engine ai-os-chat ai-os-rail ai-os-find ai-os-alert; do install -m 0755 "$root/runtime/target/release/$b" "$stage/ai-os/bin/$b"; done
install -m 0755 "$root/install/install.sh" "$root/install/check.sh" "$stage/ai-os/"
install -m 0644 "$root/install/ai-os-engine.service.in" "$root/runtime/rail/org.aios.Rail.desktop" "$stage/ai-os/"
printf '%s\n' "$version" > "$stage/ai-os/VERSION"
sed -i 's/\r$//' "$stage/ai-os/"*.sh "$stage/ai-os/"*.in "$stage/ai-os/"*.desktop
install -d "$root/dist"
tar -C "$stage" --owner=0 --group=0 -czf "$root/dist/ai-os-linux-amd64.tar.gz" ai-os
echo "built dist/ai-os-linux-amd64.tar.gz ($(cat "$stage/ai-os/VERSION"))"
