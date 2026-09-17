#!/usr/bin/env bash
# Task 5's live check: the desktop hand against a real window in the invisible session.
# The session must already be up (Task 8 makes it a unit).
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export AI_OS_DISPLAY_INVISIBLE=${AI_OS_DISPLAY_INVISIBLE:-wayland-ai} AI_OS_DISPLAY_VISIBLE=wayland-0 AI_OS_DESKTOP=1
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo test -p executor --test desktop_live -- --nocapture 2>&1 | tail -30
