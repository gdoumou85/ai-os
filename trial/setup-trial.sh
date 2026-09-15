#!/usr/bin/env bash
# Phase 0 trial setup. Run as root inside the ai-os distro:
#   bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh <section>
# Sections: base gpu desktop-gnome apps kde snapshot. Each is safe to re-run.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
REPO=/mnt/c/Users/gdoum/Desktop/projects/ai-os
section=${1:?section name required}

base() {
  cat >/etc/wsl.conf <<'EOF'
[boot]
systemd=true
[user]
default=ai
[interop]
appendWindowsPath=false
EOF
  id ai >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash -G sudo ai
  echo 'ai ALL=(ALL) NOPASSWD:ALL' >/etc/sudoers.d/ai
  chmod 440 /etc/sudoers.d/ai
  apt-get update
  apt-get install -y curl git python3 python3-gi gir1.2-glib-2.0 jq
  install -d -o ai -g ai /home/ai/probes
  cp "$REPO"/trial/probes/* /home/ai/probes/ 2>/dev/null || true
  chown -R ai:ai /home/ai/probes
}

gpu() {
  # Ollama is the trial runner only; the product picks its runner in Phase 1.
  apt-get install -y zstd   # the Ollama installer unpacks with zstd
  command -v ollama >/dev/null || curl -fsSL https://ollama.com/install.sh | sh
  # q8_0 KV cache: on 8 GB this is what lets an 8k context stay fully on the GPU (measured in Task 2)
  install -d /etc/systemd/system/ollama.service.d
  printf '[Service]\nEnvironment=OLLAMA_FLASH_ATTENTION=1\nEnvironment=OLLAMA_KV_CACHE_TYPE=q8_0\n' >/etc/systemd/system/ollama.service.d/kv.conf
  systemctl daemon-reload
  systemctl enable --now ollama
  systemctl restart ollama
  # One multimodal model (spec decision 13). qwen3.5:9b: newest small model with vision + tools (checked 2026-09-15).
  sudo -u ai ollama pull "${MODEL_TAG:-qwen3.5:9b}"
}

desktop_gnome() {
  apt-get install -y ubuntu-desktop-minimal gnome-remote-desktop gnome-session gdm3 \
    xdg-desktop-portal xdg-desktop-portal-gnome pipewire wireplumber \
    python3-pyatspi gnome-text-editor gstreamer1.0-pipewire gstreamer1.0-tools gstreamer1.0-plugins-good
  systemctl set-default graphical.target
  # WSLg mounts /tmp/.X11-unix read-only; gnome-shell aborts when it cannot start XWayland there.
  # Replace it with a writable folder at every boot (WSL re-creates the mount on start).
  cat >/etc/systemd/system/ai-os-x11-unix.service <<'EOF'
[Unit]
Description=Writable /tmp/.X11-unix for GNOME under WSL
Before=gdm.service
[Service]
Type=oneshot
ExecStart=/bin/sh -c 'umount /tmp/.X11-unix 2>/dev/null; rm -rf /tmp/.X11-unix; mkdir -m1777 /tmp/.X11-unix'
[Install]
WantedBy=multi-user.target
EOF
  systemctl enable ai-os-x11-unix.service
  # attempt A: system-level headless RDP login (GNOME 46+ "remote login"); trial-only password, localhost only
  # the system daemon refuses to listen without a TLS certificate; a self-signed one is enough for localhost
  install -d -m 0755 /etc/ai-os-trial   # the daemon runs as its own user and must be able to enter the folder
  [ -f /etc/ai-os-trial/rdp-key.pem ] || openssl req -x509 -newkey rsa:2048 -nodes -days 365 -subj "/CN=ai-os" \
    -keyout /etc/ai-os-trial/rdp-key.pem -out /etc/ai-os-trial/rdp-cert.pem 2>/dev/null
  chown gnome-remote-desktop: /etc/ai-os-trial/rdp-*.pem 2>/dev/null || true
  chmod 0600 /etc/ai-os-trial/rdp-key.pem; chmod 0644 /etc/ai-os-trial/rdp-cert.pem
  grdctl --system rdp set-tls-key /etc/ai-os-trial/rdp-key.pem || true
  grdctl --system rdp set-tls-cert /etc/ai-os-trial/rdp-cert.pem || true
  grdctl --system rdp set-credentials ai trial-only || true
  grdctl --system rdp enable || true
  systemctl enable --now gnome-remote-desktop.service || true
  systemctl enable gdm3 || true
}

apps() {
  # Firefox and Chrome as debs (the snap versions add confinement that would muddy the a11y result).
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc
  echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" >/etc/apt/sources.list.d/mozilla.list
  printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' >/etc/apt/preferences.d/mozilla
  curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor --yes -o /etc/apt/keyrings/google.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" >/etc/apt/sources.list.d/google-chrome.list
  curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor --yes -o /etc/apt/keyrings/microsoft.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main" >/etc/apt/sources.list.d/vscode.list
  apt-get update
  # --allow-downgrades: Mozilla's repo pins a Firefox older than Ubuntu's own snap-wrapper package
  apt-get install -y --allow-downgrades firefox google-chrome-stable code libreoffice-writer
  # tell toolkits an assistive technology is present
  sudo -u ai dbus-launch gsettings set org.gnome.desktop.interface toolkit-accessibility true || true
}

kde() {
  # Plasma 6: the Wayland session is in kwin-wayland + plasma-workspace, not a separate -wayland pkg
  apt-get install -y kde-plasma-desktop kwin-wayland plasma-workspace xdg-desktop-portal-kde konsole kate
}

snapshot() {
  apt-get install -y btrfs-progs
  install -d /var/lib/ai-os /data
  if [ ! -f /var/lib/ai-os/data.img ]; then
    truncate -s 50G /var/lib/ai-os/data.img      # sparse; the product sizes this from free space
    mkfs.btrfs -q -L ai-os-data /var/lib/ai-os/data.img
  fi
  grep -q ' /data ' /etc/fstab || echo '/var/lib/ai-os/data.img /data btrfs loop,noatime,compress=zstd 0 0' >>/etc/fstab
  mountpoint -q /data || mount /data
  [ -d /data/live ] || btrfs subvolume create /data/live
  install -d /data/snapshots
  chown ai:ai /data/live
}

case "$section" in
  base) base ;;
  gpu) gpu ;;
  desktop-gnome) desktop_gnome ;;
  apps) apps ;;
  kde) kde ;;
  snapshot) snapshot ;;
  *) echo "unknown section: $section" >&2; exit 2 ;;
esac
echo "section $section done"
