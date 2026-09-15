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

case "$section" in
  base) base ;;
  *) echo "unknown section: $section" >&2; exit 2 ;;
esac
echo "section $section done"
