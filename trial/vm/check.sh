#!/usr/bin/env bash
# The desktop edition's smoke check (desktop design §6): the first failing line is the answer.
# WORKSHOP ONLY. Run as ai inside the VM.
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/1000} DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/1000/bus}
fail() { echo "FAIL: $1"; exit 1; }
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || fail "/data is not btrfs"
[ "$(cat /etc/sudoers.d/ai-os-admin 2>/dev/null)" = 'ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin' ] || fail "sudoers is not the one line"
id ai-sandbox >/dev/null 2>&1 || fail "no ai-sandbox user"
systemctl --user is-active ai-os-engine.service >/dev/null || fail "engine unit not active"
[ -S "$XDG_RUNTIME_DIR/ai-os.sock" ] || fail "no engine socket"
curl -fsS --max-time 5 http://10.0.2.2:11434/api/tags | grep -q qwen3.5 || fail "the host model does not answer at 10.0.2.2:11434"
busctl --user call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress >/dev/null 2>&1 || fail "no accessibility bus on the session bus"
[ -f /etc/xdg/autostart/org.aios.Rail.desktop ] || fail "no rail autostart entry"
echo "desktop edition ready"
