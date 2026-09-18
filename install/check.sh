#!/usr/bin/env bash
# Is the AI OS installed and alive for this user? Prints "AI OS ready" or the first failure.
#   bash check.sh [--no-session]     --no-session skips what needs a logged-in desktop
session=1
case "${1:-}" in
  "") ;;
  --no-session) session=0 ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac
owner=$(id -un); export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
fail() { echo "FAIL: $1"; exit 1; }
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || fail "/data is not btrfs"
id ai-sandbox >/dev/null 2>&1 || fail "no ai-sandbox account"
# Both halves are needed: the call below also succeeds on a sudo timestamp cached by the installer
# a minute ago, and the grant is what has to outlive it. The file is 0440 root, so only its
# presence is asked about, never its contents.
[ -f /etc/sudoers.d/ai-os-admin ] || fail "the root helper has no permission line in /etc/sudoers.d"
sudo -n /usr/local/libexec/ai-os-admin service ai-os-none state >/dev/null 2>&1 || fail "the root helper does not answer $owner through sudo"
[ "$(stat -c %U:%G /data/projects)" = "$owner:ai-sandbox" ] || fail "/data/projects is not $owner:ai-sandbox"
systemctl --user is-active ai-os-engine.service >/dev/null || fail "the engine service is not running (journalctl --user -u ai-os-engine)"
[ -S "$XDG_RUNTIME_DIR/ai-os.sock" ] || fail "the engine's socket is missing"
url=$(systemctl --user show ai-os-engine.service -p Environment | tr ' ' '\n' | sed -n 's/^AI_OS_MODEL_URL=//p'); url=${url:-http://127.0.0.1:11434}
curl -fsS --max-time 5 "$url/api/tags" >/dev/null || fail "the model runner does not answer at $url"
[ -f /etc/xdg/autostart/org.aios.Rail.desktop ] || fail "the chat window is not set to open with the session"
if [ $session -eq 1 ]; then
  busctl --user call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress >/dev/null 2>&1 || fail "no accessibility bus in this session"
fi
echo "AI OS ready"
