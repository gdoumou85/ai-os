#!/usr/bin/env bash
# Phase 2a acceptance: the real model, three hands, no human. The desktop unit must be up.
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export AI_OS_DISPLAY_INVISIBLE=wayland-ai AI_OS_DISPLAY_VISIBLE=wayland-0 AI_OS_LIVE=1
# A cold distro starts the unit with us; give it a moment before giving up on it.
for _ in $(seq 60); do systemctl --user is-active --quiet ai-os-desktop.service && break; sleep 1; done
systemctl --user is-active ai-os-desktop.service || { echo "desktop unit not up: run trial/setup-desktop.sh as root"; exit 1; }
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && timeout 1500 cargo test -p aios-core --test live_2a -- --nocapture > /tmp/live-2a.log 2>&1
status=$?
echo "exit $status (full log: /tmp/live-2a.log)"
tail -120 /tmp/live-2a.log
exit $status
