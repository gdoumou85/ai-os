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
# the portal services are activated by the user manager and must know this is a GNOME Wayland session,
# otherwise xdg-desktop-portal loads no backend and RemoteDesktop/ScreenCast are missing
systemctl --user set-environment XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=$display GNOME_ACCESSIBILITY=1
dbus-update-activation-environment --systemd XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=$display
# xdg-desktop-portal-gnome is PartOf=graphical-session.target, which only a real gnome-session raises;
# for the trial, allow raising it by hand (the product runs a proper session and will not need this)
mkdir -p ~/.config/systemd/user/graphical-session.target.d
printf '[Unit]\nRefuseManualStart=no\n' > ~/.config/systemd/user/graphical-session.target.d/manual.conf
systemctl --user daemon-reload
systemctl --user start graphical-session.target
systemctl --user restart xdg-desktop-portal-gnome xdg-desktop-portal 2>/dev/null || true
systemctl --user is-active ai-headless-shell.service
echo "WAYLAND_DISPLAY=$display"
busctl --user list | grep -E 'org.a11y|org.gnome.Mutter.RemoteDesktop|org.gnome.Mutter.ScreenCast|org.gnome.Shell.Screenshot' || true
