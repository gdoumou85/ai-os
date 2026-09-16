#!/usr/bin/env bash
# Machine test for ai-os-admin. Run as user ai inside the ai-os distro after trial/setup-admin.sh.
set -u
A="sudo -n /usr/local/libexec/ai-os-admin"
fail=0; ok(){ echo "PASS $1"; }; bad(){ echo "FAIL $1"; fail=1; }
refuses(){ if $A "${@:2}" </dev/null >/dev/null 2>/tmp/err && false; then bad "$1 (was allowed)"; elif grep -q '^refused:' /tmp/err; then ok "$1"; else bad "$1 (no 'refused:' line: $(cat /tmp/err))"; fi; }
refuses "unknown verb"            format-disk
refuses "essential remove"        remove -- bash
refuses "runtime remove"          remove -- systemd
refuses "option as package"       install -- -o APT::Update::Pre-Invoke::=/bin/echo
refuses "bad package name"        install -- 'cowsay;id'
refuses "unit path as service"    service /tmp/x.service enable
refuses "protected service"       service ollama disable
refuses "path outside roots"      write-file /usr/bin/evil
refuses "sudoers write"           write-file /etc/sudoers.d/x
refuses "shadow read"             read-file /etc/shadow
refuses "make-dir outside roots"  make-dir /opt/x
refuses "sandbox cwd outside"     sandbox-run --net=none --cwd=/etc -- id
$A pkg-list | grep -qx bash && ok "pkg-list has bash" || bad "pkg-list"
$A service ollama state | grep -q '^enabled active' && ok "service state" || bad "service state: $($A service ollama state)"
echo hello | $A write-file /data/housekeeping/t1c.txt && [ "$($A read-file /data/housekeeping/t1c.txt)" = hello ] && ok "write/read file" || bad "write/read file"
$A remove-file /data/housekeeping/t1c.txt && [ ! -e /data/housekeeping/t1c.txt ] && ok "remove-file" || bad "remove-file"
$A make-dir /data/t1c-dir && [ "$(stat -c '%U:%G %a' /data/t1c-dir)" = "ai:ai-sandbox 2770" ] && ok "make-dir owner" || bad "make-dir owner: $(stat -c '%U:%G %a' /data/t1c-dir)"
$A remove-dir /data/t1c-dir && [ ! -e /data/t1c-dir ] && ok "remove-dir" || bad "remove-dir"
[ "$($A sandbox-run --net=none --cwd=/data/housekeeping -- id -un)" = ai-sandbox ] && ok "sandbox-run uid" || bad "sandbox-run uid"
$A sandbox-run --net=none --cwd=/data/housekeeping -- getent hosts example.com >/dev/null 2>&1 && bad "net=none leaks" || ok "net=none blocked"
[ "$($A sandbox-run --net=none --cwd=/data/housekeeping -- printf '%s' '${HOME}')" = '${HOME}' ] && ok "dollar survives" || bad "dollar expanded"
resolver=$(awk '/^nameserver/{print $2; exit}' /etc/resolv.conf); pypi=$(getent ahostsv4 pypi.org | awk '{print $1}' | sort -u | paste -sd,)
code=$($A sandbox-run --net=$resolver,$pypi --cwd=/data/housekeeping -- curl -sS -m 20 -o /dev/null -w '%{http_code}' https://pypi.org/simple/ 2>/dev/null)
[ "$code" = 200 ] && ok "allowlist reaches pypi" || bad "allowlist pypi code=$code"
$A sandbox-run --net=$resolver,$pypi --cwd=/data/housekeeping -- curl -sS -m 8 -o /dev/null https://example.com 2>/dev/null && bad "allowlist leaks to example.com" || ok "allowlist blocks example.com"
$A sandbox-run --net=none --cwd=/data/housekeeping -- python3 -m venv /data/housekeeping/.venv-t1c && ok "venv works in jail" || bad "venv in jail"; rm -rf /data/housekeeping/.venv-t1c
exit $fail
