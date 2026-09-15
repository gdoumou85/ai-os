#!/usr/bin/env python3
"""Does an AT-SPI action (in-process, no seat) actually act? Toggle LibreOffice's Bold button
and read its checked-state before/after. This is a different path from synthetic keystrokes."""
import json, os, time
import pyatspi
desk = pyatspi.Registry.getDesktop(0)
out = {"found_bold": False, "checked_before": None, "checked_after": None, "changed": None, "error": None}


def writer():
    for a in desk:
        if "soffice" in (a.name or "").lower() and a.childCount:
            return a
    return None


def walk(a, limit=9000):
    st = [a]
    while st and limit > 0:
        n = st.pop(); limit -= 1
        yield n
        try:
            st.extend(n[i] for i in range(min(n.childCount, 400)))
        except Exception:
            pass


try:
    app = writer()
    if not app:
        raise RuntimeError("LibreOffice not open")
    bold = None
    for n in walk(app):
        try:
            if n.getRoleName() in ("toggle button", "push button") and (n.name or "").strip() == "Bold":
                bold = n; break
        except Exception:
            pass
    if bold is None:
        raise RuntimeError("Bold button not found")
    out["found_bold"] = True
    out["checked_before"] = bold.getState().contains(pyatspi.STATE_CHECKED)
    bold.queryAction().doAction(0)
    time.sleep(1)
    out["checked_after"] = bold.getState().contains(pyatspi.STATE_CHECKED)
    out["changed"] = out["checked_after"] != out["checked_before"]
    bold.queryAction().doAction(0)  # toggle back
except Exception as e:
    out["error"] = str(e)
print(json.dumps(out))
