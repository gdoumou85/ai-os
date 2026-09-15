#!/usr/bin/env bash
# Create the unprivileged Linux user the sandbox worker runs commands as.
# Run once as root inside the distro. The user owns nothing outside job workspaces.
set -e
id ai-sandbox >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin ai-sandbox
install -d -o ai-sandbox -g ai-sandbox -m 0770 /data/jobs
# The orchestrator (user "ai") creates job workspaces under /data/jobs; without
# group membership it can't even mkdir there (0770, no "other" bits). Job
# subdirs it creates keep their normal 0755-ish perms, which already give
# ai-sandbox (as "other") the r-x it needs to chdir/read inside them.
usermod -aG ai-sandbox ai 2>/dev/null || true
# SandboxWorker drives the privileged system systemd via `sudo -n systemd-run` (the
# unprivileged `ai` user's own `systemd --user` manager can't grant --uid=/PrivateNetwork=).
# Grant exactly that, nothing else, so a fresh distro works from this script alone.
install -m 0440 /dev/stdin /etc/sudoers.d/ai-sandbox-run <<< 'ai ALL=(root) NOPASSWD: /usr/bin/systemd-run'
visudo -cf /etc/sudoers.d/ai-sandbox-run
echo "ai-sandbox ready"
