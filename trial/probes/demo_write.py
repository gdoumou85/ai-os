#!/usr/bin/env python3
"""Prove tier-2 live write: type a sentence INTO the open LibreOffice document via AT-SPI
(not by writing a file), then leave it on screen for a capture. Prints what it did."""
import json, os, subprocess, sys, time
import pyatspi

SENTENCE = sys.argv[1] if len(sys.argv) > 1 else "The AI wrote this line directly into the open document."
env = dict(os.environ, SAL_USE_VCLPLUGIN="gtk3", GNOME_ACCESSIBILITY="1", ACCESSIBILITY_ENABLED="1")
desk = pyatspi.Registry.getDesktop(0)


def find_writer():
    for app in desk:
        if "soffice" in (app.name or "").lower() and app.childCount:
            return app
    return None


def walk(acc, limit=6000):
    stack = [acc]
    while stack and limit > 0:
        n = stack.pop(); limit -= 1
        yield n
        try:
            stack.extend(n[i] for i in range(min(n.childCount, 300)))
        except Exception:
            pass


out = {"typed": SENTENCE, "readback_ok": None, "target_role": None, "error": None}
try:
    app = find_writer()
    if not app:
        subprocess.Popen(["libreoffice", "--writer", "--norestore"], env=env,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        for _ in range(45):
            time.sleep(1)
            app = find_writer()
            if app:
                break
        time.sleep(5)
    if not app:
        raise RuntimeError("LibreOffice never appeared on the a11y bus")
    # the main document body is the editable "document frame"/"text" with the biggest text buffer
    target = None
    for n in walk(app):
        try:
            role = n.getRoleName()
            if role in ("document frame", "text", "document text") and n.getState().contains(pyatspi.STATE_EDITABLE):
                target = n
                out["target_role"] = role
                break
        except Exception:
            pass
    if target is None:
        raise RuntimeError("no editable document body found")
    et = target.queryEditableText()
    et.insertText(0, SENTENCE, len(SENTENCE))
    time.sleep(1.5)
    got = target.queryText().getText(0, -1)
    out["readback_ok"] = SENTENCE in got
    out["document_now_contains"] = got[:120]
except Exception as e:
    out["error"] = str(e)
print(json.dumps(out))
