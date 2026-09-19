#!/usr/bin/env python3
"""AI OS Phase 2b probe: can GNOME tell the person's input apart from the AI's?
Run it in Terminal:  python3 /media/sf_shrd/probe-stop-if-you-move.py
Follow what it prints. It moves the mouse pointer a little by itself once."""
import time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

bus = Gio.bus_get_sync(Gio.BusType.SESSION)


def call(dest, path, iface, method, sig=None, args=None, reply=None):
    v = GLib.Variant(sig, args) if sig else None
    return bus.call_sync(dest, path, iface, method, v, GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, -1, None)


def idle():
    return call("org.gnome.Mutter.IdleMonitor", "/org/gnome/Mutter/IdleMonitor/Core",
                "org.gnome.Mutter.IdleMonitor", "GetIdletime", reply="(t)").unpack()[0]


RD = "org.gnome.Mutter.RemoteDesktop"
out = {}
try:
    sess = call(RD, "/org/gnome/Mutter/RemoteDesktop", RD, "CreateSession", reply="(o)").unpack()[0]
    call(RD, sess, RD + ".Session", "Start")
    out["session"] = "ok"
except Exception as e:
    out["session"] = f"FAILED: {e}"
    sess = None

print("\n>>> Do NOT touch the mouse or keyboard for 5 seconds...")
time.sleep(5)
out["idle_before_ai_move_ms"] = idle()

if sess:
    # the AI moves the pointer 40 px right and back
    call(RD, sess, RD + ".Session", "NotifyPointerMotionRelative", "(dd)", (40.0, 0.0))
    time.sleep(0.2)
    call(RD, sess, RD + ".Session", "NotifyPointerMotionRelative", "(dd)", (-40.0, 0.0))
    time.sleep(0.5)
    out["idle_after_ai_move_ms"] = idle()

print(">>> Now MOVE THE MOUSE a little (you have 5 seconds)...")
low = 10**9
end = time.time() + 5
while time.time() < end:
    low = min(low, idle())
    time.sleep(0.1)
out["lowest_idle_while_you_moved_ms"] = low

if sess:
    try:
        call(RD, sess, RD + ".Session", "Stop")
    except Exception:
        pass

print("\n===== RESULT (send a screenshot of this) =====")
for k, v in out.items():
    print(f"{k}: {v}")
if sess and "idle_after_ai_move_ms" in out:
    ai_resets = out["idle_after_ai_move_ms"] < 2000
    you_reset = out["lowest_idle_while_you_moved_ms"] < 1000
    print("AI's move counts as activity:", "YES" if ai_resets else "NO")
    print("Your move counts as activity:", "YES" if you_reset else "NO")
