#!/usr/bin/env bash
# Throwaway 2a probe runner: starts the invisible shell, then four cells on two displays, in one call.
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
grep -v "sudo loginctl" /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/headless-session.sh > /tmp/hs.sh
bash /tmp/hs.sh 2>&1 | grep -E "WAYLAND_DISPLAY|active"
source ~/session.env
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/text_probe.py ~/probes/
pkill -u ai -f 'soffice.bin|gnome-text-editor' 2>/dev/null; sleep 2
cd ~/probes
ls /run/user/1000 | grep wayland
unset DISPLAY
echo "== invisible session ($WAYLAND_DISPLAY)"
python3 text_probe.py editor-headless "text-editor" gnome-text-editor --new-window
python3 text_probe.py writer-headless soffice libreoffice --writer --norestore
sleep 2
export WAYLAND_DISPLAY=wayland-0 DISPLAY=:0
echo "== WSLg display ($WAYLAND_DISPLAY)"
python3 text_probe.py editor-wslg "text-editor" gnome-text-editor --new-window
python3 text_probe.py writer-wslg soffice libreoffice --writer --norestore
echo PROBE-DONE
