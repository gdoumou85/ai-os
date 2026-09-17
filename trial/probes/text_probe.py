#!/usr/bin/env python3
"""Throwaway 2a probe: does AT-SPI text entry commit? Run inside the session env.
Usage: WAYLAND_DISPLAY=<display> text_probe.py <label> <app-substring> <launch command...>
Cells: insert = EditableText.insertText; paste = wl-copy then EditableText.pasteText;
menu_paste = Edit > Paste menu item action. Each read back through Text.getText on a fresh
walk, plus any status-bar label mentioning words (Writer's real commit signal)."""
import json, os, subprocess, sys, time
import pyatspi

label, name, cmd = sys.argv[1], sys.argv[2], sys.argv[3:]
env = dict(os.environ, GNOME_ACCESSIBILITY="1", ACCESSIBILITY_ENABLED="1",
           QT_LINUX_ACCESSIBILITY_ALWAYS_ON="1", SAL_USE_VCLPLUGIN="gtk3")
desk = pyatspi.Registry.getDesktop(0)
out = {"cell": label, "display": os.environ.get("WAYLAND_DISPLAY"), "insert": None, "paste": None,
       "menu_paste": None, "words": {}, "editable_role": None, "error": None}


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


def find_editable(app):
    best = None
    for node in walk(app):
        try:
            role, st = node.getRoleName(), node.getState()
            node.queryEditableText()
        except Exception:
            continue
        if not st.contains(pyatspi.STATE_EDITABLE):
            continue
        if role in ("paragraph", "document text", "text") and st.contains(pyatspi.STATE_SHOWING):
            return node
        best = best or node
    return best


def read_all(app, needle):
    """Fresh walk: is `needle` in any text node? Plus word-count labels."""
    found, words = False, {}
    for node in walk(app):
        try:
            role = node.getRoleName()
            if role in ("label", "status bar", "text", "paragraph", "document text"):
                txt = ""
                try:
                    txt = node.queryText().getText(0, -1)
                except Exception:
                    txt = node.name or ""
                if needle in txt:
                    found = True
                if "word" in txt.lower() and len(txt) < 80:
                    words[role] = txt
        except Exception:
            continue
    return found, words


def menu_item(app, menu_name, item_sub):
    for node in walk(app):
        try:
            if node.getRoleName() == "menu" and (node.name or "") == menu_name:
                node.queryAction().doAction(0)
                time.sleep(1.5)
                for i in range(min(node.childCount, 40)):
                    it = node[i]
                    if item_sub.lower() in (it.name or "").lower():
                        return node, it
                node.queryAction().doAction(0)
        except Exception:
            pass
    return None, None


proc = None
try:
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    for _ in range(45):
        time.sleep(1)
        app = find_app()
        if app:
            break
    if not app:
        raise RuntimeError("app never appeared on the a11y bus")
    time.sleep(5)
    ed = find_editable(app)
    if ed is None:
        raise RuntimeError("no editable node")
    out["editable_role"] = ed.getRoleName()

    # cell 1: insert
    ed.queryEditableText().insertText(0, "INSERTED-ai-os ", 15)
    time.sleep(1.5)
    out["insert"], w = read_all(app, "INSERTED-ai-os")
    out["words"]["after_insert"] = w

    # cell 2: paste through the widget's own interface
    subprocess.run(["wl-copy", "PASTED-ai-os "], env=env, check=True)
    time.sleep(0.5)
    ed = find_editable(app)
    n = ed.queryText().characterCount
    ed.queryEditableText().pasteText(n)
    time.sleep(1.5)
    out["paste"], w = read_all(app, "PASTED-ai-os")
    out["words"]["after_paste"] = w

    # cell 3: paste through the Edit menu's item action
    subprocess.run(["wl-copy", "MENUPASTED-ai-os "], env=env, check=True)
    time.sleep(0.5)
    menu, item = menu_item(app, "Edit", "paste")
    if item is None:
        out["menu_paste"] = "no Edit>Paste item"
    else:
        try:
            item.queryAction().doAction(0)
            time.sleep(1.5)
            out["menu_paste"], w = read_all(app, "MENUPASTED-ai-os")
            out["words"]["after_menu_paste"] = w
        except Exception as e:
            out["menu_paste"] = f"error: {e}"
except Exception as e:
    out["error"] = str(e)
finally:
    if proc is not None:
        proc.terminate()
        try:
            proc.wait(5)
        except Exception:
            proc.kill()
print(json.dumps(out))
