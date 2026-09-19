# Full Access Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The AI runs with root through `sudo`, the whole network and every folder, never asks, has no Undo and no mouse stop (v0.8.0).

**Architecture:** One real worker for commands and files (`MachineWorker`) replaces `SandboxWorker` and `AdminWorker`; the executor keeps two lanes (machine, desktop) plus the engine's own. Everything that existed to confine, ask or undo is deleted end to end: executor rules/admin/undo, engine approval and undo flows, store undo rows, project snapshots, proto/rail approval and undo UI, the root helper script, the `ai-sandbox` user's job.

**Tech Stack:** Rust workspace `runtime/` (core, executor, proto, rail), bash installer `install/`. No local toolchain: every test run is CI — push the branch, `gh workflow run build.yml --ref full-access`, `gh run watch`.

**Spec:** `docs/superpowers/specs/2026-09-19-full-access-design.md`

## Global Constraints

- Never ask: no action is ever held for a yes.
- `run_command` runs as the owner in the job folder, network on, `sudo -n` available, stdin closed, `DEBIAN_FRONTEND=noninteractive`, killed after 1800 s (`timeout 1800`).
- File actions take any absolute path or one relative to the job folder; a write the owner cannot make goes through `sudo -n tee`.
- Loop guards and budgets in `engine.rs` stay; `asks_permission` send-back stays.
- Help guide (`rail/src/guide.txt`) changes in the same release.
- Tests that pinned confinement are deleted, not bent.

---

### Task 1: Executor — one machine worker, no rules, no admin, no undo

**Files:**
- Modify: `runtime/executor/src/worker.rs` (replace `SandboxWorker` with `MachineWorker`; drop `undo` from `Outcome`, `reverse` from `Worker`, `fetch`, `hidden_note`, `absolute_program`, `sandbox_args`, their tests)
- Modify: `runtime/executor/src/executor.rs` (lanes `Machine | Desktop | Engine`; no `classify`, `wrong_hand`, `Blocked`, `reverse`, `log_text` only if still used)
- Modify: `runtime/executor/src/action.rs` (delete `HttpPost`, `Install`, `Remove`, `Service`, `MakeDir`, `FetchPackages`, `ServiceDo`, `Manager`, `valid_name`)
- Delete: `runtime/executor/src/rules.rs`, `runtime/executor/src/admin.rs`, `runtime/executor/src/undo.rs`, `runtime/admin/ai-os-admin`, `runtime/admin/test-admin.sh`, `runtime/executor/tests/admin_integration.rs`, `runtime/executor/tests/sandbox_integration.rs`
- Modify: `runtime/executor/src/lib.rs`, `runtime/executor/src/main.rs`

**Interfaces — Produces:**
- `pub struct MachineWorker { pub workspace: PathBuf }` implementing `Worker`
- `pub trait Worker { fn run(&self, action: &Action) -> Outcome; }`
- `pub struct Outcome { pub ok: bool, pub detail: String, pub image: Option<Vec<u8>> }`
- `pub enum ExecOutcome` is gone: `Executor::execute(&self, job_id, action) -> Result<Outcome, LogError>`
- `Executor::new(machine: W, desktop: W, log: ActionLog, workspace: PathBuf)`

- [ ] **Step 1: Write the worker's tests** (in `worker.rs` tests, on a temp dir):

```rust
fn temp_ws(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("aios-mw-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d); fs::create_dir_all(&d).unwrap(); d
}

#[test]
fn files_anywhere_relative_to_the_job_folder_or_absolute() {
    let ws = temp_ws("files"); let w = MachineWorker { workspace: ws.clone() };
    assert!(w.run(&Action::WriteFile { path: "a/b.txt".into(), contents: "x = 1\n".into() }).ok, "parents are made");
    let abs = ws.join("abs.txt").display().to_string();
    assert!(w.run(&Action::WriteFile { path: abs.clone(), contents: "hi".into() }).ok);
    assert_eq!(w.run(&Action::ReadFile { path: abs, from_line: None, lines: None }).detail, "hi");
    assert!(w.run(&Action::EditFile { path: "a/b.txt".into(), find: "x = 1".into(), replace: "x = 2".into() }).ok);
    assert_eq!(fs::read_to_string(ws.join("a/b.txt")).unwrap(), "x = 2\n");
    fs::remove_dir_all(&ws).unwrap();
}

#[test]
fn a_command_runs_in_the_job_folder_and_its_failure_keeps_the_tail() {
    let ws = temp_ws("cmd"); let w = MachineWorker { workspace: ws.clone() };
    let o = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "pwd".into()] });
    assert!(o.ok && o.detail.contains(ws.to_str().unwrap()), "{o:?}");
    let f = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "echo boom >&2; exit 3".into()] });
    assert!(!f.ok && f.detail.contains("exit 3") && f.detail.contains("boom"), "{f:?}");
    fs::remove_dir_all(&ws).unwrap();
}
```
Keep the existing `edit_file_replaces_exactly_one_match`, `edit_file_refuses_zero_and_many_matches`, `read_file_window_returns_only_those_lines`, head/tail and fake tests, pointed at `MachineWorker`.

- [ ] **Step 2: Write `MachineWorker`** in `worker.rs`:

```rust
/// Runs commands and file actions as the owner, anywhere, with the network on. Root is
/// `sudo -n`, granted without a password at install (full-access spec). The VM is the net.
pub struct MachineWorker { pub workspace: PathBuf }

/// A hung command (a GUI started from a shell, a prompt nobody answers) must not hold the job forever.
const COMMAND_SECS: &str = "1800";

impl MachineWorker {
    fn path(&self, p: &str) -> PathBuf { self.workspace.join(p) } // an absolute `p` replaces the base

    fn command(&self, argv: &[String]) -> Outcome {
        match Command::new("timeout").arg(COMMAND_SECS).args(argv).current_dir(&self.workspace)
            .env("DEBIAN_FRONTEND", "noninteractive").stdin(std::process::Stdio::null()).output() {
            Ok(o) => {
                let ok = o.status.success();
                let (stdout, stderr) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                let cut = |s: &str| if ok { head(s, 500) } else { tail(s, 500) };
                let code = o.status.code().unwrap_or(-1);
                let timed = if code == 124 { " (stopped after 30 min)" } else { "" };
                let detail = format!("exit {code}{timed}; stdout: {} stderr: {}", cut(&stdout), cut(&stderr));
                if ok { Outcome::ok(detail) } else { Outcome::err(detail) }
            }
            Err(e) => Outcome::err(format!("could not start {}: {e}", argv[0])),
        }
    }

    fn read(&self, p: &Path) -> Result<String, String> {
        match fs::read_to_string(p) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let o = Command::new("sudo").args(["-n", "cat", "--"]).arg(p).output().map_err(|e| e.to_string())?;
                if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).into_owned()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            }
            r => r.map_err(|e| e.to_string()),
        }
    }

    fn write(&self, p: &Path, contents: &str) -> Result<(), String> {
        let direct = p.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| fs::write(p, contents));
        match direct {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                use std::io::Write;
                if let Some(parent) = p.parent() {
                    let _ = Command::new("sudo").args(["-n", "mkdir", "-p", "--"]).arg(parent).status();
                }
                let mut c = Command::new("sudo").args(["-n", "tee", "--"]).arg(p)
                    .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped())
                    .spawn().map_err(|e| e.to_string())?;
                c.stdin.take().unwrap().write_all(contents.as_bytes()).map_err(|e| e.to_string())?;
                let o = c.wait_with_output().map_err(|e| e.to_string())?;
                if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            }
            r => r.map_err(|e| e.to_string()),
        }
    }
}

impl Worker for MachineWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => self.command(argv),
            Action::ReadFile { path, from_line, lines } => match self.read(&self.path(path)) {
                Ok(t) => Outcome::ok(window(&t, *from_line, *lines)),
                Err(e) => Outcome::err(e),
            },
            Action::WriteFile { path, contents } => match self.write(&self.path(path), contents) {
                Ok(()) => Outcome::ok("written"),
                Err(e) => Outcome::err(e),
            },
            // Rule 9: edit in place — quote an exact passage from a prior read and swap it.
            Action::EditFile { path, find, replace } => {
                let p = self.path(path);
                let text = match self.read(&p) { Ok(t) => t, Err(e) => return Outcome::err(e) };
                match text.matches(find.as_str()).count() {
                    0 => return Outcome::err("find text not found — re-read the file and quote it exactly"),
                    1 => {}
                    n => return Outcome::err(format!("find text occurs in {n} places — include more surrounding lines so it is unique")),
                }
                match self.write(&p, &text.replacen(find.as_str(), replace, 1)) { Ok(()) => Outcome::ok("edited"), Err(e) => Outcome::err(e) }
            }
            _ => Outcome::err("the machine hand has no hand for this action"),
        }
    }
}
```

- [ ] **Step 3: Executor** — replace `lane`/`execute` with:

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lane { Machine, Desktop, Engine }

pub fn lane(action: &Action) -> Lane {
    match action {
        Action::RunCommand { .. } | Action::ReadFile { .. } | Action::WriteFile { .. } | Action::EditFile { .. } => Lane::Machine,
        Action::SetSetting { .. } => Lane::Engine,
        Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. }
        | Action::ScreenLook { .. } | Action::ScreenClick { .. } | Action::ScreenType { .. } => Lane::Desktop,
    }
}

/// The one door: run it on its hand and log it. Nothing is ever held for a yes.
pub fn execute(&self, job_id: &str, action: &Action) -> Result<Outcome, LogError> {
    let worker = match lane(action) {
        Lane::Machine => &self.machine,
        Lane::Desktop => &self.desktop,
        Lane::Engine => {
            let reason = "engine action reached the executor";
            self.log.append(job_id, action, &format!("error: {reason}"))?;
            return Ok(Outcome::err(reason));
        }
    };
    let outcome = worker.run(action);
    let tag = if outcome.ok { "ok" } else { "error" };
    if let Err(e) = self.log.append(job_id, action, &format!("{tag}: {}", outcome.detail)) {
        eprintln!("executor: failed to log action outcome: {e}");
    }
    Ok(outcome)
}
```
Tests kept: `auto_action_runs_and_logs` (now `execute("j1", &a)`), `set_setting_never_reaches_a_worker_but_log_only_records_it`, `an_engine_action_that_reaches_execute_is_refused_not_run`, `desktop_actions_take_the_desktop_lane`, plus a new one that `Press{name:"Close"}` and `ScreenClick{name:"Send"}` run at once. Delete the rest.

- [ ] **Step 4: Delete** the files listed above; fix `lib.rs` `mod` lines and `main.rs`; `atspi.rs` loses its risk-word use if any and keeps `find_app`.
- [ ] **Step 5: Commit** `feat(executor): one machine hand with root, no rules, no admin lane, no undo`.

### Task 2: Screen — no mouse stop

**Files:** Modify `runtime/executor/src/screen.rs` (delete `TOOK_OVER`, `took_over`, the idle read and its check in `run`, its tests), `runtime/core/src/engine.rs` (delete the `TOOK_OVER` cancel branch).

- [ ] **Step 1:** Delete them; keep `png_size`, `keysym`, `click_point`, the zoom-before-click rule.
- [ ] **Step 2: Commit** `feat(screen): the AI keeps working while the person uses the mouse`.

### Task 3: Core — no asking, no undo, no snapshots

**Files:**
- Modify `runtime/core/src/engine.rs`: delete `State::WaitingApproval` handling, `is_yes`/`is_refusal` (if only used there), `resume_after_approval`, `approval_question` use, `approved` parameter of `perform`, `declined_actions`/`approved_actions` checks, `ExecOutcome::Blocked` branch, `is_undo`, `undo_last`, snapshot at job start (keep `snapshot::create_project_dir` for the new project folder), `snapshotter`/`snapshots_dir` fields and `with_snapshotter`, `add_undo` calls, `SetSetting`'s `with_undo`. `set_setting projects_root` accepts any absolute path.
- Modify `runtime/core/src/job.rs`: delete `WaitingApproval`, `pending_action`/`pending_reason`, `approved_actions`, `declined_actions`, `ok_questions`.
- Modify `runtime/core/src/store.rs`: delete the undo table functions (`add_undo`, `unapplied_undo`, `mark_undo_applied`, `last_undoable_job`); leave the table in old databases alone.
- Modify `runtime/core/src/snapshot.rs`: keep only `create_project_dir` (a plain `create_dir_all` if btrfs subvolumes were only for snapshots) and delete `share_with_sandbox`, `Snapshotter`, `RealSnapshotter`, `is_subvolume`.
- Modify `runtime/core/src/schema.rs`: drop the removed action kinds.
- Modify `runtime/core/src/prompt.rs`: rewrite lines 10, 18–22, 24–25, 130 per spec §7; delete `approval_question`.
- Modify `runtime/core/src/event.rs`, `testing.rs`, `bin/ai-os-engine.rs` (build `MachineWorker`), `learn.rs`.
- Delete `runtime/core/tests/snapshot_it.rs`; fix `core/tests/*.rs` that use removed items.

New prompt lines (replace the old ones verbatim):

```
- You have full access to this computer: every folder, the whole network, and root through `sudo` (never asks for a password). Nobody is asked before you act — just do it.
- Install, remove and set up software with run_command: `sudo apt-get install -y …`, `sudo systemctl …`, `pip`, `npm`, `cargo`. Make folders with `mkdir -p`.
- read_file, write_file and edit_file take any absolute path, or one relative to the working directory; a file only root may write is written for you.
```

Tests: new engine test `nothing_is_ever_held_for_a_yes` — a script with `act(1, Action::Press{control:2,name:"Delete".into()})` then `done(...)` finishes with the press in `rec.desktop_calls` and no `NeedsOk`/`Waiting::Ok` event; prompt test asserts `SYSTEM.contains("sudo")` and `!SYSTEM.contains("sandbox")`. Delete the approval, undo, snapshot and took-over tests.

- [ ] **Step 1:** Write the two new tests. **Step 2:** Make the removals. **Step 3:** Commit `feat(core): never ask, no undo, no snapshots; the prompt says full access`.

### Task 4: Proto and rail — no approval card, no Undo

**Files:** `runtime/proto/src/lib.rs` (delete `Waiting::Ok`, `Event::NeedsOk` if present, `Event::Undone`, `UndoLine`, any `Command::Undo`/approve variants), `runtime/rail/src/cards.rs`, `runtime/rail/src/main.rs`, `runtime/rail/tests/cards.rs`, `runtime/rail/src/guide.txt`.

- [ ] **Step 1:** Delete the approval card and the Undo button/card and their tests.
- [ ] **Step 2:** Guide: remove the Undo and "it asks before…" passages; add: "The AI has full access to this computer — it installs, changes and deletes without asking. Take a VirtualBox snapshot before big jobs; that is the way back."
- [ ] **Step 3: Commit** `feat(rail): no approval card, no Undo; the guide says full access`.

### Task 5: Installer — root for the owner, no helper

**Files:** `install/install.sh`, `install/check.sh`, `install/make-release.sh` (stop shipping `ai-os-admin`), `trial/setup-admin.sh` if it installs the helper.

- [ ] **Step 1:** Replace the sudoers line with `<owner> ALL=(ALL) NOPASSWD: ALL` in `/etc/sudoers.d/ai-os` (validated with `visudo -cf`); remove `/usr/local/libexec/ai-os-admin` on upgrade; keep `/data` and its folders owned by the owner (drop the `ai-sandbox` group requirement from `check.sh`; leave the user in place on existing machines).
- [ ] **Step 2: Commit** `feat(install): the owner's AI gets passwordless root; the helper is gone`.

### Task 6: CI, docs, release

- [ ] **Step 1:** Push, `gh workflow run build.yml --ref full-access`, fix until green.
- [ ] **Step 2:** Main design status (`2026-09-15-ai-os-design.md` last section) gets a v0.8.0 line pointing at the full-access spec. README if it mentions Undo or approvals.
- [ ] **Step 3:** Merge to master, tag `v0.8.0`, push.
