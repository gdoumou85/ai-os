# Executor Spine Implementation Plan (Phase 1a)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the safety-critical core every AI action routes through — a Rust executor that classifies each structured action by the risky-actions rule, runs safe ones through a sandbox worker, and logs every action to SQLite. No model, no UI, no admin/desktop workers yet.

**Architecture:** The model (later) emits JSON `Action` values. The `Executor` is a pure dispatcher: it classifies an action as `Auto` or `NeedsConfirm(reason)` by *type* (never by asking the model), refuses unconfirmed risky actions, hands the rest to a `Worker`, and appends the action plus its result to an SQLite log tied to a job id. Classification and dispatch are pure and unit-tested with a fake worker; the real `SandboxWorker` (separate Linux user via `systemd-run`, workspace-locked, no network) is proven by one integration test gated to run inside the distro.

**Tech Stack:** Rust (edition 2021), `serde`/`serde_json` (action JSON), `rusqlite` (bundled SQLite), `thiserror` (error types). Sandbox via `systemd-run --user`. Cargo workspace at `runtime/`.

**Spec:** `../specs/2026-09-15-ai-os-design.md` (§4.7 executor, §4.10 stack, decision 9 risky actions, decision 12 no raw admin shell)

## Global Constraints

- **Language: Rust**, one self-contained binary; no interpreter runtime shipped (spec §4.10).
- **The model is never asked whether its own action is risky** — classification is code, a pure function of the typed action (decision 9, §4.7).
- **No shell strings are ever executed** — actions map to coded handlers; `RunCommand` passes argv as a list, never a shell line (decision 12).
- **Sandbox worker** runs as a *separate Linux user* via `systemd-run --user` scoped limits (not Docker), cwd locked to the job workspace, no network (spec §4.7, §4.10).
- **State is SQLite**, one file, survives restarts (§4.1, §4.10).
- All new code lives under `runtime/`. Tests run with `cargo test`; the sandbox integration test is gated behind `AI_OS_SANDBOX_IT=1` and run inside the `ai-os` distro.

---

### Task 1: Cargo workspace + executor crate skeleton

**Files:**
- Create: `runtime/Cargo.toml`
- Create: `runtime/executor/Cargo.toml`
- Create: `runtime/executor/src/lib.rs`

**Interfaces:**
- Produces: the `executor` library crate that every later task extends.

- [ ] **Step 1: Create the workspace manifest**

`runtime/Cargo.toml`:
```toml
[workspace]
members = ["executor"]
resolver = "2"
```

- [ ] **Step 2: Create the crate manifest**

`runtime/executor/Cargo.toml`:
```toml
[package]
name = "executor"
version = "0.0.1"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled"] }
thiserror = "1"
```

- [ ] **Step 3: Create the lib root with the module list**

`runtime/executor/src/lib.rs`:
```rust
pub mod action;
pub mod rules;
pub mod log;
pub mod worker;
pub mod executor;
```

- [ ] **Step 4: Verify it compiles**

Run: `cd runtime && cargo build`
Expected: FAIL — the modules do not exist yet ("file not found for module `action`"). This confirms the module wiring; the next tasks create each file.

- [ ] **Step 5: Commit**

```bash
git add runtime/Cargo.toml runtime/executor/Cargo.toml runtime/executor/src/lib.rs
git commit -m "feat(executor): workspace + crate skeleton"
```

---

### Task 2: The Action type

**Files:**
- Create: `runtime/executor/src/action.rs`

**Interfaces:**
- Produces: `enum Action` with serde JSON (de)serialization. Variants for the spine: `RunCommand { argv: Vec<String> }`, `ReadFile { path: String }`, `WriteFile { path: String, contents: String }` (workspace-scoped by classification), and one that leaves the machine to prove the rule: `HttpPost { url: String, body: String }`.

- [ ] **Step 1: Write the failing test**

`runtime/executor/src/action.rs`:
```rust
use serde::{Deserialize, Serialize};

/// A structured action the model proposes. Tagged JSON: {"kind":"run_command","argv":[...]}.
/// Adding a variant here is the only way to give the AI a new kind of hand.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    RunCommand { argv: Vec<String> },
    ReadFile { path: String },
    WriteFile { path: String, contents: String },
    HttpPost { url: String, body: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_run_command() {
        let a: Action = serde_json::from_str(r#"{"kind":"run_command","argv":["ls","-la"]}"#).unwrap();
        assert_eq!(a, Action::RunCommand { argv: vec!["ls".into(), "-la".into()] });
    }

    #[test]
    fn rejects_unknown_kind() {
        let r: Result<Action, _> = serde_json::from_str(r#"{"kind":"format_disk"}"#);
        assert!(r.is_err(), "unknown action kinds must not deserialize");
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd runtime && cargo test -p executor action::`
Expected: PASS (the type and tests are in the same step because the type *is* the implementation).

- [ ] **Step 3: Commit**

```bash
git add runtime/executor/src/action.rs
git commit -m "feat(executor): Action type with tagged JSON"
```

---

### Task 3: Risky-action classification (decision 9, as code)

**Files:**
- Create: `runtime/executor/src/rules.rs`
- Test: same file, `#[cfg(test)]`

**Interfaces:**
- Consumes: `Action` (Task 2).
- Produces: `enum Risk { Auto, NeedsConfirm(String) }` and `fn classify(action: &Action, workspace: &Path) -> Risk`. This is the pure function decision 9 requires — later tasks call it, never re-implement it.

- [ ] **Step 1: Write the failing test**

`runtime/executor/src/rules.rs`:
```rust
use crate::action::Action;
use std::path::{Path, PathBuf};

/// The risky-actions verdict. `Auto` runs without asking; `NeedsConfirm` must be
/// approved by the user first. A pure function of the typed action (decision 9):
/// the model is never consulted.
#[derive(Debug, Clone, PartialEq)]
pub enum Risk {
    Auto,
    NeedsConfirm(String),
}

/// Resolve a possibly-relative path against the workspace, without touching disk.
fn resolves_inside(path: &str, workspace: &Path) -> bool {
    let p = Path::new(path);
    let joined: PathBuf = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
    // Reject any `..` escape by normalizing lexically.
    let mut norm = PathBuf::new();
    for c in joined.components() {
        use std::path::Component::*;
        match c {
            ParentDir => { if !norm.pop() { return false; } }
            CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    norm.starts_with(workspace)
}

pub fn classify(action: &Action, workspace: &Path) -> Risk {
    match action {
        // Sandbox-scoped and network-isolated by construction → reversible → Auto.
        Action::RunCommand { .. } => Risk::Auto,
        Action::ReadFile { .. } => Risk::Auto,
        // Writing inside the workspace is reversible; outside is "destroys work" territory.
        Action::WriteFile { path, .. } => {
            if resolves_inside(path, workspace) {
                Risk::Auto
            } else {
                Risk::NeedsConfirm(format!("writes outside the workspace: {path}"))
            }
        }
        // Leaves the machine → always ask (a snapshot cannot bring it back).
        Action::HttpPost { url, .. } => Risk::NeedsConfirm(format!("sends data off the machine to {url}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> PathBuf { PathBuf::from("/data/jobs/j1") }

    #[test]
    fn run_command_is_auto() {
        assert_eq!(classify(&Action::RunCommand { argv: vec!["ls".into()] }, &ws()), Risk::Auto);
    }

    #[test]
    fn write_inside_workspace_is_auto() {
        let a = Action::WriteFile { path: "out.txt".into(), contents: "x".into() };
        assert_eq!(classify(&a, &ws()), Risk::Auto);
    }

    #[test]
    fn write_outside_workspace_needs_confirm() {
        let a = Action::WriteFile { path: "/etc/passwd".into(), contents: "x".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn parent_dir_escape_needs_confirm() {
        let a = Action::WriteFile { path: "../../etc/x".into(), contents: "x".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn http_post_needs_confirm() {
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd runtime && cargo test -p executor rules::`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add runtime/executor/src/rules.rs
git commit -m "feat(executor): risky-action classification (decision 9)"
```

---

### Task 4: The action log (SQLite)

**Files:**
- Create: `runtime/executor/src/log.rs`

**Interfaces:**
- Consumes: `Action` (Task 2).
- Produces: `struct ActionLog` with `open(path) -> Result<ActionLog, LogError>`, `open_in_memory()`, and `append(&self, job_id: &str, action: &Action, outcome: &str) -> Result<i64, LogError>` returning the new row id. Schema: `jobs(id TEXT PRIMARY KEY, created_at INTEGER)`, `actions(id INTEGER PK, job_id TEXT, action_json TEXT, outcome TEXT, at INTEGER)`.

- [ ] **Step 1: Write the failing test**

`runtime/executor/src/log.rs`:
```rust
use crate::action::Action;
use rusqlite::Connection;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct ActionLog {
    conn: Connection,
}

impl ActionLog {
    pub fn open(path: &str) -> Result<Self, LogError> {
        Self::from_conn(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, LogError> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self, LogError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, created_at INTEGER);
             CREATE TABLE IF NOT EXISTS actions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                action_json TEXT NOT NULL,
                outcome TEXT NOT NULL,
                at INTEGER NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    pub fn ensure_job(&self, job_id: &str) -> Result<(), LogError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO jobs(id, created_at) VALUES (?1, ?2)",
            (job_id, Self::now()),
        )?;
        Ok(())
    }

    pub fn append(&self, job_id: &str, action: &Action, outcome: &str) -> Result<i64, LogError> {
        self.ensure_job(job_id)?;
        let json = serde_json::to_string(action)?;
        self.conn.execute(
            "INSERT INTO actions(job_id, action_json, outcome, at) VALUES (?1, ?2, ?3, ?4)",
            (job_id, json, outcome, Self::now()),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn count_for_job(&self, job_id: &str) -> Result<i64, LogError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM actions WHERE job_id = ?1",
            [job_id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_counts() {
        let log = ActionLog::open_in_memory().unwrap();
        let a = Action::RunCommand { argv: vec!["ls".into()] };
        let id = log.append("j1", &a, "ok: exit 0").unwrap();
        assert!(id > 0);
        assert_eq!(log.count_for_job("j1").unwrap(), 1);
        assert_eq!(log.count_for_job("other").unwrap(), 0);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd runtime && cargo test -p executor log::`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add runtime/executor/src/log.rs
git commit -m "feat(executor): SQLite action log"
```

---

### Task 5: The Worker trait + a fake for tests

**Files:**
- Create: `runtime/executor/src/worker.rs`

**Interfaces:**
- Consumes: `Action` (Task 2).
- Produces: `struct Outcome { ok: bool, detail: String }`; `trait Worker { fn run(&self, action: &Action) -> Outcome; }`; `struct FakeWorker` (records calls, returns a preset outcome) for use by Task 6's tests.

- [ ] **Step 1: Write the failing test**

`runtime/executor/src/worker.rs`:
```rust
use crate::action::Action;
use std::cell::RefCell;

/// The result of actually performing an action.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub detail: String,
}

/// A thing that can perform actions. The spine ships one real impl (SandboxWorker,
/// Task 7); tests use FakeWorker.
pub trait Worker {
    fn run(&self, action: &Action) -> Outcome;
}

/// Records every action it was asked to run; returns a preset outcome.
pub struct FakeWorker {
    pub calls: RefCell<Vec<Action>>,
    pub outcome: Outcome,
}

impl FakeWorker {
    pub fn new(ok: bool) -> Self {
        Self { calls: RefCell::new(vec![]), outcome: Outcome { ok, detail: "fake".into() } }
    }
}

impl Worker for FakeWorker {
    fn run(&self, action: &Action) -> Outcome {
        self.calls.borrow_mut().push(action.clone());
        self.outcome.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_records_calls() {
        let w = FakeWorker::new(true);
        let a = Action::ReadFile { path: "x".into() };
        let out = w.run(&a);
        assert!(out.ok);
        assert_eq!(w.calls.borrow().len(), 1);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd runtime && cargo test -p executor worker::`
Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add runtime/executor/src/worker.rs
git commit -m "feat(executor): Worker trait + FakeWorker"
```

---

### Task 6: The Executor dispatcher (ties rule + worker + log together)

**Files:**
- Create: `runtime/executor/src/executor.rs`

**Interfaces:**
- Consumes: `Action` (2), `classify`/`Risk` (3), `ActionLog` (4), `Worker`/`Outcome` (5).
- Produces: `struct Executor<W: Worker>` holding a worker, a log, and a workspace path; `enum ExecOutcome { Ran(Outcome), Blocked(String) }`; `fn execute(&self, job_id: &str, action: &Action, approved: bool) -> Result<ExecOutcome, LogError>`. This is the single door every action goes through.

- [ ] **Step 1: Write the failing test**

`runtime/executor/src/executor.rs`:
```rust
use crate::action::Action;
use crate::log::{ActionLog, LogError};
use crate::rules::{classify, Risk};
use crate::worker::{Outcome, Worker};
use std::path::PathBuf;

#[derive(Debug, PartialEq)]
pub enum ExecOutcome {
    Ran(Outcome),
    Blocked(String),
}

pub struct Executor<W: Worker> {
    worker: W,
    log: ActionLog,
    workspace: PathBuf,
}

impl<W: Worker> Executor<W> {
    pub fn new(worker: W, log: ActionLog, workspace: PathBuf) -> Self {
        Self { worker, log, workspace }
    }

    /// The one door. Classify, refuse unconfirmed risky actions, run the rest, log everything.
    pub fn execute(&self, job_id: &str, action: &Action, approved: bool) -> Result<ExecOutcome, LogError> {
        match classify(action, &self.workspace) {
            Risk::NeedsConfirm(reason) if !approved => {
                self.log.append(job_id, action, &format!("blocked: {reason}"))?;
                Ok(ExecOutcome::Blocked(reason))
            }
            _ => {
                let outcome = self.worker.run(action);
                let tag = if outcome.ok { "ok" } else { "error" };
                self.log.append(job_id, action, &format!("{tag}: {}", outcome.detail))?;
                Ok(ExecOutcome::Ran(outcome))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::FakeWorker;

    fn exec(ok: bool) -> Executor<FakeWorker> {
        Executor::new(FakeWorker::new(ok), ActionLog::open_in_memory().unwrap(), PathBuf::from("/data/jobs/j1"))
    }

    #[test]
    fn auto_action_runs_and_logs() {
        let e = exec(true);
        let a = Action::RunCommand { argv: vec!["ls".into()] };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.worker.calls.borrow().len(), 1);
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1);
    }

    #[test]
    fn risky_action_without_approval_is_blocked_and_not_run() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Blocked(_)));
        assert_eq!(e.worker.calls.borrow().len(), 0, "blocked action must never reach the worker");
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1, "the block itself is logged");
    }

    #[test]
    fn risky_action_with_approval_runs() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, true).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.worker.calls.borrow().len(), 1);
    }
}
```

- [ ] **Step 2: Run the tests**

Run: `cd runtime && cargo test -p executor executor::`
Expected: PASS.

- [ ] **Step 3: Run the whole crate's tests**

Run: `cd runtime && cargo test -p executor`
Expected: all pass (action, rules, log, worker, executor).

- [ ] **Step 4: Commit**

```bash
git add runtime/executor/src/executor.rs
git commit -m "feat(executor): dispatcher enforcing rule + worker + log"
```

---

### Task 7: The real SandboxWorker (separate user + systemd-run, workspace-locked, no network)

**Files:**
- Modify: `runtime/executor/src/worker.rs` (add `SandboxWorker`)
- Create: `runtime/executor/tests/sandbox_integration.rs`
- Create: `trial/setup-sandbox-user.sh` (creates the `ai-sandbox` Linux user; run once, needs root)

**Interfaces:**
- Consumes: `Action`, `Outcome`, `Worker`.
- Produces: `struct SandboxWorker { user: String, workspace: PathBuf }` implementing `Worker`. `RunCommand` runs the argv as the sandbox user via `systemd-run --user`-style scope with `WorkingDirectory` = workspace, no network; `ReadFile`/`WriteFile` are performed by the executor process itself, path-checked against the workspace (they were already classified). Non-command actions that reach a sandbox worker (e.g. `HttpPost`) return `Outcome{ ok:false }` — the sandbox has no such hand.

- [ ] **Step 1: Write the sandbox user setup script**

`trial/setup-sandbox-user.sh`:
```bash
#!/usr/bin/env bash
# Create the unprivileged Linux user the sandbox worker runs commands as.
# Run once as root inside the distro. The user owns nothing outside job workspaces.
set -e
id ai-sandbox >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin ai-sandbox
install -d -o ai-sandbox -g ai-sandbox -m 0770 /data/jobs
echo "ai-sandbox ready"
```

- [ ] **Step 2: Write the SandboxWorker (and a failing integration test)**

Add to `runtime/executor/src/worker.rs`:
```rust
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Runs commands as an unprivileged user, scoped to one workspace, no network.
/// ponytail: shells out to `systemd-run`; a native cgroup/namespace impl only if this proves too slow.
pub struct SandboxWorker {
    pub user: String,
    pub workspace: PathBuf,
}

impl Worker for SandboxWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => {
                // Transient scope: unprivileged user, locked cwd, network cut off.
                let out = Command::new("systemd-run")
                    .args(["--quiet", "--pipe", "--wait", "--collect"])
                    .arg(format!("--uid={}", self.user))
                    .arg(format!("--working-directory={}", self.workspace.display()))
                    // No network; personal files hidden. System dirs are already unwritable
                    // to the unprivileged sandbox user, and the workspace stays writable
                    // (do NOT use ProtectSystem=strict — it would make the workspace read-only).
                    .args(["--property=PrivateNetwork=yes", "--property=ProtectHome=yes"])
                    .arg("--")
                    .args(argv)
                    .output();
                match out {
                    Ok(o) => Outcome {
                        ok: o.status.success(),
                        detail: format!(
                            "exit {}; {}",
                            o.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&o.stdout).chars().take(500).collect::<String>()
                        ),
                    },
                    Err(e) => Outcome { ok: false, detail: format!("spawn failed: {e}") },
                }
            }
            Action::ReadFile { path } => match fs::read_to_string(self.workspace.join(path)) {
                Ok(s) => Outcome { ok: true, detail: s.chars().take(500).collect() },
                Err(e) => Outcome { ok: false, detail: e.to_string() },
            },
            Action::WriteFile { path, contents } => match fs::write(self.workspace.join(path), contents) {
                Ok(_) => Outcome { ok: true, detail: "written".into() },
                Err(e) => Outcome { ok: false, detail: e.to_string() },
            },
            _ => Outcome { ok: false, detail: "sandbox has no hand for this action".into() },
        }
    }
}
```

`runtime/executor/tests/sandbox_integration.rs`:
```rust
// Gated: only runs inside the distro with the sandbox user set up.
//   AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration
use executor::action::Action;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

fn gated() -> bool { std::env::var("AI_OS_SANDBOX_IT").as_deref() == Ok("1") }

#[test]
fn command_runs_as_sandbox_user_in_workspace() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = PathBuf::from("/data/jobs/it1");
    std::fs::create_dir_all(&ws).unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    let out = w.run(&Action::RunCommand { argv: vec!["id".into(), "-un".into()] });
    assert!(out.ok, "detail: {}", out.detail);
    assert!(out.detail.contains("ai-sandbox"), "should run as the sandbox user: {}", out.detail);
}

#[test]
fn network_is_blocked() {
    if !gated() { return; }
    let ws = PathBuf::from("/data/jobs/it2");
    std::fs::create_dir_all(&ws).unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    // With PrivateNetwork, even loopback name resolution / connect should fail.
    let out = w.run(&Action::RunCommand { argv: vec!["getent".into(), "hosts".into(), "example.com".into()] });
    assert!(!out.ok, "network must be cut off in the sandbox: {}", out.detail);
}
```

- [ ] **Step 3: Verify the unit build still passes (integration test skips off-distro)**

Run: `cd runtime && cargo test -p executor`
Expected: PASS; the two integration tests print "skipped" when `AI_OS_SANDBOX_IT` is unset.

- [ ] **Step 4: Prove the sandbox inside the distro**

Run (inside `ai-os`):
```bash
sudo bash trial/setup-sandbox-user.sh
cd runtime && AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration
```
Expected: both pass — a command runs as `ai-sandbox`, and network is blocked.
If `--uid=` or `PrivateNetwork` is rejected under `--user` systemd, switch the invocation to system `systemd-run` (no `--user`) driven by the privileged executor; record the working invocation in the plan before committing.

- [ ] **Step 5: Commit**

```bash
git add runtime/executor/src/worker.rs runtime/executor/tests/sandbox_integration.rs trial/setup-sandbox-user.sh
git commit -m "feat(executor): SandboxWorker via systemd-run (separate user, no network)"
```

---

### Task 8: A tiny demo binary (the spine, exercised end to end without a model)

**Files:**
- Create: `runtime/executor/src/main.rs`
- Modify: `runtime/executor/Cargo.toml` (add `[[bin]]` if needed; a `main.rs` beside `lib.rs` is picked up automatically)

**Interfaces:**
- Consumes: the whole crate.
- Produces: a binary that reads a JSON action from argv, runs it through a real `Executor<SandboxWorker>` against a scratch workspace, and prints the outcome — the human-visible proof the spine works before any model exists.

- [ ] **Step 1: Write the demo binary**

`runtime/executor/src/main.rs`:
```rust
use executor::action::Action;
use executor::executor::{ExecOutcome, Executor};
use executor::log::ActionLog;
use executor::worker::SandboxWorker;
use std::path::PathBuf;

fn main() {
    let json = std::env::args().nth(1).expect("usage: executor '<action-json>'");
    let action: Action = serde_json::from_str(&json).expect("invalid action JSON");
    let ws = PathBuf::from("/data/jobs/demo");
    std::fs::create_dir_all(&ws).ok();
    let worker = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let log = ActionLog::open("/data/ai-os.db").expect("open log");
    let exec = Executor::new(worker, log, ws);
    match exec.execute("demo", &action, false).expect("execute") {
        ExecOutcome::Ran(o) => println!("RAN ok={} detail={}", o.ok, o.detail),
        ExecOutcome::Blocked(reason) => println!("BLOCKED: {reason}"),
    }
}
```

- [ ] **Step 2: Build**

Run: `cd runtime && cargo build`
Expected: PASS.

- [ ] **Step 3: Prove it inside the distro**

Run (inside `ai-os`, after Task 7's setup):
```bash
cd runtime
cargo run -p executor -- '{"kind":"run_command","argv":["echo","hello from the sandbox"]}'
# → RAN ok=true detail=exit 0; hello from the sandbox
cargo run -p executor -- '{"kind":"http_post","url":"https://example.com","body":"x"}'
# → BLOCKED: sends data off the machine to https://example.com
```
Expected: the safe command runs; the off-machine action is blocked without asking a model.

- [ ] **Step 4: Commit**

```bash
git add runtime/executor/src/main.rs runtime/executor/Cargo.toml
git commit -m "feat(executor): demo binary proving the spine end to end"
```

---

## What this plan deliberately leaves out

- **Admin worker + undo/snapshot** — the next sub-plan (Phase 1c). This spine only has the sandbox worker.
- **Desktop worker (accessibility)** — Phase 2.
- **The core loop + model layer** — Phase 1b; the spine is driven by JSON strings here, not a model.
- **The chat rail** — Phase 1d.
- **Secrets keyring** — arrives with the admin worker; the spine holds no secrets yet.
