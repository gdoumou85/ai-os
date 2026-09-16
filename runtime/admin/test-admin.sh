#!/usr/bin/env bash
# Machine test for ai-os-admin. Run as user ai inside the ai-os distro after trial/setup-admin.sh.
set -u
A="sudo -n /usr/local/libexec/ai-os-admin"
fail=0; ok(){ echo "PASS $1"; }; bad(){ echo "FAIL $1"; fail=1; }
refuses(){ local d=$1 st=0; shift; $A "$@" </dev/null >/dev/null 2>/tmp/err || st=$?
  if [ "$st" -eq 0 ]; then bad "$d (was allowed)"
  elif grep -q '^refused:' /tmp/err; then ok "$d"
  else bad "$d (no 'refused:' line, exit $st: $(cat /tmp/err))"; fi; }
refuses "unknown verb"            format-disk
refuses "essential remove"        remove -- bash
refuses "runtime remove"          remove -- systemd
refuses "option as package"       install -- -o APT::Update::Pre-Invoke::=/bin/echo
refuses "bad package name"        install -- 'cowsay;id'
refuses "apt suffix as package"   install -- sudo-
refuses "trailing dash"           install -- g++-
refuses "unit path as service"    service /tmp/x.service enable
refuses "protected service"       service ollama disable
refuses "protected unit suffix"   service ollama.service disable
refuses "non-service unit"        service dbus.socket disable
refuses "deb as package"          install -- cowsay.deb
refuses "path outside roots"      write-file /usr/bin/evil
refuses "sudoers write"           write-file /etc/sudoers.d/x
refuses "shadow read"             read-file /etc/shadow
refuses "read outside roots"      read-file /root/.bashrc
refuses "empty path"              write-file ""
refuses "relative path"           write-file relative/x
refuses "make-dir outside roots"  make-dir /opt/x
refuses "sandbox cwd outside"     sandbox-run --net=none --cwd=/etc -- id
ln -sf /etc/hostname /data/housekeeping/lnk-t1c; host_before=$(cat /etc/hostname)
refuses "symlink write"           write-file /data/housekeeping/lnk-t1c
refuses "symlink read"            read-file /data/housekeeping/lnk-t1c
[ "$(cat /etc/hostname)" = "$host_before" ] && ok "symlink target untouched" || bad "symlink target untouched"
rm -f /data/housekeeping/lnk-t1c
# The caller owns the folders under /data, so it can plant a link to an allowed root INSIDE one
# and reach through it: `realpath -m` alone would hand back a path under /etc and let it pass.
ln -sfn /etc /data/housekeeping/dir-t1c
refuses "symlink through a dir (write)" write-file /data/housekeeping/dir-t1c/ai-os-through
refuses "symlink through a dir (read)"  read-file /data/housekeeping/dir-t1c/hostname
[ ! -e /etc/ai-os-through ] && ok "nothing landed in /etc" || bad "a write reached /etc"
rm -f /data/housekeeping/dir-t1c
mkfifo /data/housekeeping/fifo-t1c
refuses "fifo read"               read-file /data/housekeeping/fifo-t1c
rm -f /data/housekeeping/fifo-t1c
$A pkg-list | grep -qx bash && ok "pkg-list has bash" || bad "pkg-list"
$A service ollama state | grep -q '^enabled active' && ok "service state" || bad "service state: $($A service ollama state)"
echo hello | $A write-file /data/housekeeping/t1c.txt && [ "$($A read-file /data/housekeeping/t1c.txt)" = hello ] && ok "write/read file" || bad "write/read file"
$A remove-file /data/housekeeping/t1c.txt && [ ! -e /data/housekeeping/t1c.txt ] && ok "remove-file" || bad "remove-file"
echo one | $A write-file /data/housekeeping/m600-t1c && chmod 600 /data/housekeeping/m600-t1c && echo two | $A write-file /data/housekeeping/m600-t1c
[ "$(stat -c '%a %U:%G' /data/housekeeping/m600-t1c 2>&1)" = "600 ai:ai-sandbox" ] && ok "existing mode kept" || bad "existing mode kept: $(stat -c '%a %U:%G' /data/housekeeping/m600-t1c 2>&1)"
$A remove-file /data/housekeeping/m600-t1c
echo hi | $A write-file /data/housekeeping/nd-t1c/f.txt && [ "$(stat -c '%U:%G %a' /data/housekeeping/nd-t1c)" = "ai:ai-sandbox 2770" ] && ok "new parent dir owner" || bad "new parent dir owner: $(stat -c '%U:%G %a' /data/housekeeping/nd-t1c 2>&1)"
$A remove-file /data/housekeeping/nd-t1c/f.txt; $A remove-dir /data/housekeeping/nd-t1c
$A make-dir /data/t1c-dir && [ "$(stat -c '%U:%G %a' /data/t1c-dir)" = "ai:ai-sandbox 2770" ] && ok "make-dir owner" || bad "make-dir owner: $(stat -c '%U:%G %a' /data/t1c-dir)"
$A remove-dir /data/t1c-dir && [ ! -e /data/t1c-dir ] && ok "remove-dir" || bad "remove-dir"
# make-dir on a folder that is already there changes nothing — it must never re-own or re-mode
# a directory it did not create.
$A make-dir /data/housekeeping/keep-t1c && chmod 700 /data/housekeeping/keep-t1c
kept=$(stat -c '%a %U:%G' /data/housekeeping/keep-t1c); $A make-dir /data/housekeeping/keep-t1c
[ "$(stat -c '%a %U:%G' /data/housekeeping/keep-t1c)" = "$kept" ] && ok "make-dir leaves an existing dir alone ($kept)" || bad "make-dir re-owned an existing dir: $kept -> $(stat -c '%a %U:%G' /data/housekeeping/keep-t1c)"
$A remove-dir /data/housekeeping/keep-t1c
# A new directory OUTSIDE the AI roots stays root's: /etc/foo created for an approved write must
# not become a folder the sandbox user can then fill on its own.
echo x | $A write-file /etc/ai-os-t1c/x >/dev/null 2>&1
[ "$(stat -c %U /etc/ai-os-t1c 2>&1)" = root ] && ok "new /etc dir stays root's" || bad "new /etc dir owner: $(stat -c %U:%G /etc/ai-os-t1c 2>&1)"
# The file goes; the empty directory stays, on purpose — nothing in the menu removes a directory
# outside /data and /home/ai, which is the same boundary this check is about.
$A remove-file /etc/ai-os-t1c/x
[ "$($A sandbox-run --net=none --cwd=/data/housekeeping -- id -un)" = ai-sandbox ] && ok "sandbox-run uid" || bad "sandbox-run uid"
$A sandbox-run --net=none --cwd=/data/housekeeping -- getent hosts example.com >/dev/null 2>&1 && bad "net=none leaks" || ok "net=none blocked"
[ "$($A sandbox-run --net=none --cwd=/data/housekeeping -- printf '%s' '${HOME}')" = '${HOME}' ] && ok "dollar survives" || bad "dollar expanded"
case "$($A sandbox-run --net=none --cwd=/data/housekeeping -- printenv PATH)" in *:/usr/games) ok "games on PATH";; *) bad "games not on PATH: $($A sandbox-run --net=none --cwd=/data/housekeeping -- printenv PATH)";; esac
resolver=$(awk '/^nameserver/{print $2; exit}' /etc/resolv.conf); pypi=$(getent ahostsv4 pypi.org | awk '{print $1}' | sort -u | paste -sd,)
code=$($A sandbox-run --net=$resolver,$pypi --cwd=/data/housekeeping -- curl -sS -m 20 -o /dev/null -w '%{http_code}' https://pypi.org/simple/ 2>/dev/null)
[ "$code" = 200 ] && ok "allowlist reaches pypi" || bad "allowlist pypi code=$code"
$A sandbox-run --net=$resolver,$pypi --cwd=/data/housekeeping -- curl -sS -m 8 -o /dev/null https://example.com 2>/dev/null && bad "allowlist leaks to example.com" || ok "allowlist blocks example.com"
$A sandbox-run --net=none --cwd=/data/housekeeping -- python3 -m venv /data/housekeeping/.venv-t1c && ok "venv works in jail" || bad "venv in jail"; rm -rf /data/housekeeping/.venv-t1c
rm -f /tmp/err
exit $fail
