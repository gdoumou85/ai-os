#!/usr/bin/env bash
# Phase 1d workshop setup. Run as root inside the ai-os distro after a release build. Idempotent.
# WORKSHOP ONLY (parent spec §11): binaries are copied from the Windows-mounted repo; the product
# ships them from a package.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
apt-get install -y libgtk-4-dev
for b in ai-os-engine ai-os-chat ai-os-rail; do
  src="$repo/runtime/target/release/$b"
  [ -f "$src" ] || { echo "skip $b (not built)"; continue; }
  install -m 0755 -o root -g root "$src" /usr/local/bin/$b
done
# Open on a text file must land in the text editor. The distro's default for text/x-python is
# LibreOffice Writer, which opens a .py as a document with an import dialog. Set ai's own
# defaults; a distro without the editor installed just says so. Idempotent.
editor=org.gnome.TextEditor.desktop
if ls /usr/share/applications/$editor >/dev/null 2>&1; then
  runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 xdg-mime default $editor \
    text/x-python text/markdown text/plain text/x-shellscript application/json
else
  echo "skip mime defaults ($editor not installed)"
fi
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
install -m 0644 -o ai -g ai "$repo/trial/ai-os-engine.service" /home/ai/.config/systemd/user/ai-os-engine.service
sed -i 's/\r$//' /home/ai/.config/systemd/user/ai-os-engine.service
loginctl enable-linger ai   # already yes on the workshop; idempotent, needed on a fresh install
# ai's own service manager, addressed from root (systemd 259 supports -M user@).
systemctl --user -M ai@ daemon-reload
systemctl --user -M ai@ enable ai-os-engine.service
systemctl --user -M ai@ restart ai-os-engine.service
echo "rail ready"
