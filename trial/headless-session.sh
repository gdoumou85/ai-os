#!/usr/bin/env bash
# Start (or restart) an invisible GNOME session for user ai: gnome-shell headless with a virtual monitor,
# running under the user's systemd manager on the user's real session bus. Nothing is displayed anywhere.
# Probes run inside it with:  source ~/session.env
set -e
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
sudo loginctl enable-linger ai >/dev/null
systemctl --user stop ai-headless-shell.service 2>/dev/null || true
systemctl --user reset-failed ai-headless-shell.service 2>/dev/null || true
: >~/headless.log
systemd-run --user --unit=ai-headless-shell \
  --setenv=XDG_SESSION_TYPE=wayland --setenv=XDG_CURRENT_DESKTOP=GNOME \
  --property=StandardOutput=append:/home/ai/headless.log --property=StandardError=append:/home/ai/headless.log \
  gnome-shell --headless --virtual-monitor 1600x900 --wayland --no-x11
sleep 8
display=$(grep -o "Wayland display name '[^']*'" ~/headless.log | tail -1 | cut -d"'" -f2)
cat >~/session.env <<EOF
export XDG_RUNTIME_DIR=/run/user/1000
export DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export WAYLAND_DISPLAY=$display
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME
export GNOME_ACCESSIBILITY=1 ACCESSIBILITY_ENABLED=1 QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
EOF
gsettings set org.gnome.desktop.interface toolkit-accessibility true
systemctl --user is-active ai-headless-shell.service
echo "WAYLAND_DISPLAY=$display"
busctl --user list | grep -E 'org.a11y|org.gnome.Mutter.RemoteDesktop|org.gnome.Mutter.ScreenCast|org.gnome.Shell.Screenshot' || true
