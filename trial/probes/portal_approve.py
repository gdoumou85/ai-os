#!/usr/bin/env python3
"""Watches the a11y bus for the desktop's screen-share permission dialog and presses its accept
button, the way the user would. Runs for up to 100 s. Prints what it pressed, or 'no dialog'."""
import sys, time
import pyatspi

ACCEPT = {"share", "allow", "start", "select", "ok"}
desk = pyatspi.Registry.getDesktop(0)
deadline = time.time() + 100
pressed = []

def walk(acc, limit=1500):
    stack = [acc]
    while stack and limit > 0:
        n = stack.pop(); limit -= 1
        yield n
        try:
            stack.extend(n[i] for i in range(min(n.childCount, 100)))
        except Exception:
            pass

while time.time() < deadline:
    for app in desk:
        name = (app.name or "").lower()
        if "portal" not in name and "gnome-shell" not in name:
            continue
        try:
            for node in walk(app):
                if node.getRoleName() in ("push button", "toggle button") and (node.name or "").strip().lower() in ACCEPT:
                    if node.getState().contains(pyatspi.STATE_SENSITIVE):
                        node.queryAction().doAction(0)
                        pressed.append(f"{app.name}:{node.name}")
                        time.sleep(1.5)
        except Exception:
            pass
    if len(pressed) >= 2:
        break
    time.sleep(1)
print("pressed:", pressed if pressed else "no dialog")
