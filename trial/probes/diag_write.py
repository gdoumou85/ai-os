#!/usr/bin/env python3
"""Diagnose whether AT-SPI writing actually commits to the LibreOffice document.
Lists soffice windows, tries insertText, and reads the status-bar word count as ground truth."""
import json, os, subprocess, time
import pyatspi

env = dict(os.environ, SAL_USE_VCLPLUGIN="gtk3", GNOME_ACCESSIBILITY="1", ACCESSIBILITY_ENABLED="1")
desk = pyatspi.Registry.getDesktop(0)
SENT = "Live write via accessibility."
out = {"windows": [], "insert_readback": None, "statusbar_before": None, "statusbar_after": None,
       "method": None, "error": None}


def writer():
    for a in desk:
        if "soffice" in (a.name or "").lower() and a.childCount:
            return a
    return None


def walk(acc, limit=8000):
    st = [acc]
    while st and limit > 0:
        n = st.pop(); limit -= 1
        yield n
        try:
            st.extend(n[i] for i in range(min(n.childCount, 400)))
        except Exception:
            pass


def statusbar_words(app):
    for n in walk(app):
        try:
            t = n.queryText().getText(0, -1)
            if "word" in t and "character" in t:
                return t
        except Exception:
            pass
    return None


try:
    subprocess.run(["pkill", "-u", os.environ["USER"], "-f", "soffice.bin"], check=False)
    time.sleep(3)
    subprocess.Popen(["libreoffice", "--writer", "--norestore"], env=env,
                     stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    for _ in range(45):
        time.sleep(1); app = writer()
        if app:
            break
    time.sleep(6)
    for w in app:
        try:
            out["windows"].append(f"{w.getRoleName()}:{w.name}")
        except Exception:
            pass
    out["statusbar_before"] = statusbar_words(app)
    # find the main editable document body (biggest editable text)
    target = None
    for n in walk(app):
        try:
            if n.getRoleName() in ("text", "document frame", "document text") and \
               n.getState().contains(pyatspi.STATE_EDITABLE):
                target = n; out["method"] = "EditableText.insertText on role " + n.getRoleName()
                break
        except Exception:
            pass
    if target is None:
        raise RuntimeError("no editable doc body")
    # 1) try grabbing focus then insert
    try:
        target.queryComponent().grabFocus(); time.sleep(0.5)
    except Exception:
        pass
    target.queryEditableText().insertText(0, SENT, len(SENT))
    time.sleep(2)
    out["insert_readback"] = SENT in target.queryText().getText(0, -1)
    out["statusbar_after"] = statusbar_words(app)
except Exception as e:
    out["error"] = str(e)
print(json.dumps(out))
