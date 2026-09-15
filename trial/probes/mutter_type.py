#!/usr/bin/env python3
"""Definitive typing test: a Mutter RemoteDesktop session LINKED to a ScreenCast monitor stream
(the way gnome-remote-desktop pairs them), click into the document, then synthesize keystrokes.
Ground truth = the LibreOffice word count read via AT-SPI."""
import json, os, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
import pyatspi
from gi.repository import Gio, GLib

RD = "org.gnome.Mutter.RemoteDesktop"
SC = "org.gnome.Mutter.ScreenCast"
DC = "org.gnome.Mutter.DisplayConfig"
PROP = "org.freedesktop.DBus.Properties"
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
loop = GLib.MainLoop()
desk = pyatspi.Registry.getDesktop(0)
TEXT = "Hello from the AI."
out = {"typed": TEXT, "statusbar_before": None, "statusbar_after": None, "node_id": None, "error": None}


def call(dest, obj, iface, method, sig, args):
    return bus.call_sync(dest, obj, iface, method, GLib.Variant(sig, args), None, Gio.DBusCallFlags.NONE, -1, None)


def writer():
    for a in desk:
        if "soffice" in (a.name or "").lower() and a.childCount:
            return a
    return None


def walk(a, limit=8000):
    st = [a]
    while st and limit > 0:
        n = st.pop(); limit -= 1
        yield n
        try:
            st.extend(n[i] for i in range(min(n.childCount, 400)))
        except Exception:
            pass


def words(app):
    for n in walk(app):
        try:
            t = n.queryText().getText(0, -1)
            if "word" in t and "character" in t:
                return t
        except Exception:
            pass
    return None


try:
    app = writer()
    out["statusbar_before"] = words(app) if app else None
    # 1) RemoteDesktop session + its id
    rd = call(RD, "/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", "()", ()).unpack()[0]
    rd_id = call(RD, rd, PROP, "Get", "(ss)", (RD + ".Session", "SessionId")).unpack()[0]
    # 2) ScreenCast session linked to it
    sc = call(SC, "/org/gnome/Mutter/ScreenCast", SC, "CreateSession", "(a{sv})",
              ({"remote-desktop-session-id": GLib.Variant("s", rd_id)},)).unpack()[0]
    state = call(DC, "/org/gnome/Mutter/DisplayConfig", DC, "GetCurrentState", "()", ()).unpack()
    connector = state[1][0][0][0]
    stream = call(SC, sc, SC + ".Session", "RecordMonitor", "(sa{sv})",
                  (connector, {"cursor-mode": GLib.Variant("u", 1)})).unpack()[0]
    node = {}

    def on_added(conn, s, path, i, sig, params):
        node["id"] = params.unpack()[0]; loop.quit()

    bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, 0, on_added)
    call(RD, rd, RD + ".Session", "Start", "()", ())    # starts both halves
    GLib.timeout_add_seconds(15, loop.quit); loop.run()
    out["node_id"] = node.get("id")
    time.sleep(2)
    # 3) click into the middle of the page to place the caret, then type
    call(RD, rd, RD + ".Session", "NotifyPointerMotionAbsolute", "(sdd)", (stream, 760.0, 500.0)); time.sleep(0.3)
    call(RD, rd, RD + ".Session", "NotifyPointerButton", "(ib)", (0x110, True)); time.sleep(0.05)
    call(RD, rd, RD + ".Session", "NotifyPointerButton", "(ib)", (0x110, False)); time.sleep(0.5)
    for ch in TEXT:
        ks = ord(ch)
        call(RD, rd, RD + ".Session", "NotifyKeyboardKeysym", "(ub)", (ks, True))
        call(RD, rd, RD + ".Session", "NotifyKeyboardKeysym", "(ub)", (ks, False))
        time.sleep(0.04)
    time.sleep(2)
    out["statusbar_after"] = words(app) if app else None
except Exception as e:
    out["error"] = str(e)
print(json.dumps(out))
