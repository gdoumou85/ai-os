#!/usr/bin/env bash
# Phase 2a workshop setup. Run as root inside the ai-os distro after a release build. Idempotent.
# WORKSHOP ONLY (parent spec §11): files are copied from the Windows-mounted repo.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
apt-get install -y gnome-text-editor gnome-calculator >/dev/null
install -m 0755 -o root -g root "$repo/trial/ai-os-desktop-env" /usr/local/libexec/ai-os-desktop-env
sed -i 's/\r$//' /usr/local/libexec/ai-os-desktop-env
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
for u in ai-os-desktop.service ai-os-engine.service; do
  install -m 0644 -o ai -g ai "$repo/trial/$u" /home/ai/.config/systemd/user/$u
  sed -i 's/\r$//' /home/ai/.config/systemd/user/$u
done
if [ -f "$repo/runtime/target/release/ai-os-engine" ]; then install -m 0755 -o root -g root "$repo/runtime/target/release/ai-os-engine" /usr/local/bin/ai-os-engine; fi
# The Phase 0 hand-run session must not sit beside the unit on the same bus.
systemctl --user -M ai@ stop ai-headless-shell.service 2>/dev/null || true
systemctl --user -M ai@ reset-failed ai-headless-shell.service 2>/dev/null || true
loginctl enable-linger ai
systemctl --user -M ai@ daemon-reload
systemctl --user -M ai@ enable ai-os-desktop.service ai-os-engine.service
systemctl --user -M ai@ restart ai-os-desktop.service
systemctl --user -M ai@ restart ai-os-engine.service
sleep 3
systemctl --user -M ai@ is-active ai-os-desktop.service ai-os-engine.service
# busctl --user needs the bus address too; XDG_RUNTIME_DIR alone leaves it on the local transport,
# which root-started runuser is not permitted to use.
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus busctl --user list | grep -E 'org.a11y.Bus|org.gnome.Mutter.ScreenCast' || true
echo "desktop ready"
