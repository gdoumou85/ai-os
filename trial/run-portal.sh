#!/usr/bin/env bash
# Task 5 (U5): does GNOME remember screen permission?
#  - screencast_persist.py twice: capture-only session with persist; is the 2nd run dialog-free?
#  - portal_probe.py once: prove input injection + capture work through the portal at all.
# portal_approve.py watches the a11y bus and clicks the permission dialog the way a user would.
source ~/session.env
cd ~/probes
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/*.py .
[ -e sc_restore_token ] && unlink sc_restore_token
[ -e restore_token ] && unlink restore_token
[ -e shot.png ] && unlink shot.png
systemctl --user is-active graphical-session.target xdg-desktop-portal-gnome xdg-desktop-portal | tr '\n' ' '; echo
echo "== screencast persist, run 1 (dialog expected)"
(python3 portal_approve.py >approve1.log 2>&1 &); sleep 1
timeout 150 python3 screencast_persist.py
echo "== screencast persist, run 2 (should be dialog-free)"
(python3 portal_approve.py >approve2.log 2>&1 &); sleep 1
timeout 150 python3 screencast_persist.py
echo "== remote-desktop input + capture (one-shot)"
(python3 portal_approve.py >approve3.log 2>&1 &); sleep 1
timeout 150 python3 portal_probe.py
echo "== approver logs"; tail -1 approve1.log; tail -1 approve2.log; tail -1 approve3.log
ls -la shot.png 2>&1
