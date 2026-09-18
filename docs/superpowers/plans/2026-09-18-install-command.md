# The Install Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A person installs Ubuntu 26.04 Desktop themselves, under any user name, and one command adds the AI OS to it.

**Architecture:** The product stops naming the user `ai`: the root wrapper takes its owner from `SUDO_USER`, the prompt names the engine's own home. `install/install.sh` does as the owner what the workshop scripts did for `ai`. A tarball built in WSL carries the installer, binaries, wrapper and desktop entry; a fresh WSL distro with a differently named user proves it before his VM does.

**Tech Stack:** bash, sudo/sudoers, systemd user units, btrfs loop file, Rust (one prompt line), WSL for the proof, GitHub releases for distribution.

**Spec:** `docs/superpowers/specs/2026-09-18-desktop-edition-design.md` §9 (the rest of that document is withdrawn by §9 except §4, §6's acceptance and §7).

## Global Constraints

- Nothing in the product names the user `ai` or `/home/ai` in logic. Comments and test examples may.
- The sandbox account stays `ai-sandbox`. `/data` layout unchanged: `/data/live` subvolume, `/data/snapshots`, `/data/projects`, `/data/jobs`, `/data/housekeeping`.
- The only privilege: `<owner> ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin`, validated by `visudo -cf` before it is moved into `/etc/sudoers.d/ai-os-admin`. The installer never removes the owner from any group.
- The wrapper refuses before any verb when `SUDO_USER` is empty or `root`, or the owner's home is empty, `/`, or not a directory.
- In the WSL workshop the owner is still `ai`, so everything there behaves byte-for-byte as before: the prompt text, `runtime/admin/test-admin.sh`'s expectations, the live acceptances.
- All text files LF (`*.cmd` excepted). Tests run inside WSL: `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p <crate>` from the PowerShell tool; anything with `$` on a `wsl` command line goes in a script file. `pkill -x` fails on names over 15 characters.
- `install/` is product. Workshop-only helpers stay under `trial/` and say `WORKSHOP ONLY`.
- Never push, tag, merge, create a repo or a release without his word. Commit each task.

---

### Task 1: The owner is whoever runs it

**Files:**
- Modify: `runtime/admin/ai-os-admin`
- Modify: `runtime/admin/test-admin.sh`
- Modify: `runtime/core/src/prompt.rs:116` (the housekeeping header) and its tests module
- Run (root, workshop): `trial/setup-admin.sh` to install the changed wrapper before testing

**Interfaces:**
- Produces: a wrapper that works for any owner reached through the sudoers line; `prompt::job_turn`'s housekeeping header naming `$HOME` instead of `/home/ai`.

- [ ] **Step 1: A failing check for the refusal.** Append to `runtime/admin/test-admin.sh`, before its final summary/exit lines:

```bash
# The owner comes from sudo, never from the caller (desktop design §9.1). Run unprivileged and
# without sudo, the wrapper must refuse before it reaches any verb.
out=$(env -u SUDO_USER bash /usr/local/libexec/ai-os-admin pkg-list 2>&1 >/dev/null); st=$?
{ [ $st -eq 3 ] && echo "$out" | grep -q '^refused: run through sudo'; } && ok "no SUDO_USER refused" || bad "no SUDO_USER: exit $st: $out"
out=$(SUDO_USER=root bash /usr/local/libexec/ai-os-admin pkg-list 2>&1 >/dev/null); st=$?
{ [ $st -eq 3 ] && echo "$out" | grep -q '^refused: run through sudo'; } && ok "root as owner refused" || bad "root as owner: exit $st: $out"
out=$(SUDO_USER=no-such-user-t1 bash /usr/local/libexec/ai-os-admin pkg-list 2>&1 >/dev/null); st=$?
{ [ $st -eq 3 ] && echo "$out" | grep -q '^refused: owner has no home'; } && ok "unknown owner refused" || bad "unknown owner: exit $st: $out"
```

Also in that file: line 2's comment becomes `# Machine test for ai-os-admin. Run as the owner (the workshop's is ai) after the wrapper is installed.`; every expected owner string `ai:ai-sandbox` becomes `$USER:ai-sandbox` (lines 50, 52, 54 today — keep the quoting valid: `"600 $USER:ai-sandbox"`, `"$USER:ai-sandbox 2770"`).

- [ ] **Step 2: See it fail.** Install nothing yet; run the suite in the workshop: `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime/admin/test-admin.sh`. Expected: the three new lines FAIL (today's wrapper runs `pkg-list` happily), everything else PASS.

- [ ] **Step 3: The wrapper.** In `runtime/admin/ai-os-admin`:
  - line 2 comment: `# ai-os-admin: the ONLY thing the owner's engine may run as root. Fixed menu, validated arguments, no shell strings.`
  - directly after the `refuse(){...}` line add:

```bash
# The owner is whoever sudo says ran this. The sudoers line grants exactly one user and sudo sets
# SUDO_USER itself, so the caller cannot name someone else (desktop design §9.1).
owner=${SUDO_USER:-}
{ [ -n "$owner" ] && [ "$owner" != root ]; } || refuse "run through sudo by the owner"
home=$(getent passwd "$owner" | cut -d: -f6) || true
{ [ -n "$home" ] && [ "$home" != / ] && [ -d "$home" ]; } || refuse "owner has no home: $owner"
ogroup=$(id -gn "$owner")
```

  - every `/home/ai` used as a root argument becomes `"$home"` (the five `under ... /home/ai` calls: `under "$1" /etc /data "$home"`, `under "$1" /data "$home"`, `under "$cwd" /data "$home"`);
  - `own_dir` and `write-file`'s `case` arms: `/home/ai/*)` becomes `"$home"/*)`, `chown ai:ai-sandbox` becomes `chown "$owner":ai-sandbox`, `chown ai:ai` becomes `chown "$owner:$ogroup"`;
  - comments that say `/home/ai` or `` `ai` `` say "the owner's home" / "the owner".
  Check: `grep -n -E '/home/ai|\bai:' runtime/admin/ai-os-admin` prints nothing.

- [ ] **Step 4: The prompt.** In `runtime/core/src/prompt.rs` the housekeeping header (line 116) contains the literal `/home/ai`. Build that header with the engine's home instead:

```rust
/// The owner's home as the engine sees it; the wrapper accepts exactly this root (design §9.1).
fn home() -> String { std::env::var("HOME").unwrap_or_else(|_| "/home/ai".into()) }
```

  and in the header string replace `/home/ai` with `{}` via `format!` (the branch currently returns a `&str`/`String` pair — make the header a `String` in both branches if it is not already). Add to the tests module:

```rust
    /// The housekeeping header names the home of whoever runs the engine, not a fixed user.
    #[test]
    fn the_housekeeping_header_names_the_owners_home() {
        let h = home();
        assert!(!h.is_empty());
        let mut job = Job::new("j", "tidy"); job.housekeeping = true;
        let p = job_turn(&[], &job, None, None);
        let text = format!("{}{}", p.system, p.user);
        assert!(text.contains(&format!("under /data and {h}")), "{text}");
    }
```

  Adapt the constructor and the `Prompt` field names to what the file really has (read `job_turn`'s existing tests in the same module and build the job the way they do); the assertion — the header contains `under /data and <HOME>` — is the requirement. In the workshop `HOME=/home/ai`, so every existing prompt test must still pass unchanged.

- [ ] **Step 5: Install and run everything.** As root in the workshop: `wsl -d ai-os -u root -- bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-admin.sh`; then the wrapper suite as `ai` (Step 2's command): all PASS including the three new lines; then `trial/run-tests.sh test -p aios-core` and `test -p executor`: green.

- [ ] **Step 6: Commit.**

```bash
git add runtime/admin/ai-os-admin runtime/admin/test-admin.sh runtime/core/src/prompt.rs
git commit -m "feat(admin): the wrapper's owner is whoever sudo says; the prompt names the engine's home"
```

---

### Task 2: The installer

**Files:**
- Create: `install/install.sh`, `install/check.sh`, `install/ai-os-engine.service.in`
- Delete: `trial/vm/` (withdrawn by design §9; `git rm -r trial/vm`)

**Interfaces:**
- Consumes: next to `install.sh` at run time: `bin/ai-os-engine`, `bin/ai-os-chat`, `bin/ai-os-rail`, `ai-os-admin`, `org.aios.Rail.desktop`, `check.sh`, `ai-os-engine.service.in` (Task 3's tarball lays them out so; from a checkout, Task 3's script stages the same layout).
- Produces: `install.sh [--model-url URL] [--model NAME]`; `check.sh` printing `AI OS ready` or `FAIL: <first failure>` (exit 1); option `--no-session` on `check.sh` skipping the two checks that need a logged-in desktop (accessibility bus, engine socket under a session-less linger is still checked).

- [ ] **Step 1: `install/ai-os-engine.service.in`:**

```ini
[Unit]
Description=AI OS engine

[Service]
ExecStart=/usr/local/bin/ai-os-engine
Environment=AI_OS_DB=/data/ai-os.db
Environment=AI_OS_MODEL=@MODEL@
Environment=AI_OS_PROJECTS=/data/projects
Environment=AI_OS_DISPLAY_INVISIBLE=wayland-0
Environment=AI_OS_DISPLAY_VISIBLE=wayland-0
@MODEL_URL_LINE@
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

- [ ] **Step 2: `install/install.sh`:**

```bash
#!/usr/bin/env bash
# Adds the AI OS to an Ubuntu 26.04 desktop, for the user who runs it (desktop design §9.2).
#   bash install.sh [--model-url http://host:11434] [--model qwen3.5:9b]
# Run it as yourself, not as root: it asks sudo for the root steps. Safe to run again.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
model=qwen3.5:9b; model_url=""
while [ $# -gt 0 ]; do case "$1" in
  --model-url) model_url=${2:?--model-url needs a value}; shift 2 ;;
  --model)     model=${2:?--model needs a value}; shift 2 ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac; done
[ "$(id -u)" -ne 0 ] || { echo "run this as your own user, not as root" >&2; exit 1; }
owner=$(id -un); uid=$(id -u); ogroup=$(id -gn)
[ "$(uname -m)" = x86_64 ] || { echo "only x86_64 is built" >&2; exit 1; }
. /etc/os-release; [ "${VERSION_ID:-}" = 26.04 ] || echo "warning: built and tested on Ubuntu 26.04, this is ${PRETTY_NAME:-unknown}" >&2
for f in bin/ai-os-engine bin/ai-os-chat bin/ai-os-rail ai-os-admin org.aios.Rail.desktop check.sh ai-os-engine.service.in; do
  [ -f "$here/$f" ] || { echo "missing next to install.sh: $f" >&2; exit 1; }
done
echo "== installing the AI OS for $owner (sudo will ask for your password)"
sudo -v

echo "== packages"
sudo env DEBIAN_FRONTEND=noninteractive apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y btrfs-progs libgtk-4-1 curl gnome-text-editor \
  python3-venv npm cargo

echo "== /data (btrfs, where the AI works and what undo covers)"
if ! mountpoint -q /data; then
  sudo install -d /var/lib/ai-os /data
  if [ ! -f /var/lib/ai-os/data.img ]; then
    free_g=$(df -BG --output=avail /var/lib/ai-os | tail -1 | tr -dc 0-9)
    size_g=$(( free_g / 2 )); [ "$size_g" -le 50 ] || size_g=50
    [ "$size_g" -ge 5 ] || { echo "less than 10 GB free: not enough for /data" >&2; exit 1; }
    sudo truncate -s "${size_g}G" /var/lib/ai-os/data.img     # sparse: takes space as it fills
    sudo mkfs.btrfs -q -L ai-os-data /var/lib/ai-os/data.img
  fi
  grep -q ' /data ' /etc/fstab || echo '/var/lib/ai-os/data.img /data btrfs loop,noatime,compress=zstd 0 0' | sudo tee -a /etc/fstab >/dev/null
  sudo mount /data
fi
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || { echo "/data is mounted but is not btrfs" >&2; exit 1; }
[ -d /data/live ] || sudo btrfs subvolume create /data/live >/dev/null
sudo chown "$owner:$ogroup" /data/live

echo "== the sandbox account"
id ai-sandbox >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin ai-sandbox
sudo usermod -aG ai-sandbox "$owner"
sudo install -d -o ai-sandbox -g ai-sandbox -m 0770 /data/jobs
sudo install -d -o "$owner" -g ai-sandbox -m 2770 /data/projects /data/snapshots /data/housekeeping

echo "== the root helper and its one permission"
sudo install -d -m 0755 /usr/local/libexec
sudo install -m 0755 -o root -g root "$here/ai-os-admin" /usr/local/libexec/ai-os-admin
echo "$owner ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin" | sudo tee /etc/sudoers.d/ai-os-admin.tmp >/dev/null
sudo chmod 0440 /etc/sudoers.d/ai-os-admin.tmp
sudo visudo -cf /etc/sudoers.d/ai-os-admin.tmp >/dev/null
sudo mv -f /etc/sudoers.d/ai-os-admin.tmp /etc/sudoers.d/ai-os-admin

echo "== programs"
for b in ai-os-engine ai-os-chat ai-os-rail; do sudo install -m 0755 -o root -g root "$here/bin/$b" /usr/local/bin/$b; done
sudo install -m 0644 "$here/org.aios.Rail.desktop" /usr/share/applications/org.aios.Rail.desktop
sudo install -d /etc/xdg/autostart
sudo install -m 0644 "$here/org.aios.Rail.desktop" /etc/xdg/autostart/org.aios.Rail.desktop

echo "== the model"
if [ -n "$model_url" ]; then
  if curl -fsS --max-time 5 "$model_url/api/tags" | grep -q "\"$model\""; then echo "the runner at $model_url has $model"
  else echo "warning: $model_url did not answer with $model — the AI will say so until it does" >&2; fi
  url_line="Environment=AI_OS_MODEL_URL=$model_url"
else
  # A native install: the runner lives on this machine. Untested until a machine with a GPU runs it
  # (desktop design §9.2). The two settings are Phase 0's measurement: they are what keeps an 8k
  # context fully on an 8 GB GPU.
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y zstd     # Ollama's installer unpacks with it
  command -v ollama >/dev/null || curl -fsSL https://ollama.com/install.sh | sh
  sudo install -d /etc/systemd/system/ollama.service.d
  printf '[Service]
Environment=OLLAMA_FLASH_ATTENTION=1
Environment=OLLAMA_KV_CACHE_TYPE=q8_0
' | sudo tee /etc/systemd/system/ollama.service.d/ai-os.conf >/dev/null
  sudo systemctl daemon-reload; sudo systemctl enable --now ollama; sudo systemctl restart ollama
  for _ in $(seq 30); do curl -fsS --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break; sleep 1; done
  ollama pull "$model"      # several GB, once
  command -v nvidia-smi >/dev/null || lspci 2>/dev/null | grep -qiE 'vga.*(amd|radeon)'     || echo "note: no NVIDIA or AMD graphics driver found — the model will run on the processor, slowly. On NVIDIA: sudo ubuntu-drivers install, then restart." >&2
  url_line=""
fi

echo "== the engine, as your service"
install -d "$HOME/.config/systemd/user"
sed -e "s|@MODEL@|$model|" -e "s|@MODEL_URL_LINE@|$url_line|" "$here/ai-os-engine.service.in" > "$HOME/.config/systemd/user/ai-os-engine.service"
sudo loginctl enable-linger "$owner"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$uid}
systemctl --user daemon-reload
systemctl --user enable ai-os-engine.service
systemctl --user restart ai-os-engine.service
gsettings set org.gnome.desktop.interface toolkit-accessibility true 2>/dev/null || true
xdg-mime default org.gnome.TextEditor.desktop text/x-python text/markdown text/plain text/x-shellscript application/json 2>/dev/null || true

sleep 2
bash "$here/check.sh" ${AI_OS_CHECK_ARGS:-}
# The engine runs under your user manager, which lingers across logouts and so keeps its old groups:
# only a restart hands it the ai-sandbox membership it needs to make job folders.
echo "Restart the computer once. After that the chat window opens whenever you log in."
```

- [ ] **Step 3: `install/check.sh`:**

```bash
#!/usr/bin/env bash
# Is the AI OS installed and alive for this user? Prints "AI OS ready" or the first failure.
#   bash check.sh [--no-session]     --no-session skips what needs a logged-in desktop
session=1; [ "${1:-}" = --no-session ] && session=0
owner=$(id -un); export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
fail() { echo "FAIL: $1"; exit 1; }
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || fail "/data is not btrfs"
id ai-sandbox >/dev/null 2>&1 || fail "no ai-sandbox account"
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
```

- [ ] **Step 4: Withdraw the VM scaffolding:** `git rm -r -q trial/vm`.

- [ ] **Step 5: Check.** `bash -n` on both scripts inside WSL (one call each, PowerShell tool); `grep -n -E '\bai\b[^-]|/home/ai|1000' install/install.sh install/check.sh` prints nothing that names the user `ai`, its home or uid 1000; `grep -c $'\r'` = 0 on the three new files.

- [ ] **Step 6: Commit.**

```bash
git add -A install trial/vm
git commit -m "feat(install): one command adds the AI OS for whoever runs it; the VM scaffolding is withdrawn"
```

---

### Task 3: The tarball, the one-liner, and the proof on a fresh machine

**Files:**
- Create: `install/make-release.sh`, `install/get.sh`, `trial/test-install.ps1`, `trial/test-install-guest.sh`
- Modify: `.gitignore` (add `dist/`)
- Modify: `docs/superpowers/specs/2026-09-18-desktop-edition-design.md` (Results under §9.4), `docs/superpowers/specs/2026-09-15-ai-os-design.md` (status block at the end)

**Interfaces:**
- Consumes: Task 2's three files; `runtime/admin/ai-os-admin`, `runtime/rail/org.aios.Rail.desktop`, the WSL release build.
- Produces: `dist/ai-os-linux-amd64.tar.gz` whose top folder `ai-os/` holds `install.sh check.sh ai-os-engine.service.in ai-os-admin org.aios.Rail.desktop VERSION bin/{ai-os-engine,ai-os-chat,ai-os-rail}`; `get.sh` with the repo slug in one variable, `REPO=${AI_OS_REPO_SLUG:-gdoumou85/ai-os}`.

- [ ] **Step 1: `install/make-release.sh`** (run in the WSL workshop from the repo root):

```bash
#!/usr/bin/env bash
# Builds dist/ai-os-linux-amd64.tar.gz (desktop design §9.3). Run inside the Ubuntu 26.04 workshop:
# the binaries promise nothing on an older release.
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
(cd "$root/runtime" && cargo build --release)
stage=$(mktemp -d); trap 'rm -rf "$stage"' EXIT
install -d "$stage/ai-os/bin"
for b in ai-os-engine ai-os-chat ai-os-rail; do install -m 0755 "$root/runtime/target/release/$b" "$stage/ai-os/bin/$b"; done
install -m 0755 "$root/install/install.sh" "$root/install/check.sh" "$root/runtime/admin/ai-os-admin" "$stage/ai-os/"
install -m 0644 "$root/install/ai-os-engine.service.in" "$root/runtime/rail/org.aios.Rail.desktop" "$stage/ai-os/"
git -C "$root" describe --always --dirty > "$stage/ai-os/VERSION"
sed -i 's/\r$//' "$stage/ai-os/"*.sh "$stage/ai-os/ai-os-admin" "$stage/ai-os/"*.in "$stage/ai-os/"*.desktop
install -d "$root/dist"
tar -C "$stage" --owner=0 --group=0 -czf "$root/dist/ai-os-linux-amd64.tar.gz" ai-os
echo "built dist/ai-os-linux-amd64.tar.gz ($(cat "$stage/ai-os/VERSION"))"
```

- [ ] **Step 2: `install/get.sh`** (the line a user runs is `curl -fsSL https://raw.githubusercontent.com/<slug>/master/install/get.sh | bash -s -- [options]`):

```bash
#!/usr/bin/env bash
# Downloads the latest AI OS release and runs its installer with the same options.
set -euo pipefail
REPO=${AI_OS_REPO_SLUG:-gdoumou85/ai-os}
dir=$(mktemp -d)
curl -fSL "https://github.com/$REPO/releases/latest/download/ai-os-linux-amd64.tar.gz" | tar -xz -C "$dir"
bash "$dir/ai-os/install.sh" "$@"
```

- [ ] **Step 3: Build it.** `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/install/make-release.sh`; expected last line `built dist/ai-os-linux-amd64.tar.gz (...)`; `tar -tzf` lists the eleven entries above. Add `dist/` to `.gitignore`.

- [ ] **Step 4: The proof (design §9.4), `trial/test-install.ps1` + `trial/test-install-guest.sh`, WORKSHOP ONLY.** The PowerShell script: finds the Ubuntu 26.04 WSL root file system under `https://cloud-images.ubuntu.com/wsl/` (list the `resolute` directory, take the `amd64` `.rootfs.tar.gz`/`.wsl` file it offers; cache it in `C:\WSL\cache\`), `wsl --import ai-os-test C:\WSL\ai-os-test <file>`, writes `/etc/wsl.conf` with `[boot] systemd=true` and `[user] default=tester`, creates user `tester` (not uid-pinned, in group `sudo`, with a sudoers drop-in `tester ALL=(ALL) NOPASSWD:ALL` — test machine only, so `install.sh`'s sudo never prompts), `wsl --terminate ai-os-test`, then runs the guest script as `tester`. The guest script: unpacks `/mnt/c/.../dist/ai-os-linux-amd64.tar.gz` into `~/t`, runs `AI_OS_CHECK_ARGS=--no-session bash ~/t/ai-os/install.sh --model-url http://127.0.0.1:9` and expects it to get as far as `check.sh` failing only on the model line (`FAIL: the model runner does not answer at http://127.0.0.1:9`) — a fresh distro has no runner, and that line is after every ownership, sudo, service and socket check; then runs `bash /mnt/c/.../runtime/admin/test-admin.sh` as `tester` and expects no `FAIL` line (the wrapper suite as a user who is not `ai`). The PowerShell script prints both outcomes and ends with `wsl --unregister ai-os-test` whether or not they passed; it never touches the `ai-os` distro.

  The wrapper suite writes under `/data/housekeeping` and `/etc`; on the throwaway distro that is fine. If `test-admin.sh` assumes anything else of the workshop (a package, a folder), install or create it in the guest script rather than weakening the suite.

- [ ] **Step 5: Run the proof** from the PowerShell tool in the background (it downloads ~350 MB once and installs packages: allow 20 minutes): `powershell -NoProfile -ExecutionPolicy Bypass -File trial\test-install.ps1`. Fix what it shows in `install/*` or the wrapper (a wrapper change re-runs Task 1's Step 5), rebuild the tarball, rerun.

- [ ] **Step 6: Record.** Under §9.4 of the desktop design add `### Results` with: the tarball's size and VERSION, the fresh-distro run's outcome line by line (user name used, install reached check, the one expected FAIL, wrapper suite count), and what had to be fixed. Append to the parent spec a block `## The desktop edition (2026-09-18)` saying in five lines what §9 is, that the proof passed, and that his own Ubuntu VM is the acceptance, pending.

- [ ] **Step 7: Commit.**

```bash
git add install/make-release.sh install/get.sh trial/test-install.ps1 trial/test-install-guest.sh .gitignore docs/superpowers/specs
git commit -m "feat(install): the release tarball, the one-line bootstrap, and the proof on a fresh distro"
```

---

### Task 4 (his word needed): the repo and the first release

Not dispatched until he has said public or private. Then: `gh repo create gdoumou85/ai-os --<visibility> --source . --remote origin`, push `master` after the branch is merged by his word, `gh release create v0.1.0 dist/ai-os-linux-amd64.tar.gz`, and the line he runs in his Ubuntu:

```bash
curl -fsSL https://raw.githubusercontent.com/gdoumou85/ai-os/master/install/get.sh | bash -s -- --model-url http://10.0.2.2:11434
```

(private repo: `gh release download` after `gh auth login` in the VM instead of the `curl` line.)

## Self-review

- **Spec coverage:** §9.1 → Task 1; §9.2 → Task 2; §9.3 → Task 3 steps 1–3 and Task 4; §9.4 → Task 3 steps 4–6. §4's `AI_OS_MODEL_URL` and desktop entry are already on the branch.
- **Placeholders:** `<slug>`/`<visibility>` in Task 4 are his decision, named as such; `@MODEL@`/`@MODEL_URL_LINE@` are template tokens `install.sh` fills.
- **Consistency:** tarball layout in Task 3 matches `install.sh`'s file check in Task 2; `check.sh --no-session` is produced in Task 2 and used in Task 3 through `AI_OS_CHECK_ARGS`; the refusal texts `run through sudo` / `owner has no home` match between the wrapper and its tests.
