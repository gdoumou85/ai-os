#!/usr/bin/env python3
"""AI OS Phase 2b probe 2: can the AI grab a screenshot and put the pointer on an exact spot?
Needs:  sudo apt install -y gstreamer1.0-tools gstreamer1.0-pipewire
Run:    python3 /media/sf_shrd/probe-screen.py
Watch the pointer: it should jump to the middle of the screen, then to the top-left corner area.
Writes the screenshot to /media/sf_shrd/probe-shot.png (you can open it on Windows too)."""
import os, subprocess, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
loop = GLib.MainLoop()
RD, SC, DC = "org.gnome.Mutter.RemoteDesktop", "org.gnome.Mutter.ScreenCast", "org.gnome.Mutter.DisplayConfig"
shot = "/media/sf_shrd/probe-shot.png"
if not os.access("/media/sf_shrd", os.W_OK):
    shot = os.path.expanduser("~/probe-shot.png")
out = {}


def call(dest, path, iface, method, sig=None, args=None, reply=None):
    v = GLib.Variant(sig, args) if sig else None
    return bus.call_sync(dest, path, iface, method, v, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, -1, None)


try:
    state = call(DC, "/org/gnome/Mutter/DisplayConfig", DC, "GetCurrentState").unpack()
    mon = state[1][0]
    connector = mon[0][0]
    mode = next((m for m in mon[1] if m[6].get("is-current")), mon[1][0])
    w, h = mode[1], mode[2]
    out["monitor"] = f"{connector} {w}x{h}"

    rd = call(RD, "/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", reply="(o)").unpack()[0]
    rd_id = call(RD, rd, "org.freedesktop.DBus.Properties", "Get", "(ss)", (RD + ".Session", "SessionId")).unpack()[0]
    sc = call(SC, "/org/gnome/Mutter/ScreenCast", SC, "CreateSession", "(a{sv})",
              ({"remote-desktop-session-id": GLib.Variant("s", rd_id)},), "(o)").unpack()[0]
    stream = call(SC, sc, SC + ".Session", "RecordMonitor", "(sa{sv})",
                  (connector, {"cursor-mode": GLib.Variant("u", 1)}), "(o)").unpack()[0]
    node = {}

    def added(c, s, p, i, sig, params):
        node["id"] = params.unpack()[0]; loop.quit()

    bus.signal_subscribe(SC, SC + ".Stream", "PipeWireStreamAdded", stream, None, 0, added)
    call(RD, rd, RD + ".Session", "Start")
    GLib.timeout_add_seconds(15, loop.quit)
    loop.run()
    if "id" not in node:
        raise RuntimeError("no PipeWire stream appeared")
    out["stream_node"] = node["id"]

    print(">>> Watch the pointer: middle of the screen...")
    call(RD, rd, RD + ".Session", "NotifyPointerMotionAbsolute", "(sdd)", (stream, w / 2, h / 2))
    time.sleep(2)
    print(">>> ...now near the top-left corner.")
    call(RD, rd, RD + ".Session", "NotifyPointerMotionAbsolute", "(sdd)", (stream, 150.0, 150.0))
    time.sleep(1)

    r = subprocess.run(["gst-launch-1.0", "-e", "-q", "pipewiresrc", f"path={node['id']}", "num-buffers=3",
                        "!", "videoconvert", "!", "pngenc", "snapshot=true", "!", "filesink", f"location={shot}"],
                       capture_output=True, text=True, timeout=30)
    size = os.path.getsize(shot) if os.path.exists(shot) else 0
    out["screenshot"] = f"{shot} ({size} bytes)" if size > 1000 else f"FAILED: {r.stderr.strip()[:300]}"
    call(RD, rd, RD + ".Session", "Stop")
except FileNotFoundError:
    out["screenshot"] = "gst-launch-1.0 missing: run  sudo apt install -y gstreamer1.0-tools gstreamer1.0-pipewire"
except Exception as e:
    out["error"] = str(e)[:400]

print("\n===== RESULT (send a screenshot of this) =====")
for k, v in out.items():
    print(f"{k}: {v}")
print("Did the pointer jump to the middle, then to the top-left? (tell me yes/no)")
