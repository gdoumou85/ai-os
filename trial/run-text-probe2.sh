#!/usr/bin/env bash
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
source ~/session.env; export WAYLAND_DISPLAY=wayland-0 DISPLAY=:0
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/text_probe2.py ~/probes/
pkill -u ai -f 'soffice.bin|gnome-text-editor' 2>/dev/null; sleep 2; cd ~/probes
python3 text_probe2.py editor
python3 text_probe2.py writer
echo PROBE2-DONE
