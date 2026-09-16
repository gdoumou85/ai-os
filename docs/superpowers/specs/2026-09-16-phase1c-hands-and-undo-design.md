# Phase 1c — the admin hand, fetch-packages, undo, housekeeping

Date: 2026-09-16. Parent: `2026-09-15-ai-os-design.md` (§4.4, §4.7, §4.8, §11). Builds on Phase 1b (`2026-09-16-phase1b-core-loop-design.md`). Path-checked against the live workshop the same day (§13); the corrections are folded in.

His decisions today, all five agreed: undo per hand (not per disk); one fixed-menu root wrapper under sudo for the workshop, a privileged daemon at packaging; install and enable run freely, remove and disable run freely with the reverse recorded first, purge never exists; housekeeping is a job with no project and no blueprint whose outcomes are settings; fetch-packages registries are pip, npm and crates only.

**One rule changed by the path check (§13 item 7):** a write under `/etc` is **not** free. A unit file, a cron entry, an apt source or a PAM rule under `/etc` is root code execution by another door, which the parent's own rule forbids ("nothing the model writes is ever run with admin rights", §4.7). So every write outside the project asks first, exactly as it did in 1b; what 1c adds is that an **approved** one now actually runs (through the admin worker, with its reverse recorded). No acceptance step needs an `/etc` write.

## 1. What 1c delivers

After 1c the AI can, on its own and with a way back for every step:

- install and remove software (apt) and enable, disable or restart services, freely;
- change a file outside the project once the user says yes;
- pull language packages (pip, npm, crates) into a project with the network open to those registries only;
- do housekeeping on the machine itself ("prep a folder where all projects live") without inventing a project;
- and the user can say **undo** and the last job's changes are reversed, in order, with a plain-language report.

The workshop's root-equivalent sudo grant is gone. One wrapper program with a fixed menu is the only privileged thing the executor can call, and it also builds the sandbox's `systemd-run` line with a fixed property set (parent §11 carry-forward).

## 2. Two facts from the live workshop that shaped this

1. **The undo disk does not hold what the AI changes.** Installed software and `/etc` live on the root disk (ext4, not snapshottable). Only `/data` is btrfs, and even there `/data/projects` sits outside the one `live` subvolume the Phase 0 rollback proved. So undo is done **per hand**: each admin operation records its own reverse before it runs, and project files get a real btrfs snapshot because every new project becomes its own subvolume. Same code on WSL and bare metal. Parent §4.8's "one snapshot disk" wording is corrected by this document (§10).
2. **The executor runs as `ai` with `NOPASSWD:ALL`.** The admin hand needs root. Instead of widening that, 1c narrows it to one root-owned wrapper (§5).

Also found: stray `jobs\r` and `projects\r` folders on `/data` from a script that once ran with CRLF endings. The setup script removes them.

## 3. New hands (executor `Action` variants)

Every hand is a typed variant, dispatched to coded handlers. No shell string from the model runs with privilege, ever (parent §4.7).

| Action | Fields | Risk (decision 9) | Worker | Reverse recorded before/around it |
|---|---|---|---|---|
| `install` | `packages: [String]` | Auto | admin | the installed-package set before and after; undo removes exactly the difference (apt pulls dependencies in, so the named list is not the truth) |
| `remove` | `packages: [String]` | Auto | admin | same set-difference; undo reinstalls what actually went (apt removes reverse-dependencies too). `apt-get remove`, never `purge`: config files stay on disk |
| `service` | `name`, `do: enable\|disable\|restart` | Auto | admin | `is-enabled` + `is-active` before; undo restores both. `restart`'s reverse is nothing |
| `make_dir` | `path` | Auto under `/data` and `/home/ai`; NeedsConfirm elsewhere | admin | "did not exist" → undo removes it if still empty. Under `/data` the folder is chowned `ai:ai-sandbox` 2770 so projects can be created in it |
| `fetch_packages` | `manager: pip\|npm\|cargo`, `packages: [String]` | Auto | sandbox, network to registries only | none needed: everything lands inside the project, covered by the project snapshot |
| `set_setting` | `key`, `value` | Auto for known keys, rejected otherwise | engine (store), logged through the executor's door | previous value |

Existing `read_file` / `write_file` / `edit_file` **outside the workspace** keep their NeedsConfirm rule; once approved they run through the **admin** worker (root), with the file's previous contents (or "did not exist") recorded first, and the wrapper limits them to `/etc`, `/data` and `/home/ai` and refuses `/etc/sudoers*`, `/etc/passwd`, `/etc/shadow`, `/etc/group`. In 1b an approved outside-workspace write had nowhere to run.

`read_file` under `/etc` becomes **Auto and stays unprivileged**: the sandbox worker's read (which runs as the executor's own user `ai`, an ordinary user) accepts a canonical path under `/etc` as well as inside the workspace. `/etc/shadow` and friends are unreadable to `ai` by permissions, as they should be; root never reads anything without the user's yes.

**Refusals coded in the wrapper, not in the model:** package and service names must match `^[a-z0-9][a-z0-9+.@_-]*$` (no `/`, no leading `-`) and are passed after `--`, because apt accepts `-o APT::Update::Pre-Invoke::=/bin/echo` as a "package" and `systemctl enable /some/path.service` links a unit from anywhere. `remove` refuses `Essential: yes` and `Priority: required` packages (dpkg's own flags) and the runtime pieces `btrfs-progs systemd sudo`. `service` refuses to disable `ollama dbus gdm* systemd-* ssh* NetworkManager ai-os*`. A refused action is an `error` outcome with the reason, fed back to the model like any failure.

### 3.1 fetch-packages: how the network opens

The sandbox unit normally has `PrivateNetwork=yes`. For a `fetch_packages` step the worker asks the wrapper for a unit with network on but `IPAddressDeny=any` and `IPAddressAllow=<addresses>`: the machine's DNS resolver (from `/etc/resolv.conf`) plus the registry hosts resolved **at that moment** by the executor:

- pip: `pypi.org`, `files.pythonhosted.org`
- npm: `registry.npmjs.org`
- cargo: `crates.io`, `index.crates.io`, `static.crates.io`

Proven on the workshop kernel: the deny is real (an allowed address answers 200, a non-allowed one times out, and a foreign hostname on a shared CDN edge is rejected by the edge). The command is fixed per manager and lands inside the project: pip → `python3 -m venv .venv` (if missing) then `.venv/bin/pip install -- <pkgs>`; npm → `npm install -- <pkgs>`; cargo → `cargo add -- <pkgs>` then `cargo fetch`. The unit gets `HOME=<workspace>` and `CARGO_HOME=<workspace>/.cargo` because the sandbox has no home. The model names the manager and the packages (same name regex, enforced in Rust); it never writes the command. A CDN that rotates addresses mid-step makes the step fail and the model retries: accepted, and rare within one step. `ponytail:` if the allowlist proves flaky, the upgrade path is a local allowlisting proxy owned by the executor.

## 4. Undo

**Unit of undo: one job.** Every reverse is a row in a new `undo` table: `id, job_id, seq, entry(JSON), applied, at`. An entry is one of: packages added, packages removed, a service's previous state, a file's previous contents (or none), a directory created, a setting's previous value, a project snapshot.

- **Project files:** `create_project_folder` makes each new project a btrfs subvolume (`btrfs subvolume create` works unprivileged; it falls back to a plain folder where the parent is not btrfs, e.g. tests), then the usual `chgrp ai-sandbox` + `chmod 2770` because a subvolume does not inherit the setgid group. At the start of every job in a subvolume project, a read-only snapshot is taken into `/data/snapshots/<project>@<job>` (the folder is chowned to `ai:ai-sandbox` by setup). Undo restores it as the Phase 0 probe did, **entirely as user `ai`**: make the snapshot writable (`btrfs property set -ts … ro false`), swap the folders, `rm -rf` the old subvolume (the kernel allows an owner to remove an empty subvolume this way; no root and no wrapper verb needed). Only the last snapshot per project is kept; older ones are removed when a new job starts. A project that predates 1c is a plain folder: no snapshot is possible, and the undo report says so for that job ("files in *primes* were not covered: the project predates undo").
- **install / remove / service / file / make_dir / set_setting:** the reverse is in the row (§3 last column). Admin entries are reversed by the admin worker (through the wrapper); setting and snapshot entries by the engine itself.

**Trigger:** the user says **undo** (a fixed word list, like `stop`: "undo", "undo that", "undo the last job", "roll back", "put it back"; negations refused like `is_yes`). Only when no job is open. The engine takes the most recent job that is done, failed **or cancelled** and still has unapplied rows, applies them in reverse order, marks each applied, and reports one line per reversal in plain words ("Removed the 3 packages installed for *primes*", "Put `/etc/foo.conf` back"). A reversal that fails is reported and the rest still run. A second "undo" reaches the job before that. No model call: undo is deterministic, like stop and approvals.

**What undo cannot do**, said plainly in the report: unsaved work in an open program (parent §4.8), files a program wrote outside the project during the job, and a package version that the archive no longer carries (reinstall takes the current one).

## 5. The admin worker and the wrapper

`AdminWorker` (new, in the executor crate) runs every admin action as `sudo -n /usr/local/libexec/ai-os-admin <verb> [args]`, feeding file contents on stdin. The wrapper is a **root-owned shell script, 0755, installed by the setup script**, with a `case` on the verb and nothing else:

```
install -- <pkg>…            DEBIAN_FRONTEND=noninteractive apt-get install -y -o DPkg::Lock::Timeout=60
remove -- <pkg>…             apt-get remove -y (never purge; refusals in §3)
pkg-list                     dpkg-query -W -f '${Package}\n' (installed only) — the before/after set
service <name> enable|disable|restart|state
read-file <path>             roots /etc, /data, /home/ai; refused files; no symlinks
write-file <path>            contents on stdin; roots /etc, /data, /home/ai; parent dirs created; refused files
remove-file <path>           undo of a write that created the file
make-dir <path>              roots /data, /home/ai; under /data chown ai:ai-sandbox 2770, under /home/ai chown ai
remove-dir <path>            rmdir, only if empty (undo of make-dir)
sandbox-run --net=none|<ip,…> --cwd=<ws> [--env=K=V]… -- <argv>…
                             the systemd-run line with the FIXED property set; uid is always ai-sandbox; cwd must be under /data or /home/ai
```

The sudoers file becomes exactly one line: `ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin`. The `NOPASSWD:ALL` line and the bare `systemd-run` line are removed by the setup script (`create_project_folder`'s `chgrp`/`chmod` never needed sudo: `ai` is in the group). The wrapper `set -euo pipefail`s, validates every path with `realpath -m` against its allowed roots and every name with the regex, and refuses anything not in its menu. It is shell, not Rust, on purpose: it is workshop transport that disappears at packaging when the executor becomes the privileged daemon (his decision 2 today), and a hundred auditable lines beat a second privileged binary.

`SandboxWorker` stops calling `sudo systemd-run` itself and calls `ai-os-admin sandbox-run`. Two corrections to its property set from the path check: `InaccessiblePaths=-/mnt/c -/media -/srv` instead of `-/mnt`, because the workshop's `/etc/resolv.conf` is a link into `/mnt/wsl` and hiding all of `/mnt` kills name resolution for the fetch step (the Windows drive stays hidden); and every `$` in argv is doubled before it reaches `systemd-run`, which otherwise expands `${VAR}` in arguments (a `dpkg-query -f '${Package}'` printed blank lines).

## 6. Routing inside the executor

`Executor` gains a second worker of the same type (`sandbox`, `admin`). `execute()` classifies as before, then picks the lane:

- `install`, `remove`, `service`, `make_dir` → admin;
- `read_file`/`write_file`/`edit_file` whose path resolves outside the workspace **and** is approved → admin;
- everything else → sandbox (`fetch_packages` included; `read_file` under `/etc` included);
- `set_setting` never reaches a worker: the engine applies it and calls the executor's log-only door so the action log still shows it.

A worker's `Outcome` now carries an optional undo entry; the engine saves it on the job's undo rows. Tests use two fake workers and assert which one each action reached.

Before any of that, `execute()` refuses a `run_command` that is really another hand's job — a package manager asked to install, remove, upgrade or update (`rules::wrong_hand`) — with the hand that does it named in the reason, and it never reaches a worker. Prose in the prompt was not enough: §11's Results has the run where nine steps of `apt-get` convinced the model the machine has no root. Read-only uses of the same programs stay free.

## 7. Housekeeping

A third front-door move: `housekeep { goal, understood, remember? }`. It creates a job with **no project**: `Job.project` is empty and `Job.housekeeping` is true. Its folder is `/data/housekeeping` (a plain folder the setup script creates, `ai:ai-sandbox` 2770), so `run_command`, `read_file` and `write_file` still work for scratch and inspection; the sandbox jail already shows `/etc` read-only and `systemctl is-enabled`, `apt list --installed` and `cat /etc/fstab` work inside it (proven). Four places branch on `housekeeping`:

1. the `done` gate: no blueprint rule at all (a scratch `write_file` must not demand a `BLUEPRINT.md`);
2. `job_turn`: no "(no blueprint yet…)" line; instead "This is housekeeping on the machine itself: no project, no blueprint; anything that must outlive this job is a setting (`set_setting`)";
3. `write_or_wipe_last_run`: never written (there is no project to read it from);
4. the project snapshot at job start: none.

**Settings in v1: one key, `projects_root`.** The value must be an absolute path under `/data` or `/home/ai` with no `..` (the same roots the wrapper allows), otherwise the action fails with the reason. The engine reads it from the store when it creates a project; the constructor's path is the default when unset. Existing projects keep working because every `Job` now carries its `folder` (copied from the project row at start; `serde(default)` for old rows) and the engine's `workspace()` takes the job, not a name; re-starting an existing project keeps its stored folder instead of recomputing it from the root (a live 1b bug the path check found). An unknown key is rejected with the list of known keys.

The front door's legal moves become `reply`, `start`, `housekeep`. The prompt tells the model: "the machine's own layout, settings and installed tools are housekeeping, not a project."

## 8. The blueprint gate, made absolute for new projects

Carry-forward from the 1b review. Today a job can end `done` with no blueprint if nothing was written through `write_file`/`edit_file` (a `sed -i` via `run_command` is invisible to the gate). New rule: when a project was **created by this job** (`Job.new_project`), `done` is rejected unless `BLUEPRINT.md` exists on disk in the job's folder at that moment. The existing "blueprint older than the last code change" rule stays for every project. Housekeeping is exempt (§7). The core's scripted test worker now really writes `write_file` into the temp folder, so this gate is exercised by the same tests that already write a blueprint.

## 9. Prompt changes

`SYSTEM` gains: software is installed with `install`, never with `run_command apt`; services with `service`; language packages come through `fetch_packages` because the sandbox has no network; a file outside the project needs the user's yes, so say why; the machine's own layout and settings are housekeeping. The schema (`schema.rs`) gains the six action shapes and the `housekeep` move, discriminator first (key order is load-bearing, 1b finding). `Machine:` line in `job_turn` gains "apt via install; pip/npm/cargo via fetch_packages".

The live acceptance added more, each after watching the 9B walk past the rule it needed (§11 Results): a folder outside the working directory is made with `make_dir`, never `run_command mkdir`; the project's own files are named relative to the working directory, never by an absolute path; a step that already succeeded is not repeated; and the housekeeping header forbids a `BLUEPRINT.md` by name, warns that the sandbox's blindness outside the scratch folder limits checking and never doing, and — **only when the user's request is about where projects live from now on** — spells the recipe out in order: make_dir the folder, `set_setting projects_root`, then `done` with `make_dir` (or `run_command ls`) as the check, and the job is not done until the setting is set.

The job also carries **`Job.request`, the user's message word for word**, and `job_turn` prints it right under the goal ("The user asked (verbatim): …"). `Goal` is the model's own paraphrase from the front door and it drops what it did not think mattered; a job cannot act on what it never saw (§11 Results finding 9). An ordered recipe keyed on a paraphrase is worth nothing — this is what makes the conditional one reachable.

## 10. Changes to the parent spec (applied at close-out)

- §4.8 Undo: "one snapshot disk" becomes "per-hand undo: btrfs snapshot per project subvolume for files; recorded reverse for every admin operation; unit of undo is one job; reversed on the word *undo*". The disk sentence stays as where projects live.
- §4.7 Executor: the admin worker's fixed menu is §5's verb list; approved outside-workspace file operations run there; `/etc` writes are never free.
- §11: Phase 1c status, the sudoers narrowing done, and the carry-forwards left (§12).

## 11. How 1c is proven

Unit tests (no machine): every new action parses and round-trips; every classify rule in §3; routing to the right fake worker; the log-only door; `$` doubling; the resolver and registry list per manager; undo rows saved from outcomes and applied in reverse with failures reported; `housekeep` creates a job with no project, no blueprint gate, no `LAST_RUN.md`; `set_setting` rejects unknown keys and bad paths and moves new projects; the absolute blueprint gate; the `undo` word list with negations; the schema's discriminator order and the allowed lists.

Machine tests (`ai-os` distro, gated by `AI_OS_SANDBOX_IT=1` like the existing sandbox tests, plus a shell test for the wrapper): the wrapper refuses an Essential package, an option-shaped package name, a protected service, a unit path as a service name, a path outside its roots, a refused file, and an unknown verb; `sandbox-run --net=none` still has no network; `--net=<ips>` reaches exactly those addresses and not `example.com`; `$` survives argv; unprivileged `btrfs subvolume create` + `snapshot -r` + restore under `/data/projects` brings a deleted file back; `python3 -m venv` works in the jail.

Live acceptance, no human, qwen3.5:9b, in this order:
1. "Prepare a folder at /data/work where all my projects will live from now on" → a housekeeping job, `make_dir` + `set_setting projects_root`, done with a check that the folder exists; a later "make me a primes script" lands under `/data/work`.
2. "Install cowsay and prove it works" → `install cowsay`, done with a `run_command cowsay` check.
3. "undo" → cowsay is gone; "undo" again → `/data/work` is removed if empty and `projects_root` is back to the default.
4. A pip fetch inside a project ("make a script that prints a table with the `tabulate` package") → `fetch_packages pip tabulate` succeeds with the allowlist; the same step through `--net=none` fails, proving the gate.

### Results (2026-09-16)

All of it inside the `ai-os` distro as user `ai`, `runtime/` as the working directory, Ollama up with `qwen3.5:9b`.

`cargo test 2>&1 | grep -E "^( +Running|running|test result|   Doc-tests)"` — everything; the two live tests skip without `AI_OS_LIVE`, and the build is warning-free:

```
     Running unittests src/lib.rs (target/debug/deps/aios_core-2698f216674a1c5b)
running 86 tests
test result: ok. 86 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.48s
     Running unittests src/main.rs (target/debug/deps/ai_os_chat-e1f7f512f1c84eb3)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/live_1c.rs (target/debug/deps/live_1c-0237a372fc8c595b)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/live_primes.rs (target/debug/deps/live_primes-57f7390f04ae5e82)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/snapshot_it.rs (target/debug/deps/snapshot_it-19313ce4fcec988e)
running 1 test
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running unittests src/lib.rs (target/debug/deps/executor-8e29a400eeb57364)
running 64 tests
test result: ok. 64 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.05s
     Running unittests src/main.rs (target/debug/deps/executor-8f54303fc6d950d5)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/admin_integration.rs (target/debug/deps/admin_integration-f141a0a17a058a3c)
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/sandbox_integration.rs (target/debug/deps/sandbox_integration-16f0894823b99626)
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests aios_core
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
   Doc-tests executor
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`AI_OS_SANDBOX_IT=1 cargo test -p executor` (the machine tests really run), same filter:

```
     Running unittests src/lib.rs (target/debug/deps/executor-7780159ae869c28d)
running 64 tests
test result: ok. 64 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.18s
     Running unittests src/main.rs (target/debug/deps/executor-424083cac5610998)
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
     Running tests/admin_integration.rs (target/debug/deps/admin_integration-a0145212066ea07c)
running 7 tests
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 5.05s
     Running tests/sandbox_integration.rs (target/debug/deps/sandbox_integration-aaba29069491d76f)
running 9 tests
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.43s
   Doc-tests executor
running 0 tests
test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

`AI_OS_SANDBOX_IT=1 cargo test -p aios-core --test snapshot_it`:

```
running 1 test
test a_project_subvolume_snapshots_and_restores_as_the_ai_user ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.34s
```

`bash runtime/admin/test-admin.sh` — 36 checks, every one PASS, exit 0:

```
PASS unknown verb
PASS essential remove
PASS runtime remove
PASS option as package
PASS bad package name
PASS apt suffix as package
PASS trailing dash
PASS unit path as service
PASS protected service
PASS protected unit suffix
PASS path outside roots
PASS sudoers write
PASS shadow read
PASS read outside roots
PASS empty path
PASS relative path
PASS make-dir outside roots
PASS sandbox cwd outside
PASS symlink write
PASS symlink read
PASS symlink target untouched
PASS pkg-list has bash
PASS service state
PASS write/read file
PASS remove-file
PASS existing mode kept
PASS new parent dir owner
PASS make-dir owner
PASS remove-dir
PASS sandbox-run uid
PASS net=none blocked
PASS dollar survives
PASS games on PATH
PASS allowlist reaches pypi
PASS allowlist blocks example.com
PASS venv works in jail
```

**Live acceptance** — `AI_OS_LIVE=1 cargo test -p aios-core --test live_1c -- --nocapture`, fresh `/data/ai-os-live-1c.db`, `/data/work` cleared and asserted gone first, no human: the test answers "You decide." to a question (printed in the transcript) and **never** approves anything — a job that waits for an approval fails the run, because all four scripts are Auto work.

**All four scripts passed, 346.90s in total** (2026-09-16, after the review round):

| script | steps | seconds | what was asserted on the machine |
|---|---|---|---|
| 1a the projects folder | 6 | 28.7 | `/data/work` exists and `projects_root` = `/data/work` |
| 1b a project under it | 5 | 37.6 | `/data/work/prime-script/BLUEPRINT.md` exists |
| 2 install cowsay | 8 | 107.7 | `pkg-list` through the wrapper contains `cowsay` |
| 3 undo ×3 | — | 2.7 / 0.1 / 0.1 | cowsay gone, the project's files really moved, `projects_root` back to unset |
| 4 fetch a pip package | 4 | 169.6 | a `fetch_packages` row in the action log whose outcome starts `ok:` |

The undo of the files is proven, not taken on trust: the test writes `UNDO_MARKER.txt` into the project after script 1b, deletes it from disk before the first undo, and the restore brings it back out of the snapshot — `before the undo: ["BLUEPRINT.md", "primes.py"]` → `after the restore: ["BLUEPRINT.md", "UNDO_MARKER.txt", "primes.py"]`.

The reversal report, verbatim: "Removed the 1 packages installed (cowsay)" / "Restored the files of /data/work/prime-script from before the job" / "Cleared setting projects_root" / "Could not undo: Removed the folder /data/work — left as is (rmdir: failed to remove '/data/work': Directory not empty)". That last line is §11's "removed **if empty**" being honest: undo restores a project's files but never removes the project folder, so what the primes job left inside `/data/work` keeps the folder alive. After the run there is no cowsay and no `projects_root`; `/data/work/prime-script` and `/data/projects/primes` (the latter from the 1b live test) are the project folders left.

**Two things the passing run shows that are worth reading as they are.** The second undo said *"Could not undo: Restored the files of /data/work/prime-script … (btrfs property set ro false: ERROR: Could not open: No such file or directory)"* — the model had done the cowsay work *inside* the primes project, so that job's snapshot replaced the primes job's (only the newest per project is kept, §12) and the older job's files could not be put back. The ceiling is known; the message is how it looks from the front. And script 4's fetch succeeded on its first step, but the job then gave up after six replans without a finished script: the acceptance asserts the fetch, which is what §11 asks of that script, not that the 9B finishes the table.

**What the live runs changed** — eighteen runs in all, and every failure named something missing in the product, never in the test:

1. **`mkdir` instead of `make_dir`.** The model ran `run_command mkdir -p /data/work`, read the jail's "Read-only file system" as the machine's truth, and gave up on a machine "without root". `SYSTEM` names `make_dir`, and — after the model reached for `mkdir` again anyway — the executor refuses `mkdir`/`rmdir` on an absolute path and names the hand instead (`rules::wrong_hand`).
2. **A blueprint in a housekeeping job.** Told only "no project, no blueprint", it wrote and then re-read a `BLUEPRINT.md` in the folder it had just made, burning the job on approvals. The housekeeping header forbids it by name.
3. **Absolute paths for the project's own files.** Inside its project it wrote `/data/work/BLUEPRINT.md` — one level above its workspace — so every write needed an approval and nothing landed in the project. `SYSTEM` now says the project's own files are named relative to the working directory.
4. **`apt` as a free command.** Nine steps of `apt-get`, `pip3`, `ensurepip`, all failing with permission errors that convinced it the machine has no root. `rules::wrong_hand` refuses a package manager's changing verbs (and looks past a leading `sudo`/`env`) before they reach the sandbox, naming the hand that does the job; read-only uses (`apt list --installed`, `cargo build`) stay free. The next run installed cowsay through the `install` hand on the step right after the refusal.
5. **`/usr/games` was not on the sandbox's PATH**, so the cowsay it had just installed could not be run to prove it. The wrapper sets PATH for `sandbox-run` now, with a check in `test-admin.sh`.
6. **A successful `make_dir` said nothing.** The wrapper prints nothing, so the step's detail was empty and the model went looking for other proof. The outcome now reads `/data/work exists`.
7. **The jail's blindness read as the machine's truth.** `ls -ld /data/work` answers "No such file or directory" because `/data` is an empty tmpfs inside the jail. A failed `run_command` whose arguments name a path under the AI roots that really exists now carries that fact in its detail (`worker::hidden_note`).
8. **A workspace-relative program never ran.** `.venv/bin/python3 prime-script.py` came back "Failed to find executable": systemd resolves a unit's program against `/` (§13 item 14), which the fetch step already worked around privately. `worker::absolute_program` now does it for `run_command` too — a program with a slash resolves against the workspace, a bare name stays on PATH, exactly like a shell.
9. **The front door summarises the standing half of a request away — and that was the root cause of the last six failures.** "Prepare a folder where all my projects will live from now on" reached the job as "create a folder at /data/work for project storage", so the job never saw the part that asks for a setting, and no rule keyed on the goal could reach it. Fixed at the cause: `Job.request` carries the user's message word for word and `job_turn` shows it right under the goal as "The user asked (verbatim): …". Telling the front door to keep the whole request was not enough on its own; carrying the words is.

10. **Only the newest snapshot per project is kept, and a second job in the same project consumes it.** The model installed cowsay *inside* the primes project, which took a second snapshot and pruned the first, so undoing the older job answered "Could not undo: Restored the files of … (btrfs property set ro false: ERROR: Could not open: No such file or directory)". The ceiling is the one already carried in §12; what the run adds is how it reads from the front, and that the acceptance must not assume which snapshot a restore puts back.

Still true of the 9B and recorded rather than papered over: it asks a question or two even in creative mode (the test answers "You decide." once), and it will repeat an identical failing *check* up to the three-strike cap — a check is deliberately exempt from the identical-action dedup (engine `perform`'s `is_check`, and the 1b test that pins it), so a check that keeps failing with nothing changed in between still costs a job three strikes.

## 12. Out of scope, carried forward

- 1d: everything about showing this in the rail (undo as a button, the reversal report as a card).
- Moving existing projects when `projects_root` changes (they keep their folder); converting pre-1c plain-folder projects to subvolumes.
- Undo across more than the most recent jobs one at a time; undo of a job while another is open.
- Snap and Flatpak packages; apt only in v1.
- Restarting the service that owns a config file after an approved edit (the model may follow with `service … restart`).
- The privileged daemon (packaging phase) replaces the wrapper; the verb list is its interface.
- Other Windows drive letters under `/mnt` (only `/mnt/c` is hidden; the workshop has one drive).
- `sudo -u <user> apt …` still launders past `wrong_hand`: it is a word list over the argv, and `-u` takes a value that is not an option, so the program it finds is the user name. The refusal is a teaching aid, not a security boundary — the wrapper is what actually decides what may run as root.
- Small 1b carry-forwards still open: `allowed_moves` ↔ `run_turns` as one table; HTTP error flattening; unbounded standing instructions.

## 13. Path check (2026-09-16, read-only agent against the live distro) — what changed

1. `IPAddressDeny/Allow` proven real on the WSL kernel (BPF cgroup filtering); `InaccessiblePaths=-/mnt` broke DNS → `-/mnt/c`; systemd expands `$` in argv → doubled.
2. `python3-venv`, `npm`, `cargo` absent in the distro → setup installs them.
3. Unprivileged btrfs create/snapshot/restore all work as `ai` once `/data/snapshots` is theirs → no root verb for project restore.
4. apt accepts options as package names and `systemctl enable` takes a unit path → name regex + `--`; apt removes reverse-dependencies → undo by set difference; `DPkg::Lock::Timeout` against the daily timers.
5. Root reads of `/etc` would expose `shadow`/keys → unprivileged reads instead.
6. Root `make-dir` produced `root:root` folders that block project creation → chown in the wrapper; `projects_root` unvalidated → same roots as the wrapper.
7. Free `/etc` writes = root code execution through unit files, cron, apt sources, PAM → NeedsConfirm, as in 1b.
8. Housekeeping would have been unreachable through the existing `done` gate and prompt text; `LAST_RUN.md` would land in the projects root; re-starting a project overwrote its folder; cancelled jobs were not undoable; the absolute gate needed the test worker to write real files. All folded into §7, §8.

**Build-time findings (2026-09-16), from writing the wrapper and the workers:**

9. `install -d` on an existing folder **resets its mode and owner**, so `make-dir` creates only what is missing and never re-stats a folder that is already there.
10. `write-file` wrote through a temporary file next to the target; a symlink planted at that temporary name between the create and the rename would have written wherever it pointed. Closed with an exclusive create (`set -C` / `O_EXCL`) on the temporary name.
11. apt accepts a **`pkg-` suffix** as "remove this package" inside an `install` line, so the name regex refuses a trailing dash as well as a leading one.
12. `systemctl` maps a bare name to `<name>.service`, so a protected-service list matched on the bare name was bypassed by writing `ssh.service`. Only the `.service` suffix is normalised before the check; any other unit suffix is refused outright.
13. A **relative path** is refused at both choke points — in Rust before the call and in the wrapper — because `realpath -m` resolves it against the *caller's* cwd, which is not what anyone approved.
14. systemd resolves a unit's program against `/`, not against `--working-directory`, so `sandbox-run` passes an absolute program path (and the fetch commands are built accordingly).
15. A plain folder on **tmpfs can carry inode 256**, which made the subvolume check try to snapshot `/tmp`. The check needs the device half too: inode 256 *and* a different device number from its parent.
16. Hiding `/mnt/c` alone is not enough on this workshop — every Windows drive and the WSL plumbing live under `/mnt`. The jail mounts an empty read-only tmpfs over all of `/mnt` and binds back the single file `/etc/resolv.conf` points at, or an allowlisted fetch could never resolve a registry name.
