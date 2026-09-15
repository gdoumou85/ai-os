#!/usr/bin/env bash
# Task 4: run the accessibility probe on the four apps inside the headless session.
source ~/session.env
pkill -u ai -f 'firefox|google-chrome|/usr/share/code|soffice.bin' 2>/dev/null || true
sleep 2
cd ~/probes
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/a11y_probe.py .
: >a11y-gnome.jsonl
PAGE='data:text/html,<button>Hello</button><input placeholder=name><textarea></textarea>'
python3 a11y_probe.py firefox firefox --new-window "$PAGE"                                        | tee -a a11y-gnome.jsonl
python3 a11y_probe.py chrome  google-chrome --no-sandbox --force-renderer-accessibility --ozone-platform=wayland "$PAGE" | tee -a a11y-gnome.jsonl
# VS Code (Electron): single-instance, so let the probe be the only launcher. --disable-gpu for headless.
python3 a11y_probe.py code    code --no-sandbox --force-renderer-accessibility --disable-gpu --new-window | tee -a a11y-gnome.jsonl
python3 a11y_probe.py soffice libreoffice --writer --norestore                                    | tee -a a11y-gnome.jsonl
cp a11y-gnome.jsonl /mnt/c/Users/gdoum/Desktop/projects/ai-os/docs/superpowers/findings/
echo A11Y-DONE