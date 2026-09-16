#!/usr/bin/env bash
# Phase 1c admin setup. Run as root inside the ai-os distro. Idempotent.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
# test-admin.sh's "new /etc dir stays root's" check leaves an empty /etc/ai-os-t1c behind:
# nothing in the wrapper's menu removes a directory outside /data and /home/ai, which is the
# boundary that check is about. Clear it here so a reinstall-then-test run really exercises the
# case again instead of asserting about a directory that was already there.
rmdir /etc/ai-os-t1c 2>/dev/null || true
apt-get install -y python3-venv npm cargo curl
install -m 0755 -o root -g root "$repo/runtime/admin/ai-os-admin" /usr/local/libexec/ai-os-admin
sed -i 's/\r$//' /usr/local/libexec/ai-os-admin
# One grant, nothing else. Removes the workshop's NOPASSWD:ALL and the bare systemd-run line.
rm -f /etc/sudoers.d/ai /etc/sudoers.d/ai-sandbox-run
# Written to a .tmp name (sudo ignores names containing a dot), validated, then moved in.
# `install /dev/stdin` is not idempotent: it fails on an already-present 0440 destination.
echo 'ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin' > /etc/sudoers.d/ai-os-admin.tmp
chmod 0440 /etc/sudoers.d/ai-os-admin.tmp
visudo -cf /etc/sudoers.d/ai-os-admin.tmp
mv -f /etc/sudoers.d/ai-os-admin.tmp /etc/sudoers.d/ai-os-admin
# The one grant is the whole grant: drop ai from the sudo group so nothing else can be reached.
deluser ai sudo 2>/dev/null || gpasswd -d ai sudo 2>/dev/null || true
install -d -o ai -g ai-sandbox -m 2770 /data/snapshots /data/housekeeping
# Leftovers from a script that once ran with CRLF endings.
rmdir "/data/jobs"$'\r' "/data/projects"$'\r' 2>/dev/null || true
echo "admin ready"
