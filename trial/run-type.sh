#!/usr/bin/env bash
source ~/session.env
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/mutter_type.py ~/probes/
pkill -u ai -f soffice.bin 2>/dev/null || true
sleep 3
(libreoffice --writer --norestore >/dev/null 2>&1 &)
sleep 14
echo "== synthesize keystrokes via compositor RemoteDesktop"
python3 ~/probes/mutter_type.py
echo "== capture"
python3 ~/probes/mutter_shot.py
cp ~/probes/shot.png /mnt/c/Users/gdoum/Desktop/projects/ai-os/docs/superpowers/findings/u3-live-write.png 2>/dev/null && echo copied
