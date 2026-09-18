#!/usr/bin/env bash
# The desktop edition's guest setup (desktop design §3). Root, idempotent, run by cloud-init on the
# first boot and by hand after a rebuild of the binaries:  sudo is not available to ai, so from the
# VM's console as root, or again through cloud-init by rebuilding the VM.
# WORKSHOP ONLY (parent spec §11): binaries and scripts come from the shared repo; the product ships
# them from a package.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export AI_OS_REPO=${AI_OS_REPO:-/mnt/repo}
rel="$AI_OS_REPO/runtime/target/release"
for b in ai-os-engine ai-os-chat ai-os-rail; do
  [ -f "$rel/$b" ] || { echo "$rel/$b is missing: build the release in WSL first" >&2; exit 1; }
done

echo "== /data on the second disk"
disk=/dev/sdb
[ -b "$disk" ] || { echo "no second disk at $disk" >&2; exit 1; }
blkid "$disk" >/dev/null 2>&1 || mkfs.btrfs -q -L ai-os-data "$disk"
install -d /data
grep -q ' /data ' /etc/fstab || echo 'LABEL=ai-os-data /data btrfs noatime,compress=zstd 0 0' >>/etc/fstab
mountpoint -q /data || mount /data
[ -d /data/live ] || btrfs subvolume create /data/live
install -d /data/snapshots
chown ai:ai /data/live

echo "== the desktop"
# linux-generic: the cloud image's kernel flavour may lack modules-extra (the VMSVGA display driver);
# the reboot that ends the first boot picks the new kernel up.
apt-get update
apt-get install -y ubuntu-desktop-minimal gdm3 xdg-desktop-portal-gnome pipewire wireplumber \
  gnome-text-editor gnome-calculator libgtk-4-1 python3-gi linux-generic
systemctl set-default graphical.target
runuser -u ai -- dbus-run-session gsettings set org.gnome.desktop.interface toolkit-accessibility true || true

echo "== sandbox user and the wrapper"
bash "$AI_OS_REPO/trial/setup-sandbox-user.sh"
install -d -m 0755 /usr/local/libexec   # Debian's /usr/local has no libexec by default
bash "$AI_OS_REPO/trial/setup-admin.sh"

echo "== binaries"
for b in ai-os-engine ai-os-chat ai-os-rail; do install -m 0755 -o root -g root "$rel/$b" /usr/local/bin/$b; done

echo "== the engine as ai's service"
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
install -m 0644 -o ai -g ai "$AI_OS_REPO/trial/vm/ai-os-engine.service" /home/ai/.config/systemd/user/ai-os-engine.service
sed -i 's/\r$//' /home/ai/.config/systemd/user/ai-os-engine.service
loginctl enable-linger ai
# ai's manager may not be up yet on the first boot; enabling by symlink needs no manager.
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user enable ai-os-engine.service 2>/dev/null \
  || { install -d -o ai -g ai /home/ai/.config/systemd/user/default.target.wants
       ln -sf ../ai-os-engine.service /home/ai/.config/systemd/user/default.target.wants/ai-os-engine.service
       chown -h ai:ai /home/ai/.config/systemd/user/default.target.wants/ai-os-engine.service; }

echo "== the rail with the session"
install -m 0644 -o root -g root "$AI_OS_REPO/runtime/rail/org.aios.Rail.desktop" /usr/share/applications/org.aios.Rail.desktop
install -d /etc/xdg/autostart
install -m 0644 -o root -g root "$AI_OS_REPO/runtime/rail/org.aios.Rail.desktop" /etc/xdg/autostart/org.aios.Rail.desktop
sed -i 's/\r$//' /usr/share/applications/org.aios.Rail.desktop /etc/xdg/autostart/org.aios.Rail.desktop

echo "== text files open in the editor"
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 xdg-mime default org.gnome.TextEditor.desktop \
  text/x-python text/markdown text/plain text/x-shellscript application/json || true

VBoxControl guestproperty set /ai-os/setup ready
echo "desktop edition set up"
