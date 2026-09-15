#!/usr/bin/env bash
# Capture the real headless monitor via Mutter RecordMonitor (LibreOffice open); fall back to a
# generated labelled image; then run the model vision check on whichever has pixels.
source ~/session.env
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/mutter_shot.py ~/probes/
command -v convert >/dev/null || sudo DEBIAN_FRONTEND=noninteractive apt-get install -y imagemagick >/dev/null 2>&1
pkill -u ai -f soffice.bin 2>/dev/null || true
rm -f ~/probes/shot.png
(libreoffice --writer --norestore >/dev/null 2>&1 &)
sleep 14
echo "== mutter RecordMonitor"; python3 ~/probes/mutter_shot.py
img=~/probes/shot.png
if [ ! -s "$img" ]; then
  echo "== real capture empty; generating a labelled test image"
  img=~/probes/synthetic.png
  convert -size 800x400 xc:white -fill black -pointsize 22 \
    -draw "text 20,40 'LibreOffice Writer'" \
    -draw "rectangle 20,70 780,380" \
    -fill '#3465a4' -draw "rectangle 20,70 780,110" \
    -fill white -draw "text 30,98 'File  Edit  View  Insert  Format'" \
    -fill black -draw "text 40,200 'The quarterly report is due Friday.'" "$img"
fi
echo "== vision check on $img"
python3 ~/probes/model_probe.py qwen3.5:9b "$img"
