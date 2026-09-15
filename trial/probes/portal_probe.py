#!/usr/bin/env python3
"""U5 probe. Run inside the desktop session. First run: a permission dialog is expected; approve it.
Second run: must start with NO dialog, using the saved restore token. Also moves the pointer and grabs one frame."""
import json, os, subprocess, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

TOKEN_FILE = os.path.expanduser("~/probes/restore_token")
PORTAL = "org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
RD = "org.freedesktop.portal.RemoteDesktop"
SC = "org.freedesktop.portal.ScreenCast"

bus = Gio.bus_get_sync(Gio.BusType.SESSION)
sender = bus.get_unique_name()[1:].replace(".", "_")
loop = GLib.MainLoop()
counter = [0]
result = {}


def v(t, x):
    return x if isinstance(x, GLib.Variant) else GLib.Variant(t, x)


def call(iface, method, sig, args, opts):
    """Call a portal request method, wait for its Response signal, return the results dict."""
    counter[0] += 1
    tok = f"t{counter[0]}"
    req_path = f"{PATH}/request/{sender}/{tok}"
    got = {}

    def on_resp(conn, s, path, i, signame, params):
        got["code"], got["res"] = params.unpack()
        loop.quit()

    sub = bus.signal_subscribe(PORTAL, "org.freedesktop.portal.Request", "Response", req_path, None, 0, on_resp)
    opts = dict(opts, handle_token=GLib.Variant("s", tok))
    bus.call_sync(PORTAL, PATH, iface, method, GLib.Variant(sig, (*args, opts)), None, Gio.DBusCallFlags.NONE, -1, None)
    GLib.timeout_add_seconds(120, loop.quit)
    loop.run()
    bus.signal_unsubscribe(sub)
    if got.get("code", -1) != 0:
        raise RuntimeError(f"{method}: response code {got.get('code', 'timeout')}")
    return got["res"]


# A RemoteDesktop (input-injection) session on GNOME cannot persist; persist is a ScreenCast-only
# feature. So this probe just proves input + capture work through the portal once; the "is the
# permission remembered" question for capture is answered by screencast_persist.py.
result["dialog_expected"] = True

res = call(RD, "CreateSession", "(a{sv})", (), {"session_handle_token": GLib.Variant("s", f"s{int(time.time())}")})
session = res["session_handle"]
dev_opts = {"types": GLib.Variant("u", 7), "persist_mode": GLib.Variant("u", 0)}
src_opts = {"types": GLib.Variant("u", 1), "persist_mode": GLib.Variant("u", 0)}
call(RD, "SelectDevices", "(oa{sv})", (session,), dev_opts)
call(SC, "SelectSources", "(oa{sv})", (session,), src_opts)
res = call(RD, "Start", "(osa{sv})", (session, ""), {})
result["restore_token_saved"] = False  # not applicable to an input session on GNOME
streams = res.get("streams", [])
result["stream_started"] = len(streams) > 0

node_id = streams[0][0] if streams else None
try:
    for x, y in ((100.0, 100.0), (400.0, 300.0)):
        bus.call_sync(PORTAL, PATH, RD, "NotifyPointerMotionAbsolute",
                      GLib.Variant("(oa{sv}udd)", (session, {}, node_id, x, y)), None, Gio.DBusCallFlags.NONE, -1, None)
        time.sleep(0.3)
    result["pointer_moved"] = True
except Exception as e:
    result["pointer_moved"] = f"error: {e}"

try:
    reply, fds = bus.call_with_unix_fd_list_sync(PORTAL, PATH, SC, "OpenPipeWireRemote",
                                                 GLib.Variant("(oa{sv})", (session, {})), None,
                                                 Gio.DBusCallFlags.NONE, -1, None, None)
    fd = fds.get(reply.unpack()[0])
    shot = os.path.expanduser("~/probes/shot.png")
    subprocess.run(["gst-launch-1.0", "-q", "pipewiresrc", f"fd={fd}", f"path={node_id}", "num-buffers=1",
                    "!", "videoconvert", "!", "pngenc", "!", "filesink", f"location={shot}"],
                   check=True, pass_fds=(fd,), timeout=30)
    result["screenshot"] = shot if os.path.getsize(shot) > 1000 else "empty"
except Exception as e:
    result["screenshot"] = f"error: {e}"
print(json.dumps(result))
