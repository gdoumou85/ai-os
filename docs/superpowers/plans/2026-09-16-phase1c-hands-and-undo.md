# Phase 1c — Hands and Undo Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the AI an admin hand (install/remove/service/make_dir), registry-only package fetching, per-job undo, and housekeeping jobs, while narrowing the workshop's sudo grant to one fixed-menu wrapper.

**Architecture:** The executor crate gains six typed actions, an `AdminWorker` that shells to a root-owned wrapper `ai-os-admin` (the only sudo grant), an `UndoEntry` carried back on every `Outcome`, and a two-lane `Executor` (sandbox / admin). The core crate gains the `housekeep` front-door move, a `settings` and an `undo` table, `Job.folder`/`housekeeping`/`new_project`, btrfs project snapshots at job start, and a deterministic `undo` word that reverses the last job. Everything the model can say is still one grammar-forced JSON move.

**Tech Stack:** Rust 2021 (workspace at `runtime/`), serde/serde_json (`preserve_order` in core), rusqlite, ureq; bash for the wrapper; systemd-run, apt, btrfs-progs on the `ai-os` WSL distro.

**Spec:** `docs/superpowers/specs/2026-09-16-phase1c-hands-and-undo-design.md` (read it first; §3 table, §5 verb list and §13 are the contract).

## Global Constraints

- The model never writes a shell string that runs with privilege: every admin operation is a typed `Action` variant → coded handler → fixed wrapper verb.
- Every wrapper path is validated with `realpath -m` against its roots (`/etc`, `/data`, `/home/ai` for files; `/data`, `/home/ai` for dirs and cwd); refused files: `/etc/sudoers*`, `/etc/passwd`, `/etc/shadow`, `/etc/group`.
- Package and service names: `^[a-z0-9][a-z0-9+.@_-]*$`, passed after `--`.
- `/etc` writes are NeedsConfirm (never Auto). `/etc` reads are Auto and unprivileged.
- `remove` never purges; refuses `Essential: yes`, `Priority: required`, and `btrfs-progs systemd sudo`. `service disable` refuses `ollama dbus gdm* systemd-* ssh* NetworkManager ai-os*`.
- Sandbox jail property set is unchanged except `InaccessiblePaths=-/mnt/c -/media -/srv` and `$`→`$$` in argv.
- JSON discriminator (`move` / `kind`) is the first key in every schema object.
- All text files LF (`.gitattributes` enforces it). Commit after every task; never push, tag or merge without his word.
- Tests: `cargo test` in `runtime/` for unit tests; machine tests run inside the distro with `AI_OS_SANDBOX_IT=1`. Run machine tests via `wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && AI_OS_SANDBOX_IT=1 cargo test -p executor --test <name>"`; root-only steps via `wsl -d ai-os -u root -- bash -c "..."`. Never edit `/etc/sudoers.d` by hand: only through `trial/setup-admin.sh`.
- Never adjust a test to make it pass; a failing test is a finding.

---

## File map

| File | Responsibility |
|---|---|
| `runtime/admin/ai-os-admin` (new, bash) | the fixed-menu root wrapper (§5) |
| `runtime/admin/test-admin.sh` (new, bash) | machine test of the wrapper's refusals and `sandbox-run` |
| `trial/setup-admin.sh` (new, bash) | installs the wrapper, rewrites sudoers to one line, installs `python3-venv npm cargo`, chowns `/data/snapshots`, creates `/data/housekeeping`, removes the `\r` folders |
| `runtime/executor/src/action.rs` | + `Install`, `Remove`, `Service`, `MakeDir`, `FetchPackages`, `SetSetting`; `valid_name()` |
| `runtime/executor/src/rules.rs` | + classify rules for the six, `/etc` read Auto, `make_dir` roots |
| `runtime/executor/src/undo.rs` (new) | `UndoEntry` enum |
| `runtime/executor/src/worker.rs` | `Outcome.undo`; `Worker::reverse` default; `SandboxWorker` via `sandbox-run`; `$` doubling; `/etc` read; `FetchPackages` |
| `runtime/executor/src/admin.rs` (new) | `AdminWorker` (run + reverse through the wrapper); `escape_dollars`, `registry_hosts`, `resolver_ips` helpers live here too |
| `runtime/executor/src/executor.rs` | two lanes; `lane()`; `log_only()` |
| `runtime/executor/tests/admin_integration.rs` (new) | gated machine tests for the admin worker, fetch, btrfs |
| `runtime/core/src/schema.rs`, `moves.rs`, `prompt.rs` | `housekeep` move, six action shapes, prompt lines, allowed lists |
| `runtime/core/src/job.rs` | `folder`, `housekeeping`, `new_project` |
| `runtime/core/src/store.rs` | `settings`, `undo` tables |
| `runtime/core/src/snapshot.rs` (new) | btrfs subvolume create / snapshot / restore as `ai`, with plain-folder fallback |
| `runtime/core/src/engine.rs` | housekeeping front door, `workspace(&Job)`, gates, `set_setting`, undo rows, `is_undo`, `undo_last` |
| `runtime/core/src/testing.rs` | `ScriptedWorker` writes real files; second recorder for the admin lane |
| `runtime/core/src/main.rs` | wires `AdminWorker` |
| `runtime/core/tests/live_1c.rs` (new) | gated live acceptance (§11) |

---

### Task 1: The wrapper and its setup

**Files:**
- Create: `runtime/admin/ai-os-admin`, `runtime/admin/test-admin.sh`, `trial/setup-admin.sh`

**Interfaces:**
- Produces: the verbs in spec §5, exactly: `install -- <pkg>…`, `remove -- <pkg>…`, `pkg-list`, `service <name> enable|disable|restart|state`, `read-file <path>`, `write-file <path>` (stdin), `remove-file <path>`, `make-dir <path>`, `remove-dir <path>`, `sandbox-run --net=none|<ip,…> --cwd=<dir> [--env=K=V]… -- <argv>…`. Exit 0 on success; non-zero with one line `refused: <reason>` on stderr for every refusal. `service … state` prints `<enabled|disabled|…> <active|inactive|…>` on one line. `pkg-list` prints one package name per line, sorted.

- [ ] **Step 1: Write the machine test first** — `runtime/admin/test-admin.sh` (bash, run as `ai` inside the distro after setup):

```bash
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
```

- [ ] **Step 2: Write the wrapper** — `runtime/admin/ai-os-admin`:

```bash
#!/usr/bin/env bash
# ai-os-admin: the ONLY thing user `ai` may run as root. Fixed menu, validated arguments, no shell strings.
# Workshop transport for Phase 1c; replaced by the privileged executor daemon at packaging.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
refuse(){ echo "refused: $*" >&2; exit 3; }
NAME_RE='^[a-z0-9][a-z0-9+.@_-]*$'
names(){ [ $# -gt 0 ] || refuse "no names"; for n in "$@"; do [[ "$n" =~ $NAME_RE ]] || refuse "bad name: $n"; done; }
# realpath -m: resolves symlinks in the existing prefix, no need for the leaf to exist
under(){ local p; p=$(realpath -m -- "$1"); for r in "${@:2}"; do [[ "$p" == "$r" || "$p" == "$r/"* ]] && { echo "$p"; return; }; done; refuse "path outside allowed roots: $1"; }
not_refused_file(){ case "$1" in /etc/sudoers|/etc/sudoers.d|/etc/sudoers.d/*|/etc/passwd|/etc/shadow|/etc/group|/etc/gshadow) refuse "protected file: $1";; esac; }
verb=${1:-}; shift || true
case "$verb" in
  install) [ "${1:-}" = -- ] || refuse "expected --"; shift; names "$@"
    apt-get install -y -o DPkg::Lock::Timeout=60 -- "$@" ;;
  remove)  [ "${1:-}" = -- ] || refuse "expected --"; shift; names "$@"
    for p in "$@"; do
      case "$p" in btrfs-progs|systemd|sudo) refuse "runtime package: $p";; esac
      if dpkg-query -W -f '${Essential} ${Priority}\n' -- "$p" 2>/dev/null | grep -Eq '^yes|required$'; then refuse "essential package: $p"; fi
    done
    apt-get remove -y -o DPkg::Lock::Timeout=60 -- "$@" ;;
  pkg-list) dpkg-query -W -f '${db:Status-Abbrev} ${Package}\n' | awk '$1=="ii"{print $2}' | sort ;;
  service) [ $# -eq 2 ] || refuse "service <name> <enable|disable|restart|state>"; names "$1"
    case "$2" in
      enable)  systemctl enable --now -- "$1" ;;
      disable) case "$1" in ollama|dbus|gdm*|systemd-*|ssh*|NetworkManager|ai-os*) refuse "protected service: $1";; esac
               systemctl disable --now -- "$1" ;;
      restart) systemctl restart -- "$1" ;;
      state)   e=$(systemctl is-enabled -- "$1" 2>/dev/null || true); a=$(systemctl is-active -- "$1" 2>/dev/null || true); echo "${e:-unknown} ${a:-unknown}" ;;
      *) refuse "unknown service action: $2" ;;
    esac ;;
  read-file)  [ $# -eq 1 ] || refuse "read-file <path>"; not_refused_file "$(realpath -m -- "$1")"; cat -- "$1" ;;
  write-file) [ $# -eq 1 ] || refuse "write-file <path>"; p=$(under "$1" /etc /data /home/ai); not_refused_file "$p"
    install -d -- "$(dirname "$p")"; cat > "$p.ai-os-tmp"; mv -f -- "$p.ai-os-tmp" "$p"
    case "$p" in /data/*) chown ai:ai-sandbox -- "$p"; chmod 0660 -- "$p";; /home/ai/*) chown ai:ai -- "$p";; esac ;;
  remove-file) [ $# -eq 1 ] || refuse "remove-file <path>"; p=$(under "$1" /etc /data /home/ai); not_refused_file "$p"; rm -f -- "$p" ;;
  make-dir)   [ $# -eq 1 ] || refuse "make-dir <path>"; p=$(under "$1" /data /home/ai); install -d -- "$p"
    case "$p" in /data/*) chown ai:ai-sandbox -- "$p"; chmod 2770 -- "$p";; *) chown ai:ai -- "$p";; esac ;;
  remove-dir) [ $# -eq 1 ] || refuse "remove-dir <path>"; p=$(under "$1" /data /home/ai); rmdir -- "$p" ;;
  sandbox-run)
    net=none; cwd=""; envs=()
    while [ $# -gt 0 ]; do case "$1" in
      --net=*) net=${1#--net=};; --cwd=*) cwd=${1#--cwd=};; --env=*) envs+=("--setenv=${1#--env=}");;
      --) shift; break;; *) refuse "bad sandbox-run option: $1";; esac; shift; done
    [ $# -gt 0 ] || refuse "no command"; [ -n "$cwd" ] || refuse "no cwd"; cwd=$(under "$cwd" /data /home/ai)
    netprops=(--property=PrivateNetwork=yes)
    if [ "$net" != none ]; then
      netprops=(--property=IPAddressDeny=any)
      IFS=, read -ra ips <<< "$net"; for ip in "${ips[@]}"; do
        [[ "$ip" =~ ^[0-9a-fA-F.:]+$ ]] || refuse "bad ip: $ip"; netprops+=("--property=IPAddressAllow=$ip"); done
    fi
    args=(); for a in "$@"; do args+=("${a//\$/\$\$}"); done   # systemd expands ${VAR} in argv
    exec systemd-run --quiet --pipe --wait --collect --uid=ai-sandbox "--working-directory=$cwd" \
      "${netprops[@]}" --property=ProtectHome=yes --property=PrivateTmp=yes \
      --property=ProtectSystem=strict --property=UMask=0002 --property=TemporaryFileSystem=/data:ro \
      "--property=InaccessiblePaths=-/mnt/c -/media -/srv" "--property=BindPaths=$cwd" "--property=ReadWritePaths=$cwd" \
      "${envs[@]}" -- "${args[@]}" ;;
  *) refuse "unknown verb: ${verb:-<none>}" ;;
esac
```

- [ ] **Step 3: Write the setup script** — `trial/setup-admin.sh` (run once as root inside the distro; idempotent):

```bash
#!/usr/bin/env bash
# Phase 1c admin setup. Run as root inside the ai-os distro. Idempotent.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
apt-get install -y python3-venv npm cargo curl
install -m 0755 -o root -g root "$repo/runtime/admin/ai-os-admin" /usr/local/libexec/ai-os-admin
sed -i 's/\r$//' /usr/local/libexec/ai-os-admin
# One grant, nothing else. Removes the workshop's NOPASSWD:ALL and the bare systemd-run line.
rm -f /etc/sudoers.d/ai /etc/sudoers.d/ai-sandbox-run
install -m 0440 /dev/stdin /etc/sudoers.d/ai-os-admin <<< 'ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin'
visudo -cf /etc/sudoers.d/ai-os-admin
install -d -o ai -g ai-sandbox -m 2770 /data/snapshots /data/housekeeping
# Leftovers from a script that once ran with CRLF endings.
rmdir "/data/jobs"$'\r' "/data/projects"$'\r' 2>/dev/null || true
echo "admin ready"
```

- [ ] **Step 4: Install and run the test on the distro**

```bash
wsl -d ai-os -u root -- bash -c "sed -i 's/\r$//' /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-admin.sh; bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-admin.sh"
wsl -d ai-os -u ai -- bash -c "cd /data/housekeeping && bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime/admin/test-admin.sh"
```
Expected: every line `PASS`, exit 0. Also confirm the old grant is gone: `wsl -d ai-os -u ai -- sudo -n apt-get --version` must print `sudo: a password is required`.

- [ ] **Step 5: Run the existing sandbox integration tests** (they still call `sudo systemd-run` until Task 4, so they are EXPECTED to fail now — record the failure, do not fix here; Task 4 restores them).

- [ ] **Step 6: Commit**

```bash
git add runtime/admin trial/setup-admin.sh
git commit -m "feat(admin): ai-os-admin fixed-menu root wrapper, single sudo grant, machine test"
```

---

### Task 2: New actions and their risk rules

**Files:**
- Modify: `runtime/executor/src/action.rs`, `runtime/executor/src/rules.rs`

**Interfaces:**
- Produces:
```rust
// action.rs
pub enum Action { RunCommand{argv}, ReadFile{..}, WriteFile{..}, EditFile{..}, HttpPost{..},
    Install { packages: Vec<String> },
    Remove { packages: Vec<String> },
    Service { name: String, #[serde(rename = "do")] action: ServiceDo },
    MakeDir { path: String },
    FetchPackages { manager: Manager, packages: Vec<String> },
    SetSetting { key: String, value: String },
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum ServiceDo { Enable, Disable, Restart }
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)] #[serde(rename_all = "snake_case")]
pub enum Manager { Pip, Npm, Cargo }
pub fn valid_name(s: &str) -> bool   // ^[a-z0-9][a-z0-9+.@_-]*$
// rules.rs
pub const AI_ROOTS: [&str; 2] = ["/data", "/home/ai"];
pub fn under_any(path: &str, roots: &[&str]) -> bool   // lexical, like resolves_inside; absolute only
```

- [ ] **Step 1: Failing tests** in `action.rs` tests:

```rust
#[test]
fn parses_the_1c_actions() {
    let cases = [
        r#"{"kind":"install","packages":["cowsay"]}"#,
        r#"{"kind":"remove","packages":["cowsay"]}"#,
        r#"{"kind":"service","name":"nginx","do":"enable"}"#,
        r#"{"kind":"make_dir","path":"/data/work"}"#,
        r#"{"kind":"fetch_packages","manager":"pip","packages":["tabulate"]}"#,
        r#"{"kind":"set_setting","key":"projects_root","value":"/data/work"}"#,
    ];
    for c in cases { let a: Action = serde_json::from_str(c).unwrap_or_else(|e| panic!("{c}: {e}")); let back = serde_json::to_string(&a).unwrap(); assert_eq!(a, serde_json::from_str::<Action>(&back).unwrap()); }
    assert!(matches!(serde_json::from_str::<Action>(r#"{"kind":"service","name":"x","do":"enable"}"#).unwrap(), Action::Service { action: ServiceDo::Enable, .. }));
}
#[test]
fn names_are_validated() {
    assert!(valid_name("cowsay")); assert!(valid_name("libkf6-x.y+z"));
    assert!(!valid_name("-o")); assert!(!valid_name("a;b")); assert!(!valid_name("/tmp/x.service")); assert!(!valid_name("")); assert!(!valid_name("Cowsay"));
}
```
and in `rules.rs` tests:
```rust
#[test] fn install_remove_service_are_auto() {
    assert_eq!(classify(&Action::Install { packages: vec!["cowsay".into()] }, &ws()), Risk::Auto);
    assert_eq!(classify(&Action::Remove { packages: vec!["cowsay".into()] }, &ws()), Risk::Auto);
    assert_eq!(classify(&Action::Service { name: "nginx".into(), action: ServiceDo::Disable }, &ws()), Risk::Auto);
}
#[test] fn bad_names_are_blocked_not_run() {
    assert!(matches!(classify(&Action::Install { packages: vec!["-o".into()] }, &ws()), Risk::NeedsConfirm(_)));
    assert!(matches!(classify(&Action::Service { name: "/tmp/x.service".into(), action: ServiceDo::Enable }, &ws()), Risk::NeedsConfirm(_)));
    assert!(matches!(classify(&Action::FetchPackages { manager: Manager::Pip, packages: vec!["a b".into()] }, &ws()), Risk::NeedsConfirm(_)));
}
#[test] fn make_dir_roots() {
    assert_eq!(classify(&Action::MakeDir { path: "/data/work".into() }, &ws()), Risk::Auto);
    assert_eq!(classify(&Action::MakeDir { path: "/home/ai/x".into() }, &ws()), Risk::Auto);
    assert!(matches!(classify(&Action::MakeDir { path: "/opt/x".into() }, &ws()), Risk::NeedsConfirm(_)));
    assert!(matches!(classify(&Action::MakeDir { path: "/data/../etc".into() }, &ws()), Risk::NeedsConfirm(_)));
    assert!(matches!(classify(&Action::MakeDir { path: "relative".into() }, &ws()), Risk::NeedsConfirm(_)));
}
#[test] fn etc_read_is_auto_but_etc_write_is_not() {
    assert_eq!(classify(&Action::ReadFile { path: "/etc/fstab".into(), from_line: None, lines: None }, &ws()), Risk::Auto);
    assert!(matches!(classify(&Action::WriteFile { path: "/etc/fstab".into(), contents: "x".into() }, &ws()), Risk::NeedsConfirm(_)));
    assert!(matches!(classify(&Action::ReadFile { path: "/etc/../root/x".into(), from_line: None, lines: None }, &ws()), Risk::NeedsConfirm(_)));
}
#[test] fn fetch_and_set_setting_are_auto() {
    assert_eq!(classify(&Action::FetchPackages { manager: Manager::Npm, packages: vec!["left-pad".into()] }, &ws()), Risk::Auto);
    assert_eq!(classify(&Action::SetSetting { key: "projects_root".into(), value: "/data/work".into() }, &ws()), Risk::Auto);
}
```
(Blocked-by-name uses `NeedsConfirm` with reason "invalid name …" so the existing door refuses it without a new variant; the engine's user prompt will show the reason and the model will fix the name. `ponytail:` a third `Risk::Refused` variant only if a human ever wrongly approves a bad name.)

- [ ] **Step 2: Run** `cargo test -p executor` → the new tests fail to compile.
- [ ] **Step 3: Implement** the enum variants, `valid_name` (a hand loop: first char `a-z0-9`, rest in the set), `under_any` (reuse the normalisation in `resolves_inside`: refactor its loop into `fn normalize(path: &Path) -> Option<PathBuf>` and use it in both), and the classify arms:

```rust
Action::Install { packages } | Action::Remove { packages } => names_ok(packages),
Action::Service { name, .. } => names_ok(std::slice::from_ref(name)),
Action::FetchPackages { packages, .. } => names_ok(packages),
Action::MakeDir { path } => if under_any(path, &AI_ROOTS) { Risk::Auto } else { Risk::NeedsConfirm(format!("creates a folder outside the AI's areas: {path}")) },
Action::SetSetting { .. } => Risk::Auto,
// ReadFile arm: inside workspace OR under_any(path, &["/etc"]) → Auto
```
with `fn names_ok(ns: &[String]) -> Risk { match ns.iter().find(|n| !valid_name(n)) { None if !ns.is_empty() => Risk::Auto, Some(n) => Risk::NeedsConfirm(format!("invalid name: {n}")), None => Risk::NeedsConfirm("no names given".into()) } }`.
- [ ] **Step 4: Run** `cargo test -p executor` → all pass (the sandbox integration tests are gated off outside the distro).
- [ ] **Step 5: Commit** `git commit -am "feat(executor): install/remove/service/make_dir/fetch_packages/set_setting actions with risk rules"`.

---

### Task 3: Undo entries and the admin worker

**Files:**
- Create: `runtime/executor/src/undo.rs`, `runtime/executor/src/admin.rs`, `runtime/executor/tests/admin_integration.rs`
- Modify: `runtime/executor/src/worker.rs` (Outcome + trait), `runtime/executor/src/lib.rs`

**Interfaces:**
```rust
// undo.rs
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)] #[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoEntry {
    PackagesAdded { packages: Vec<String> },
    PackagesRemoved { packages: Vec<String> },
    ServiceState { name: String, was_enabled: bool, was_active: bool },
    FileBefore { path: String, contents: Option<String> },   // None = did not exist
    DirCreated { path: String },
    Setting { key: String, previous: Option<String> },
    ProjectSnapshot { folder: String, snapshot: String },
}
impl UndoEntry { pub fn describe(&self) -> String }   // plain words: "Removed the 3 packages installed (cowsay, …)"
// worker.rs
pub struct Outcome { pub ok: bool, pub detail: String, pub undo: Option<UndoEntry> }
impl Outcome { pub fn ok(d: impl Into<String>) -> Self; pub fn err(d: impl Into<String>) -> Self; pub fn with_undo(self, u: UndoEntry) -> Self }
pub trait Worker { fn run(&self, action: &Action) -> Outcome; fn reverse(&self, entry: &UndoEntry) -> Outcome { Outcome::err("this worker cannot undo") } }
// admin.rs
pub const WRAPPER: &str = "/usr/local/libexec/ai-os-admin";
pub struct AdminWorker;
impl AdminWorker { pub fn call(verb: &str, args: &[&str], stdin: Option<&str>) -> Outcome }  // sudo -n WRAPPER verb args…; stderr "refused: …" → err
impl Worker for AdminWorker { run: Install/Remove/Service/MakeDir/ReadFile/WriteFile/EditFile; reverse: PackagesAdded→remove, PackagesRemoved→install, ServiceState→enable/disable to was_enabled (+restart if was_active && still enabled), FileBefore→write-file or remove-file, DirCreated→remove-dir }
```

- [ ] **Step 1: Unit tests** (no machine) in `undo.rs` and `admin.rs`:
```rust
#[test] fn entries_round_trip_and_describe() {
    let e = UndoEntry::PackagesAdded { packages: vec!["cowsay".into(), "libx".into()] };
    let s = serde_json::to_string(&e).unwrap(); assert_eq!(e, serde_json::from_str(&s).unwrap());
    assert!(e.describe().contains("2 packages") && e.describe().contains("cowsay"));
    assert!(UndoEntry::FileBefore { path: "/etc/x".into(), contents: None }.describe().contains("removed /etc/x"));
}
// admin.rs: the pure helpers
#[test] fn set_difference_is_the_undo_list() { assert_eq!(added(&["a","b"], &["a","b","c","d"]), vec!["c","d"]); assert_eq!(added(&["a","b","c"], &["a"]), Vec::<String>::new()); assert_eq!(removed(&["a","b","c"], &["a"]), vec!["b","c"]); }
#[test] fn service_state_parses() { assert_eq!(parse_state("enabled active"), (true, true)); assert_eq!(parse_state("disabled inactive"), (false, false)); assert_eq!(parse_state("static active"), (false, true)); assert_eq!(parse_state("garbage"), (false, false)); }
```
- [ ] **Step 2: Machine tests** `tests/admin_integration.rs` (gated with `AI_OS_SANDBOX_IT`, same pattern as `sandbox_integration.rs`):
```rust
#[test] fn install_records_added_and_reverse_removes() {
    if !gated() { return; }
    let w = AdminWorker;
    let out = w.run(&Action::Install { packages: vec!["cowsay".into()] });
    assert!(out.ok, "{}", out.detail);
    let undo = out.undo.expect("undo entry");
    assert!(matches!(&undo, UndoEntry::PackagesAdded { packages } if packages.contains(&"cowsay".to_string())));
    assert!(w.reverse(&undo).ok);
    assert!(!AdminWorker::call("pkg-list", &[], None).detail.lines().any(|l| l == "cowsay"));
}
#[test] fn essential_remove_is_refused_with_reason() { if !gated() { return; } let o = AdminWorker.run(&Action::Remove { packages: vec!["bash".into()] }); assert!(!o.ok && o.detail.contains("refused")); }
#[test] fn write_outside_records_previous_and_reverse_restores() {
    if !gated() { return; }
    let p = "/data/housekeeping/it-undo.txt"; let _ = std::fs::remove_file(p);
    let o = AdminWorker.run(&Action::WriteFile { path: p.into(), contents: "v1".into() });
    assert!(o.ok); assert_eq!(o.undo, Some(UndoEntry::FileBefore { path: p.into(), contents: None }));
    let o2 = AdminWorker.run(&Action::EditFile { path: p.into(), find: "v1".into(), replace: "v2".into() });
    assert_eq!(o2.undo, Some(UndoEntry::FileBefore { path: p.into(), contents: Some("v1".into()) }));
    assert!(AdminWorker.reverse(&o2.undo.unwrap()).ok); assert_eq!(std::fs::read_to_string(p).unwrap(), "v1");
    assert!(AdminWorker.reverse(&o.undo.unwrap()).ok); assert!(!std::path::Path::new(p).exists());
}
#[test] fn make_dir_then_reverse() { if !gated() { return; } let o = AdminWorker.run(&Action::MakeDir { path: "/data/it-mk".into() }); assert!(o.ok); assert!(AdminWorker.reverse(&o.undo.unwrap()).ok); assert!(!std::path::Path::new("/data/it-mk").exists()); }
```
- [ ] **Step 3: Implement.** `Outcome` gets the field and constructors; update every `Outcome { ok, detail }` literal in worker.rs to the constructors (mechanical). `AdminWorker::run`: Install = `pkg-list` before → `install -- pkgs` → `pkg-list` after → `PackagesAdded{added}` (undo recorded even if the install partially failed: whatever was added is real); Remove symmetric with `PackagesRemoved{removed}`; Service = `state` before → verb → `ServiceState`; MakeDir = exists? (`Path::exists`) → `make-dir` → `DirCreated` only if it did not exist; WriteFile/EditFile = `read-file` before (err ⇒ None) → `write-file` with stdin (EditFile applies the same once-only `find` rule as the sandbox worker on the read contents) → `FileBefore`; ReadFile = `read-file`, windowed like the sandbox (share `head`/window code by making the sandbox's windowing a free fn `window(text, from_line, lines)`). All other actions → `Outcome::err("not an admin action")`.
- [ ] **Step 4: Run** unit tests on Windows and the machine tests in the distro (`AI_OS_SANDBOX_IT=1 cargo test -p executor --test admin_integration`). Expected: all pass; cowsay absent afterwards.
- [ ] **Step 5: Commit** `git commit -am "feat(executor): UndoEntry on every Outcome; AdminWorker through ai-os-admin with reverse"`.

---

### Task 4: Sandbox worker through the wrapper, `/etc` reads, fetch-packages

**Files:**
- Modify: `runtime/executor/src/worker.rs`, `runtime/executor/src/admin.rs` (helpers), `runtime/executor/tests/sandbox_integration.rs`, `runtime/executor/tests/admin_integration.rs`

**Interfaces:**
```rust
// admin.rs
pub fn resolver_ips() -> Vec<String>                        // `nameserver` lines of /etc/resolv.conf
pub fn registry_hosts(m: Manager) -> &'static [&'static str] // pip: pypi.org, files.pythonhosted.org; npm: registry.npmjs.org; cargo: crates.io, index.crates.io, static.crates.io
pub fn resolve_all(hosts: &[&str]) -> Vec<String>           // std::net::ToSocketAddrs on (host, 443), deduped
pub fn fetch_argv(m: Manager, packages: &[String]) -> Vec<Vec<String>> // the fixed command list per manager
// worker.rs: SandboxWorker::run_in_sandbox(&self, net: &str, envs: &[String], argv: &[String]) -> Outcome  (calls sudo -n WRAPPER sandbox-run …)
```

- [ ] **Step 1: Unit tests** in `admin.rs`:
```rust
#[test] fn fetch_commands_are_fixed_per_manager() {
    let pip = fetch_argv(Manager::Pip, &["tabulate".into()]);
    assert_eq!(pip[0], vec!["sh","-c","[ -x .venv/bin/pip ] || python3 -m venv .venv"]);   // one guarded step, no model text in it
    assert_eq!(pip[1], vec![".venv/bin/pip","install","--","tabulate"]);
    assert_eq!(fetch_argv(Manager::Npm, &["left-pad".into()]), vec![vec!["npm","install","--","left-pad"]]);
    assert_eq!(fetch_argv(Manager::Cargo, &["serde".into()]), vec![vec!["cargo","add","--","serde"], vec!["cargo","fetch"]]);
}
#[test] fn registry_hosts_per_manager() { assert!(registry_hosts(Manager::Pip).contains(&"files.pythonhosted.org")); assert_eq!(registry_hosts(Manager::Npm), &["registry.npmjs.org"]); assert!(registry_hosts(Manager::Cargo).contains(&"static.crates.io")); }
```
(`sh -c` with a **constant** string is not a model shell string; keep the constant in code and note it.)
- [ ] **Step 2: Machine tests** — in `sandbox_integration.rs` add:
```rust
#[test] fn etc_is_readable_unprivileged_but_not_shadow() {
    if !gated() { return; }
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: PathBuf::from("/data/housekeeping") };
    assert!(w.run(&Action::ReadFile { path: "/etc/fstab".into(), from_line: None, lines: None }).ok);
    assert!(!w.run(&Action::ReadFile { path: "/etc/shadow".into(), from_line: None, lines: None }).ok);
    assert!(!w.run(&Action::ReadFile { path: "/home/ai/.bashrc".into(), from_line: None, lines: None }).ok, "outside workspace and not /etc");
}
#[test] fn dollar_survives_argv() { if !gated() { return; } let w = SandboxWorker { user: "ai-sandbox".into(), workspace: PathBuf::from("/data/housekeeping") }; let o = w.run(&Action::RunCommand { argv: vec!["printf".into(), "%s".into(), "${HOME}".into()] }); assert!(o.detail.contains("${HOME}"), "{}", o.detail); }
```
and in `admin_integration.rs`:
```rust
#[test] fn pip_fetch_reaches_registry_and_nothing_else() {
    if !gated() { return; }
    let ws = PathBuf::from("/data/projects/it-fetch"); std::fs::create_dir_all(&ws).unwrap();
    let _ = std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status(); let _ = std::process::Command::new("chmod").arg("2770").arg(&ws).status();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let o = w.run(&Action::FetchPackages { manager: Manager::Pip, packages: vec!["tabulate".into()] });
    assert!(o.ok, "{}", o.detail); assert!(ws.join(".venv/bin/pip").exists());
    let leak = w.run(&Action::RunCommand { argv: vec!["curl".into(), "-sS".into(), "-m".into(), "5".into(), "https://example.com".into()] });
    assert!(!leak.ok, "ordinary run_command must still have no network");
    let _ = std::fs::remove_dir_all(&ws);
}
```
- [ ] **Step 3: Implement.** `SandboxWorker::run_in_sandbox` builds `sudo -n WRAPPER sandbox-run --net=<net> --cwd=<ws> [--env=…] -- argv…` (no `$` doubling here: the wrapper does it; the Rust side passes argv raw). `RunCommand` → `run_in_sandbox("none", &[], argv)`. `FetchPackages` → `net = resolver_ips() + resolve_all(registry_hosts(m))` joined by `,`; `envs = ["HOME=<ws>", "CARGO_HOME=<ws>/.cargo"]`; run each argv of `fetch_argv` in order, stop at the first failure; detail = last output. `ReadFile`: in `existing_inside`, accept a canonical target that starts with `/etc` too (only for reads — `EditFile` keeps workspace-only). Keep the `user` field (documented as always `ai-sandbox`; the wrapper pins it).
- [ ] **Step 4: Run** both machine test files in the distro and `cargo test -p executor` on Windows. Expected: all pass, including the 1a/1b sandbox tests that broke in Task 1.
- [ ] **Step 5: Commit** `git commit -am "feat(executor): sandbox runs through ai-os-admin sandbox-run; /etc reads; fetch_packages with registry allowlist"`.

---

### Task 5: Two lanes in the executor

**Files:**
- Modify: `runtime/executor/src/executor.rs`, `runtime/executor/src/main.rs`

**Interfaces:**
```rust
pub enum Lane { Sandbox, Admin, Engine }
pub fn lane(action: &Action, workspace: &Path, approved: bool) -> Lane
pub struct Executor<W: Worker> { sandbox: W, admin: W, log: ActionLog, workspace: PathBuf }
impl<W: Worker> Executor<W> {
    pub fn new(sandbox: W, admin: W, log: ActionLog, workspace: PathBuf) -> Self;
    pub fn execute(&self, job_id: &str, action: &Action, approved: bool) -> Result<ExecOutcome, LogError>;  // Lane::Engine → Blocked("engine action reached the executor") — never expected
    pub fn log_only(&self, job_id: &str, action: &Action, detail: &str) -> Result<(), LogError>;
    pub fn reverse(&self, job_id: &str, entry: &UndoEntry) -> Result<Outcome, LogError>;  // admin.reverse + log line "undo: …"
}
```
- [ ] **Step 1: Tests** in `executor.rs` (replace the `exec()` helper with one holding two `FakeWorker`s):
```rust
fn exec2() -> Executor<FakeWorker> { Executor::new(FakeWorker::new(true), FakeWorker::new(true), ActionLog::open_in_memory().unwrap(), PathBuf::from("/data/jobs/j1")) }
#[test] fn admin_kinds_go_to_the_admin_lane() {
    let e = exec2();
    e.execute("j", &Action::Install { packages: vec!["cowsay".into()] }, false).unwrap();
    e.execute("j", &Action::Service { name: "nginx".into(), action: ServiceDo::Restart }, false).unwrap();
    e.execute("j", &Action::MakeDir { path: "/data/x".into() }, false).unwrap();
    assert_eq!(e.admin.calls.borrow().len(), 3); assert!(e.sandbox.calls.borrow().is_empty());
}
#[test] fn approved_outside_write_goes_admin_inside_write_goes_sandbox() {
    let e = exec2();
    e.execute("j", &Action::WriteFile { path: "a.py".into(), contents: "x".into() }, false).unwrap();
    assert!(matches!(e.execute("j", &Action::WriteFile { path: "/etc/x".into(), contents: "x".into() }, false).unwrap(), ExecOutcome::Blocked(_)));
    e.execute("j", &Action::WriteFile { path: "/etc/x".into(), contents: "x".into() }, true).unwrap();
    assert_eq!(e.sandbox.calls.borrow().len(), 1); assert_eq!(e.admin.calls.borrow().len(), 1);
}
#[test] fn etc_read_and_fetch_stay_in_the_sandbox() {
    let e = exec2();
    e.execute("j", &Action::ReadFile { path: "/etc/fstab".into(), from_line: None, lines: None }, false).unwrap();
    e.execute("j", &Action::FetchPackages { manager: Manager::Pip, packages: vec!["x".into()] }, false).unwrap();
    assert_eq!(e.sandbox.calls.borrow().len(), 2); assert!(e.admin.calls.borrow().is_empty());
}
#[test] fn set_setting_never_reaches_a_worker_but_log_only_records_it() {
    let e = exec2();
    let a = Action::SetSetting { key: "projects_root".into(), value: "/data/w".into() };
    assert_eq!(lane(&a, Path::new("/data/jobs/j1"), false), Lane::Engine);
    e.log_only("j", &a, "ok: set").unwrap();
    assert!(e.sandbox.calls.borrow().is_empty() && e.admin.calls.borrow().is_empty());
    assert_eq!(e.log.count_for("j").unwrap(), 1);   // add `count_for` to ActionLog if absent (SELECT count(*) WHERE job_id=?)
}
#[test] fn reverse_goes_through_admin_and_is_logged() { let e = exec2(); e.reverse("j", &UndoEntry::DirCreated { path: "/data/x".into() }).unwrap(); assert_eq!(e.admin.reversed.borrow().len(), 1); }
```
`FakeWorker` gains `pub reversed: RefCell<Vec<UndoEntry>>` and implements `reverse` by recording.
- [ ] **Step 2: Run** → compile failures. **Step 3: Implement** `lane()`: Install/Remove/Service/MakeDir → Admin; ReadFile/WriteFile/EditFile → Admin iff `!resolves_inside(path, ws) && approved && !(ReadFile under /etc)`, else Sandbox; SetSetting → Engine; all else Sandbox. Update `main.rs` demo to pass `AdminWorker` (or a `FakeWorker` for the demo). **Step 4: Run** `cargo test -p executor` → pass. **Step 5: Commit** `git commit -am "feat(executor): sandbox/admin lanes, log-only door, reverse through admin"`.

---

### Task 6: Schema, moves and prompt

**Files:**
- Modify: `runtime/core/src/schema.rs`, `moves.rs`, `prompt.rs`

**Interfaces:**
```rust
// moves.rs
Move::Housekeep { goal: String, understood: String, #[serde(default, skip_serializing_if = "Option::is_none")] remember: Option<String> }
// prompt.rs
pub fn front_door(...) -> Prompt   // allowed: ["reply","start","housekeep"]
pub fn job_turn(...)               // housekeeping text branch; Machine line mentions install/fetch_packages
```
- [ ] **Step 1: Tests**: extend `moves.rs::parses_every_move` with `{"move":"housekeep","goal":"prepare /data/work","understood":"Housekeeping: …"}` and an `act` carrying each new action; `move_names_match_schema` expects `["reply","start","housekeep","ask","plan","act","replan","done","give_up"]`; in `schema.rs` add `fn action_kinds_match_the_enum()` asserting `$defs.action.oneOf` kinds == `["run_command","read_file","write_file","edit_file","http_post","install","remove","service","make_dir","fetch_packages","set_setting"]`; `move_is_always_the_first_key` already covers order. In `prompt.rs`: `front_door(...).allowed == ["reply","start","housekeep"]`; `job_turn` for a job with `housekeeping = true` contains "no project, no blueprint" and NOT "no blueprint yet"; `SYSTEM` contains "install" and "fetch_packages".
- [ ] **Step 2: Run** → fail. **Step 3: Implement.** Schema entries (discriminator first):
```json
{ "type":"object", "properties": { "move": {"enum":["housekeep"]}, "goal": {"type":"string"}, "understood": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","goal","understood"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["install"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","packages"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["remove"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","packages"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["service"]}, "name": {"type":"string"}, "do": {"enum":["enable","disable","restart"]} }, "required":["kind","name","do"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["make_dir"]}, "path": {"type":"string"} }, "required":["kind","path"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["fetch_packages"]}, "manager": {"enum":["pip","npm","cargo"]}, "packages": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":10} }, "required":["kind","manager","packages"], "additionalProperties": false }
{ "type":"object", "properties": { "kind": {"enum":["set_setting"]}, "key": {"enum":["projects_root"]}, "value": {"type":"string"} }, "required":["kind","key","value"], "additionalProperties": false }
```
SYSTEM additions (three lines, spec §9): "- Install software with `install` (apt), never with run_command apt. Enable, disable or restart services with `service`. Language packages (pip, npm, crates) come through `fetch_packages`: the sandbox has no other network.\n- A file outside the project needs the user's yes; say in one line why you need it.\n- The machine's own layout, settings and installed tools are housekeeping (`housekeep`), not a project." Front-door hint gains "housekeep (the machine itself: folders, settings, tools; give goal, understood)". `job_turn`: when `job.housekeeping`, replace the Project/BLUEPRINT block with "Housekeeping on the machine itself (scratch folder is the working directory): no project, no blueprint. Anything that must outlive this job is a setting: set_setting projects_root=<abs path under /data or /home/ai>." Machine line: "Ubuntu Linux (python3, no `python`; apt via install; pip/npm/cargo via fetch_packages)". `allowed_moves` unchanged (housekeeping uses the same job moves).
- [ ] **Step 4: Run** `cargo test -p aios-core` → pass (engine tests untouched so far; `Job` gains `housekeeping` in Task 7 — for this task read it via a `Job::housekeeping` field added now with `#[serde(default)]`, default false). **Step 5: Commit** `git commit -am "feat(core): housekeep move, 1c action shapes in the grammar, prompt rules for the hands"`.

---

### Task 7: Store tables, job fields, folder-based workspace

**Files:**
- Modify: `runtime/core/src/store.rs`, `job.rs`, `engine.rs` (only `workspace`/`executor_for`/`read_blueprint`/`create_project_folder`/`write_or_wipe_last_run`/`run_turns` LAST_RUN read/`Start` arm), `testing.rs`

**Interfaces:**
```rust
// job.rs — new fields, all #[serde(default)]
pub folder: String, pub housekeeping: bool, pub new_project: bool
impl Job { pub fn new(project, folder: &str, goal, creative, understood) -> Job; pub fn housekeeping(folder: &str, goal, understood) -> Job /* project "", housekeeping true, state Planning if creative-like? no: Asking */ }
// store.rs
pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError>;
pub fn set_setting(&self, key: &str, value: &str) -> Result<Option<String>, StoreError>;  // returns previous
pub fn add_undo(&self, job_id: &str, entry: &UndoEntry) -> Result<(), StoreError>;         // seq = count+1
pub fn unapplied_undo(&self, job_id: &str) -> Result<Vec<(i64, UndoEntry)>, StoreError>;   // seq DESC
pub fn mark_undo_applied(&self, row_id: i64) -> Result<(), StoreError>;
pub fn last_undoable_job(&self) -> Result<Option<Job>, StoreError>;  // newest done/failed/cancelled job with unapplied undo rows
// engine.rs
fn workspace(&self, job: &Job) -> PathBuf  // PathBuf::from(&job.folder)
fn projects_root(&self) -> PathBuf          // setting or self.default_root
pub(crate) fn executor_for(&self, job: &Job) -> …; pub(crate) fn read_blueprint(&self, job: &Job) -> …
```
- [ ] **Step 1: Tests** in `store.rs`: settings round trip returns previous; `add_undo` seq increments and `unapplied_undo` comes back newest first; `mark_undo_applied` removes it from `unapplied_undo`; `last_undoable_job` ignores open jobs and jobs without rows. In `job.rs`: a job JSON without the three new keys still deserialises. In `engine.rs`: `existing_project_is_reused_not_recreated` additionally asserts the stored `folder` is unchanged after `e.store.set_setting("projects_root", other)` between the two starts; new test `new_projects_land_under_the_projects_root_setting` (set the setting to a temp dir, start a project, assert the folder is under it and the project row's folder matches).
- [ ] **Step 2: Run** → fail. **Step 3: Implement.** Tables: `settings(key TEXT PRIMARY KEY, value TEXT NOT NULL)`, `undo(id INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL, seq INTEGER NOT NULL, entry TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL)`. Engine: `Start` arm computes `folder = existing.map(|p| p.folder).unwrap_or(projects_root().join(name))`, creates the folder only when new, upserts with that folder, builds `Job::new(&name, &folder, …)` with `new_project = existing.is_none()`. `write_or_wipe_last_run` and the LAST_RUN read use `workspace(job)` and skip when `job.housekeeping`. `testing.rs`: `engine_with`'s factory now receives the workspace path; keep the 3-tuple signature.
- [ ] **Step 4: Run** `cargo test -p aios-core` → pass. **Step 5: Commit** `git commit -am "feat(core): settings + undo tables; jobs carry their folder; projects_root setting for new projects"`.

---

### Task 8: Housekeeping jobs, set_setting, absolute blueprint gate, real test writes

**Files:**
- Modify: `runtime/core/src/engine.rs`, `testing.rs`, `core/Cargo.toml` (none new)

**Interfaces:**
```rust
// engine.rs
pub const HOUSEKEEPING_DIR: &str = "/data/housekeeping";   // overridable in Engine::new via `housekeeping_dir: PathBuf` (tests pass a temp dir)
fn apply_setting(&self, job: &mut Job, key: &str, value: &str) -> Outcome   // validates key ∈ {projects_root}, value absolute under /data|/home/ai (or under the test root when it starts with the temp root — use `rules::under_any(value, &roots)` with roots = AI_ROOTS + [default_root])
// testing.rs
pub struct ScriptedWorker { pub rec: Recorder, pub ws: PathBuf }  // WriteFile/EditFile really write under ws (create parents), then record
pub struct Recorder { calls, outcomes, admin_calls: Rc<RefCell<Vec<Action>>>, reversed: Rc<RefCell<Vec<UndoEntry>>> }
```
- [ ] **Step 1: Tests** in `engine.rs`:
```rust
fn housekeep() -> Move { Move::Housekeep { goal: "prepare /data/work for all projects".into(), understood: "Housekeeping: I'll create the folder and make it the projects root".into(), remember: None } }
#[test] fn housekeeping_job_has_no_project_no_blueprint_gate_and_no_last_run() {
    let (mut e, rec, root) = engine_with(vec![housekeep(), plan(), act(1, Action::MakeDir { path: root.join("work").display().to_string() }), act(1, write("notes.txt")), done(run("true"))], "hk");
    let out = e.handle("prepare a folder for all my projects").unwrap();
    assert!(out.last().unwrap().contains("finished"), "{out:?}");
    assert!(!root.join("LAST_RUN.md").exists() && !e.housekeeping_dir().join("LAST_RUN.md").exists());
    assert_eq!(rec.admin_calls.borrow().len(), 1, "make_dir went to the admin lane");
    assert!(e.store.list_projects().unwrap().is_empty(), "no project row");
}
#[test] fn set_setting_moves_new_projects_and_rejects_bad_values() {
    let (mut e, _, root) = engine_with(vec![
        housekeep(), plan(), act(1, Action::SetSetting { key: "projects_root".into(), value: root.join("work").display().to_string() }), done(run("true")),
        start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
    ], "setting");
    e.handle("move projects").unwrap(); e.handle("make p").unwrap();
    assert!(root.join("work/p/BLUEPRINT.md").exists(), "new project under the new root");
    let (mut e2, _, _) = engine_with(vec![housekeep(), plan(), act(1, Action::SetSetting { key: "colour".into(), value: "blue".into() }), Move::GiveUp { reason: "x".into(), missing: "y".into() }], "badkey");
    e2.handle("set colour").unwrap();
    let prompts = e2.model.prompts.borrow(); assert!(prompts[2].user.contains("unknown setting") && prompts[2].user.contains("projects_root"));
}
#[test] fn done_on_a_new_project_without_a_blueprint_file_is_rejected() {
    let (mut e, _, root) = engine_with(vec![start("p", true), plan(), act(1, run("sed")), done(run("true")), act(1, write("BLUEPRINT.md")), done(run("true"))], "absgate");
    let out = e.handle("go").unwrap(); assert!(out.last().unwrap().contains("finished"));
    let prompts = e.model.prompts.borrow(); assert!(prompts[3].user.contains("rejected: create BLUEPRINT.md"), "{}", prompts[3].user);
    assert!(root.join("p/BLUEPRINT.md").exists(), "the scripted worker really wrote it");
}
#[test] fn housekeep_out_of_turn_is_rejected_like_start() { /* a running job then Move::Housekeep → note "a job is running" */ }
```
- [ ] **Step 2: Run** → fail. **Step 3: Implement.** Front door `Move::Housekeep` arm: `Job::housekeeping(&housekeeping_dir, goal, understood)` (state Asking; creative flag false), same `remember` handling as `Start`, then `run_turns`. `perform()`: before the executor, `if let Action::SetSetting { key, value } = &action { let outcome = self.apply_setting(job, key, value); exec.log_only(...)?; push StepRecord; save undo entry Setting{previous}; return Ok(None) or the fail path }`. `Done` arm: `if !job.housekeeping { if job.new_project && !self.workspace(&job).join("BLUEPRINT.md").exists() { reject "create BLUEPRINT.md for this new project before saying done" } else if last_code_change > last_blueprint_update { existing rejection } }`. `is_blueprint` tracking skipped for housekeeping. `run_turns` catch-all arm lists `housekeep` with `start`/`reply` as out of turn. `testing.rs`: factory becomes `Box::new(move |ws| (Box::new(ScriptedWorker{rec: r2.clone(), ws: ws.to_path_buf()}), Box::new(AdminRecorder(r3.clone()))))` — `WorkerFactory` type becomes `Box<dyn Fn(&Path) -> (Box<dyn Worker>, Box<dyn Worker>)>`; `main.rs` follows in Task 10.
- [ ] **Step 4: Run** `cargo test -p aios-core` → pass, including all earlier engine tests (their `write("BLUEPRINT.md")` now creates a real file, which the absolute gate needs). **Step 5: Commit** `git commit -am "feat(core): housekeeping jobs, set_setting through the engine, absolute blueprint gate for new projects"`.

---

### Task 9: Undo — project snapshots, rows, the word, the reversal

**Files:**
- Create: `runtime/core/src/snapshot.rs`
- Modify: `runtime/core/src/engine.rs`, `lib.rs`

**Interfaces:**
```rust
// snapshot.rs (all as the current user; no sudo)
pub fn create_project_dir(path: &Path) -> std::io::Result<bool>       // btrfs subvolume create, else create_dir_all; returns is_subvolume; then chgrp/chmod as today
pub fn is_subvolume(path: &Path) -> bool                              // inode == 256 && same-device rule: `metadata.ino() == 256`
pub fn take(folder: &Path, snapshots_dir: &Path, name: &str) -> std::io::Result<Option<PathBuf>>  // None when not a subvolume; deletes older `<project>@*` first
pub fn restore(folder: &Path, snapshot: &Path) -> std::io::Result<()> // property set ro false; rename folder→folder.old; rename snapshot→folder; rm -rf folder.old
// engine.rs
pub fn is_undo(text: &str) -> bool   // "undo", "undo that", "undo the last job", "roll back", "put it back", "revert"; negations refused ("don't undo")
fn undo_last(&mut self) -> Result<Vec<String>, EngineError>
```
- [ ] **Step 1: Tests**:
```rust
#[test] fn undo_words() { assert!(is_undo("undo")); assert!(is_undo("Undo that.")); assert!(is_undo("roll back")); assert!(!is_undo("don't undo")); assert!(!is_undo("undo is a word")); }
#[test] fn undo_reverses_the_last_jobs_rows_newest_first_and_reports() {
    let (mut e, rec, root) = engine_with(vec![housekeep(), plan(), act(1, Action::MakeDir { path: root.join("w").display().to_string() }), act(1, Action::SetSetting { key: "projects_root".into(), value: root.join("w").display().to_string() }), done(run("true"))], "undo");
    rec.admin_outcomes.borrow_mut().push_back(Outcome::ok("made").with_undo(UndoEntry::DirCreated { path: root.join("w").display().to_string() }));
    e.handle("prep").unwrap();
    let out = e.handle("undo").unwrap();
    assert_eq!(rec.reversed.borrow().len(), 1, "the dir reversal went to the admin worker");
    assert!(out.iter().any(|l| l.contains("projects_root")) && out.iter().any(|l| l.contains("folder")), "{out:?}");
    assert_eq!(e.store.get_setting("projects_root").unwrap(), None, "setting reversed by the engine");
    assert!(e.handle("undo").unwrap()[0].contains("Nothing left to undo"));
}
#[test] fn undo_while_a_job_is_open_is_refused() { /* start a job that asks; "undo" → "finish or stop the current job first" and the job stays open */ }
#[test] fn undo_is_not_a_model_call() { /* prompts count unchanged across handle("undo") */ }
#[test] fn a_failing_reversal_is_reported_and_the_rest_still_run() { /* two admin rows, first reverse err → both lines present, one says "could not" */ }
#[test] fn cancelled_jobs_are_undoable() { /* job with an admin row, then "stop", then "undo" reverses it */ }
```
Machine test in `runtime/core/tests/snapshot_it.rs` (gated): create `/data/projects/it-snap` via `create_project_dir` → `is_subvolume` true; write `a.txt`; `take` → path exists; delete `a.txt`; `restore` → `a.txt` back and folder perms `2770`; clean up.
- [ ] **Step 2: Run** → fail. **Step 3: Implement.** `create_project_folder` → `snapshot::create_project_dir`. In `Start`'s job creation (not housekeeping): `if let Some(snap) = snapshot::take(&folder, &snapshots_dir, &format!("{name}@{}", job.id))? { store.add_undo(&job.id, &UndoEntry::ProjectSnapshot{folder, snapshot}) }` — `snapshots_dir` = `Engine::new` parameter (tests: temp; main: `/data/snapshots`). `perform()`: after `ExecOutcome::Ran(outcome)`, `if let Some(u) = outcome.undo.clone() { self.store.add_undo(&job.id, &u)? }`. `handle_inner`: when no job open and `is_undo(text)` → `undo_last()`; when a job is open and `is_undo` → "finish or stop the current job first". `undo_last`: `last_undoable_job()` or "Nothing left to undo."; for each row newest first: `ProjectSnapshot` → `snapshot::restore`; `Setting` → `store.set_setting`/delete previous None; else `executor_for(&job).reverse(&job.id, &entry)`; mark applied on success **and** on failure (a failed reverse must not be retried blindly; the line says "could not …, left as is"); lines from `entry.describe()`; append the caveat line "Not covered: unsaved work in open programs; files written outside the project." and, for a plain-folder project, "Files in *{project}* were not covered: the project predates undo."
- [ ] **Step 4: Run** unit tests on Windows; snapshot machine test in the distro. **Step 5: Commit** `git commit -am "feat(core): per-job undo — project btrfs snapshots, undo rows from outcomes, the undo word, reversal report"`.

---

### Task 10: Wiring, live acceptance, close-out

**Files:**
- Modify: `runtime/core/src/main.rs`, `docs/superpowers/specs/2026-09-15-ai-os-design.md` (§4.7, §4.8, §11), the 1c design (§11 results)
- Create: `runtime/core/tests/live_1c.rs`

- [ ] **Step 1: Wire main.rs**: factory returns `(SandboxWorker{…}, AdminWorker)`; `Engine::new(store, model, root, Some(db), factory, PathBuf::from("/data/housekeeping"), PathBuf::from("/data/snapshots"))`.
- [ ] **Step 2: Live acceptance** `tests/live_1c.rs` gated by `AI_OS_LIVE=1` (same shape as the 1b primes test): drives the real `Engine` with `OllamaModel::local("qwen3.5:9b")` through the four scripts of spec §11 with a fresh db under `/data/ai-os-live-1c.db`, asserting after each: (1) `/data/work` exists and `get_setting("projects_root") == "/data/work"`, then a primes project lands under it; (2) `pkg-list` contains `cowsay`; (3) after "undo": cowsay gone; after "undo" again: `/data/work` gone and setting None; (4) `fetch_packages pip tabulate` appears in the action log with `ok:`. Print the step count and seconds per script.
- [ ] **Step 3: Run** the full suite: `cargo test` in `runtime/` on Windows; in the distro `AI_OS_SANDBOX_IT=1 cargo test -p executor` then `AI_OS_LIVE=1 cargo test -p aios-core --test live_1c -- --nocapture`. Record results verbatim in the 1c design §11.
- [ ] **Step 4: Spec close-out** per design §10; update the memory file `ai-os-project` with the 1c status line.
- [ ] **Step 5: Commit** `git commit -am "feat: Phase 1c wired; live acceptance; spec close-out"`. Do not merge, tag or push: report to him and stop.

---

## Self-review

- **Spec coverage:** §3 table → Tasks 2, 3, 4, 5, 8; §3.1 → Task 4; §4 → Tasks 3, 7, 9; §5 → Task 1; §6 → Task 5; §7 → Tasks 6, 7, 8; §8 → Task 8; §9 → Task 6; §10 → Task 10; §11 → every task's tests + Task 10; §13 corrections → Task 1 (jail, `$`, setup packages, chown), Task 3 (set difference, name regex in wrapper), Task 4 (`/etc` unprivileged reads, HOME/CARGO_HOME), Task 7 (folder kept), Task 8 (housekeeping branches, gate), Task 9 (cancelled jobs, plain-folder caveat).
- **Type consistency:** `Outcome::{ok,err,with_undo}`, `UndoEntry` variants, `Lane`, `lane()`, `Executor::{new(sandbox, admin, log, ws), execute, log_only, reverse}`, `WorkerFactory -> (Box<dyn Worker>, Box<dyn Worker>)`, `Job::{new(project, folder, goal, creative, understood), housekeeping(folder, goal, understood)}`, `Job.{folder,housekeeping,new_project}`, `Store::{get_setting,set_setting,add_undo,unapplied_undo,mark_undo_applied,last_undoable_job}`, `snapshot::{create_project_dir,is_subvolume,take,restore}`, `Engine::new(store, model, root, log, factory, housekeeping_dir, snapshots_dir)`, `is_undo`, `undo_last` — used with the same names throughout.
- **Placeholders:** none; the two "/* … */" test bodies in Task 9 name their exact assertion in the comment and must be written in full by the implementer.
