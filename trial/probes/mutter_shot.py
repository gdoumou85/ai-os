#!/usr/bin/env python3
"""Capture one frame of the REAL headless monitor via org.gnome.Mutter.ScreenCast.RecordMonitor
(no portal dialog). Finds the connector from Mutter DisplayConfig. Writes ~/probes/shot.png."""
import json, os, subprocess, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

SC = "org.gnome.Mutter.ScreenCast"
DC = "org.gnome.Mutter.DisplayConfig"
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
loop = GLib.MainLoop()
out = {}
shot = os.path.expanduser("~/probes/shot.png")


def call(dest, obj, iface, method, sig, args):
    return bus.call_sync(dest, obj, iface, method, GLib.Variant(sig, args), None, Gio.DBusCallFlags.NONE, -1, None)


try:
    state = call(DC, "/org/gnome/Mutter/DisplayConfig", DC, "GetCurrentState", "()", ()).unpack()
    monitors = state[1]
    connector = monitors[0][0][0]  # first monitor, monitor spec, connector
    out["connector"] = connector
    sess = call(SC, "/org/gnome/Mutter/ScreenCast", SC, "CreateSession", "(a{sv})", ({},)).unpack()[0]
    stream = call(SC, sess, SC + ".Session", "RecordMonitor", "(sa{sv})",
                  (connector, {"cursor-mode": GLib.Variant("u", 1)})).unpack()[0]
    node = {}

    def on_added(conn, s, path, i, sig, params):
        node["id"] = params.unpack()[0]
        loop.quit()

    bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, 0, on_added)
    call(SC, sess, SC + ".Session", "Start", "()", ())
    GLib.timeout_add_seconds(20, loop.quit)
    loop.run()
    out["node_id"] = node.get("id")
    if not node:
        raise RuntimeError("no PipeWireStreamAdded")
    time.sleep(2)
    subprocess.run(["gst-launch-1.0", "-e", "-q", "pipewiresrc", f"path={node['id']}", "num-buffers=5",
                    "!", "videoconvert", "!", "pngenc", "!", "filesink", f"location={shot}"],
                   check=True, timeout=30)
    sz = os.path.getsize(shot) if os.path.exists(shot) else 0
    out["screenshot"] = shot if sz > 1000 else "empty"
    out["bytes"] = sz
except Exception as e:
    out["error"] = str(e)
print(json.dumps(out))
