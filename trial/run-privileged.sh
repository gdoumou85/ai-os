#!/usr/bin/env bash
# U5, the path the product actually uses: session-owner capture + input, no portal dialog.
source ~/session.env
cd ~/probes
echo "== capture via org.gnome.Shell.Screenshot (no dialog)"
gdbus call --session --dest org.gnome.Shell.Screenshot \
  --object-path /org/gnome/Shell/Screenshot \
  --method org.gnome.Shell.Screenshot.Screenshot true false "$HOME/probes/shot.png" 2>&1
sleep 1
ls -la ~/probes/shot.png 2>&1
file ~/probes/shot.png 2>&1
echo "== Mutter ScreenCast interface present?"
busctl --user introspect org.gnome.Mutter.ScreenCast /org/gnome/Mutter/ScreenCast 2>&1 | grep -E 'CreateSession|RecordMonitor|RecordVirtual' | head
echo "== input device path for uinput (ydotool/uinput)"
ls -l /dev/uinput 2>&1
grep -w uinput /proc/misc 2>&1 || echo "uinput not in /proc/misc"
lsmod 2>/dev/null | grep -w uinput || (modprobe uinput 2>&1; grep -w uinput /proc/misc 2>&1)
echo "== ydotool available?"
command -v ydotool ydotoold 2>&1 || echo "ydotool not installed"
