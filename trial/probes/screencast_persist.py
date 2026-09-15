#!/usr/bin/env python3
"""U5, capture half: a ScreenCast-only session with persist_mode=2 (persistent).
First run saves a restore token; second run reuses it. If the second run needs no dialog and
returns a stream, screenshot permission is remembered. Prints JSON per run."""
import json, os, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

TOKEN_FILE = os.path.expanduser("~/probes/sc_restore_token")
PORTAL = "org.freedesktop.portal.Desktop"
PATH = "/org/freedesktop/portal/desktop"
SC = "org.freedesktop.portal.ScreenCast"
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
sender = bus.get_unique_name()[1:].replace(".", "_")
loop = GLib.MainLoop()
counter = [0]


def call(method, sig, args, opts):
    counter[0] += 1
    tok = f"t{counter[0]}"
    req_path = f"{PATH}/request/{sender}/{tok}"
    got = {}

    def on_resp(conn, s, path, i, signame, params):
        got["code"], got["res"] = params.unpack()
        loop.quit()

    sub = bus.signal_subscribe(PORTAL, "org.freedesktop.portal.Request", "Response", req_path, None, 0, on_resp)
    opts = dict(opts, handle_token=GLib.Variant("s", tok))
    bus.call_sync(PORTAL, PATH, SC, method, GLib.Variant(sig, (*args, opts)), None, Gio.DBusCallFlags.NONE, -1, None)
    GLib.timeout_add_seconds(120, loop.quit)
    loop.run()
    bus.signal_unsubscribe(sub)
    if got.get("code", -1) != 0:
        raise RuntimeError(f"{method}: response code {got.get('code', 'timeout')}")
    return got["res"]


restore = open(TOKEN_FILE).read().strip() if os.path.exists(TOKEN_FILE) else None
out = {"dialog_expected": restore is None}
res = call("CreateSession", "(a{sv})", (), {"session_handle_token": GLib.Variant("s", f"s{int(time.time())}")})
session = res["session_handle"]
src_opts = {"types": GLib.Variant("u", 1), "persist_mode": GLib.Variant("u", 2)}
if restore:
    src_opts["restore_token"] = GLib.Variant("s", restore)
call("SelectSources", "(oa{sv})", (session,), src_opts)
res = call("Start", "(osa{sv})", (session, ""), {})
token = res.get("restore_token")
out["restore_token_saved"] = bool(token)
if token:
    open(TOKEN_FILE, "w").write(token)
out["stream_started"] = len(res.get("streams", [])) > 0
print(json.dumps(out))
