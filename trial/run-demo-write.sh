#!/usr/bin/env bash
# Prove the AI writes INTO the live document, then capture the window to show it.
source ~/session.env
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/demo_write.py ~/probes/
pkill -u ai -f soffice.bin 2>/dev/null || true
sleep 2
echo "== type into the live document via AT-SPI"
python3 ~/probes/demo_write.py "The AI wrote this line directly into the open document, on 2026-09-15."
echo "== capture the window"
python3 ~/probes/mutter_shot.py
cp ~/probes/shot.png /mnt/c/Users/gdoum/Desktop/projects/ai-os/docs/superpowers/findings/u3-live-write.png 2>/dev/null && echo copied
