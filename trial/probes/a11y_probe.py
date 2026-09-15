#!/usr/bin/env python3
"""U3/U4 probe. Run INSIDE the headless session (source ~/session.env first).
Usage: a11y_probe.py <app-name-substring> <launch command...>
Env DECOY overrides the decoy editor (default gnome-text-editor; use kate on KDE).
Prints one JSON line: nodes, buttons, editable, menu_opened, text_roundtrip, focus_stolen, roles, error."""
import json, os, subprocess, sys, time
from collections import Counter
import pyatspi

name, cmd = sys.argv[1], sys.argv[2:]
env = dict(os.environ, GNOME_ACCESSIBILITY="1", ACCESSIBILITY_ENABLED="1",
           QT_LINUX_ACCESSIBILITY_ALWAYS_ON="1", SAL_USE_VCLPLUGIN="gtk3")
desk = pyatspi.Registry.getDesktop(0)
PROBE_TEXT = "ai-os probe"


def active_window():
    for app in desk:
        try:
            for w in app:
                if w.getState().contains(pyatspi.STATE_ACTIVE):
                    return f"{app.name}:{w.name}"
        except Exception:
            pass
    return None


def find_app():
    for app in desk:
        if name.lower() in (app.name or "").lower():
            try:
                if app.childCount:
                    return app
            except Exception:
                pass
    return None


def walk(acc, limit=6000):
    stack = [acc]
    while stack and limit > 0:
        node = stack.pop()
        limit -= 1
        yield node
        try:
            stack.extend(node[i] for i in range(min(node.childCount, 300)))
        except Exception:
            pass


out = {"app": name, "nodes": 0, "buttons": 0, "editable": 0, "menu_opened": None,
       "text_roundtrip": None, "focus_stolen": None, "roles": {}, "error": None}
decoy = proc = None
roles = Counter()
try:
    decoy = subprocess.Popen([os.environ.get("DECOY", "gnome-text-editor"), "--new-window"], env=env,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(4)
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    for _ in range(45):          # LibreOffice/Electron can take 20 s+
        time.sleep(1)
        app = find_app()
        if app:
            break
    if not app:
        raise RuntimeError("app never appeared on the a11y bus")
    time.sleep(4)
    focus_before = active_window()
    editable = menu = None
    for node in walk(app):
        out["nodes"] += 1
        try:
            role = node.getRoleName()
            st = node.getState()
        except Exception:
            continue
        roles[role] += 1
        if role in ("push button", "toggle button", "button"):
            out["buttons"] += 1
        # editable = anything that exposes the EditableText interface (role names vary by toolkit)
        is_editable = False
        try:
            node.queryEditableText()
            is_editable = True
        except Exception:
            is_editable = role in ("text", "entry", "document web", "document text", "paragraph",
                                   "document frame") and st.contains(pyatspi.STATE_EDITABLE)
        if is_editable:
            out["editable"] += 1
            editable = editable or node
        if role == "menu" and menu is None and (node.name or "") in ("File", "Edit", "View"):
            menu = node
    if menu is not None:
        try:
            menu.queryAction().doAction(0)
            time.sleep(1.5)
            out["menu_opened"] = any(menu[i].getState().contains(pyatspi.STATE_SHOWING)
                                     for i in range(min(menu.childCount, 30)))
            menu.queryAction().doAction(0)
        except Exception as e:
            out["menu_opened"] = f"error: {e}"
    if editable is not None:
        try:
            et = editable.queryEditableText()
            et.insertText(0, PROBE_TEXT, len(PROBE_TEXT))
            time.sleep(1)
            got = editable.queryText().getText(0, -1)
            out["text_roundtrip"] = PROBE_TEXT in got
        except Exception as e:
            out["text_roundtrip"] = f"error: {e}"
    time.sleep(1)
    out["focus_after"] = active_window()
    out["focus_before"] = focus_before
    out["focus_stolen"] = (out["focus_after"] != focus_before) if focus_before else None
    out["roles"] = dict(roles.most_common(8))
except Exception as e:
    out["error"] = str(e)
    out["roles"] = dict(roles.most_common(8))
finally:
    for p in (proc, decoy):
        if p is not None:
            p.terminate()
print(json.dumps(out))
