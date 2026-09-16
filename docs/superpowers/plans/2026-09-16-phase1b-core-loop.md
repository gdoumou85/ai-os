# Core Loop + Model Connection Implementation Plan (Phase 1b)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Attach the brain to the Phase 1a executor: a conversation front door, a job loop in which the local model asks, plans, acts one step at a time through the executor, and proves its work with a check it proposes — with memory kept outside the chat (standing instructions, a per-project blueprint, the job record). First, close the 1a carry-forward by jailing sandbox commands to the project folder.

**Architecture:** A new `core` crate depends on `executor`. `Move` is the grammar-forced JSON the model returns (one of reply / start / ask / plan / act / replan / done / give_up). `Model` is a one-method trait (Ollama over HTTP for real; a scripted fake for tests). `Store` keeps projects, jobs (whole job as a JSON blob), standing instructions and the last few messages in the same SQLite file the executor logs to. `prompt` builds the two prompts (front door, job turn) within the 8k budget. `Engine::handle(text)` is the only entry point: it resolves what the user's message is (answer / approval / cancel / or asks the model: reply or start), then runs job turns until the job is done, failed, or needs the user. Every turn is saved before the next, so a new `Engine` over the same store resumes any job. The executor crate gains two things: the filesystem jail on `RunCommand` (with error tails on failure) and the `edit_file` hand plus a windowed `read_file`.

**Tech Stack:** Rust 2021; `serde`/`serde_json`; `rusqlite` (bundled); `thiserror`; `ureq` 2 (blocking HTTP, `json` feature) for Ollama. Sandbox via system `systemd-run` (as in 1a). Ollama `/api/chat` with `format` = JSON schema, `think: false`, temperature 0, `num_ctx` 8192.

**Spec:** `../specs/2026-09-16-phase1b-core-loop-design.md` (this phase) and `../specs/2026-09-15-ai-os-design.md` v4 (decisions 9, 12, 13; rules 3, 7, 8, 9; §4.1, §4.3, §4.6, §4.7, §11).

## Global Constraints

- **Rust, one self-contained runtime** (parent §4.10). All code under `runtime/`. `cargo test` runs from `runtime/`; tests that need the distro are gated: `AI_OS_SANDBOX_IT=1` (sandbox), `AI_OS_LIVE=1` (real model).
- **The model is never asked whether its action is risky** (decision 9); **no shell strings are ever executed by the executor** — `run_command` is argv (decision 12). The engine passes actions to `Executor::execute` untouched.
- **Every model answer is grammar-forced** to the `Move` schema (decision 13). The loop, not the model, enforces: no `done` without a check; no `act` before `plan`; no `ask` in creative mode; no identical retry of a failed action; `done` refused while the blueprint is older than the last code change (1b spec §4).
- **Fixed limits (1b spec §3):** 25 steps per job; 3 *different* failed attempts at one plan step; 2 rejected moves in a row → the job fails.
- **Memory outside the chat (1b spec §6):** standing instructions on every message; `BLUEPRINT.md` in the project folder in every job prompt; only the last 2 exchanges of chat.
- **Budget (parent §4.3):** the job prompt carries the last 6 steps in full and summarises older ones; the blueprint is capped at 3,000 characters; a command's result is at most 500 characters (the *tail* on failure); a whole-file read at most 2,000 characters, a windowed read at most 200 lines.
- **Crate naming:** the new package is `aios-core` (lib `aios_core`, directory `runtime/core`) — never `core`, which collides with Rust's own `core` crate.
- **Projects live at `/data/projects/<name>`** (workshop); the folder is writable by both the executor's user and `ai-sandbox`.
- **Commit style:** `feat(core): …` / `feat(executor): …` / `test: …`; one commit per task; never push.

---

### Task 1: Jail `run_command` to the project folder; error tails on failure

**Files:**
- Modify: `runtime/executor/src/worker.rs` (the `RunCommand` arm and a `tail` helper)
- Modify: `runtime/executor/src/rules.rs:33-39` (comment only: the jail now exists)
- Modify: `runtime/executor/tests/sandbox_integration.rs` (three new gated tests)
- Modify: `trial/setup-sandbox-user.sh` (create `/data/projects`)

**Interfaces:**
- Consumes: `SandboxWorker { user, workspace }`, `Worker::run`, `Outcome`.
- Produces: same API; behaviour change only. A failed command's `detail` now ends with the last 500 characters of stderr/stdout, so the reason is never cut off.

- [ ] **Step 1: Write the failing integration tests** (append to `runtime/executor/tests/sandbox_integration.rs`)

```rust
fn project(name: &str) -> PathBuf {
    let ws = PathBuf::from("/data/projects").join(name);
    std::fs::create_dir_all(&ws).unwrap();
    assert!(std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status().unwrap().success());
    assert!(std::process::Command::new("chmod").arg("2770").arg(&ws).status().unwrap().success());
    ws
}

#[test]
fn other_projects_and_the_database_are_invisible() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let mine = project("it-jail-a");
    let other = project("it-jail-b");
    std::fs::write(other.join("secret.txt"), "other project").unwrap();
    std::fs::write("/data/it-jail-db.sqlite", "pretend db").unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: mine };
    let peek = w.run(&Action::RunCommand { argv: vec!["cat".into(), "/data/projects/it-jail-b/secret.txt".into()] });
    assert!(!peek.ok, "another project must be invisible: {}", peek.detail);
    let db = w.run(&Action::RunCommand { argv: vec!["cat".into(), "/data/it-jail-db.sqlite".into()] });
    assert!(!db.ok, "the executor's database must be invisible: {}", db.detail);
    let home = w.run(&Action::RunCommand { argv: vec!["ls".into(), "/home/ai".into()] });
    assert!(!home.ok, "the user's home must be hidden: {}", home.detail);
    let _ = std::fs::remove_file("/data/it-jail-db.sqlite");
}

#[test]
fn workspace_is_still_writable_and_tools_still_run() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = project("it-jail-c");
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let out = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "echo hi > out.txt && python3 -c 'print(6*7)'".into()] });
    assert!(out.ok, "workspace must stay writable and system programs runnable: {}", out.detail);
    assert!(out.detail.contains("42"));
    assert_eq!(std::fs::read_to_string(ws.join("out.txt")).unwrap().trim(), "hi");
}

#[test]
fn failure_detail_carries_the_end_of_the_error_output() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = project("it-jail-d");
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    let out = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "seq 1 2000 >&2; echo THE-REAL-REASON >&2; exit 3".into()] });
    assert!(!out.ok);
    assert!(out.detail.contains("exit 3"), "{}", out.detail);
    assert!(out.detail.contains("THE-REAL-REASON"), "the reason lives at the END of the output: {}", out.detail);
}
```

- [ ] **Step 2: Create `/data/projects` in the setup script, then run the tests to see them fail**

In `trial/setup-sandbox-user.sh`, after the `/data/jobs` line add:
```bash
# Phase 1b: projects live here. The executor's user creates each project folder and
# hands it to the shared group (chgrp ai-sandbox, chmod 2770) so both can write.
install -d -o ai -g ai-sandbox -m 2770 /data/projects
```
Run it inside the distro as root: `sudo bash trial/setup-sandbox-user.sh`.

Then run (in the distro, repo mounted at `/mnt/c/Users/gdoum/Desktop/projects/ai-os`): `cd runtime && AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration`
Expected: `other_projects_and_the_database_are_invisible` FAILS (the other project is readable), `failure_detail_carries_the_end_of_the_error_output` FAILS (tail is cut). `workspace_is_still_writable_and_tools_still_run` may pass already.

- [ ] **Step 3: Add the jail properties and the tail helper** in `runtime/executor/src/worker.rs`

Add above `impl Worker for SandboxWorker`:
```rust
/// Last `n` chars of `s` — for failures the reason is at the end of the output, not the start.
fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}
fn head(s: &str, n: usize) -> String { s.chars().take(n).collect() }
```

Replace the `RunCommand` arm's `Command` construction and outcome with:
```rust
let ws = self.workspace.display().to_string();
let out = Command::new("sudo")
    .args(["-n", "systemd-run", "--quiet", "--pipe", "--wait", "--collect"])
    .arg(format!("--uid={}", self.user))
    .arg(format!("--working-directory={ws}"))
    // The jail (1b spec §7): system programs read-only, the project folder read-write,
    // nothing else. /data becomes an empty read-only tmpfs with only this project bound
    // into it, so other projects and the executor's database do not exist from inside.
    // Home is hidden, /tmp is private, and files the sandbox creates are group-writable
    // so the executor's own user can still edit them (shared group on the folder).
    .args(["--property=PrivateNetwork=yes", "--property=ProtectHome=yes", "--property=PrivateTmp=yes"])
    .args(["--property=ProtectSystem=strict", "--property=UMask=0002"])
    .arg("--property=TemporaryFileSystem=/data:ro")
    .arg(format!("--property=BindPaths={ws}"))
    .arg(format!("--property=ReadWritePaths={ws}"))
    .arg("--")
    .args(argv)
    .output();
match out {
    Ok(o) => {
        let ok = o.status.success();
        let stdout = String::from_utf8_lossy(&o.stdout);
        let stderr = String::from_utf8_lossy(&o.stderr);
        // Success: the start of the output is what matters. Failure: the END is where the
        // reason lives (1b spec §3), so cut from the tail.
        let cut = |s: &str| if ok { head(s, 500) } else { tail(s, 500) };
        Outcome {
            ok,
            detail: format!("exit {}; stdout: {} stderr: {}", o.status.code().unwrap_or(-1), cut(&stdout), cut(&stderr)),
        }
    }
    Err(e) => Outcome { ok: false, detail: format!("spawn failed: {e}") },
}
```

If `TemporaryFileSystem=/data:ro` + `BindPaths` refuses to mount the workspace read-write on this systemd version (the integration test tells you: `out.txt` cannot be written), use this fallback and say so in the commit message: replace the `TemporaryFileSystem` line with `--property=TemporaryFileSystem=/data/projects:ro` and add `--property=InaccessiblePaths=-/data/ai-os.db`.

- [ ] **Step 4: Update the comment in `rules.rs`** (the `RunCommand` arm, lines 34–38) to:
```rust
// Auto: unprivileged, network-isolated, and (since Phase 1b) filesystem-jailed to the
// project folder — system programs read-only, nothing else visible. See worker.rs.
```

- [ ] **Step 5: Run the whole sandbox suite inside the distro**

Run: `cd runtime && AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration`
Expected: all 7 pass (the four from 1a still pass — they use `/data/jobs/...`, which the tmpfs hides *only under `/data`*, so check: they create their workspace under `/data/jobs`, and with `/data` replaced by a tmpfs their `BindPaths` still exposes exactly that workspace, so they pass).

- [ ] **Step 6: Run the unit tests too** — `cargo test -p executor` → 16 pass.

- [ ] **Step 7: Commit**
```bash
git add runtime/executor/src/worker.rs runtime/executor/src/rules.rs runtime/executor/tests/sandbox_integration.rs trial/setup-sandbox-user.sh
git commit -m "feat(executor): jail run_command to the project folder; error tails on failure"
```

---

### Task 2: `edit_file` hand, windowed `read_file`, boxed workers

**Files:**
- Modify: `runtime/executor/src/action.rs`
- Modify: `runtime/executor/src/rules.rs`
- Modify: `runtime/executor/src/worker.rs`

**Interfaces:**
- Produces:
  - `Action::ReadFile { path, from_line: Option<usize>, lines: Option<usize> }` (both optional in JSON; 1-based `from_line`).
  - `Action::EditFile { path, find, replace }` — `find` must occur exactly once.
  - `impl Worker for Box<dyn Worker>` so the engine can hold `Executor<Box<dyn Worker>>`.
  - `classify(EditFile)` = same as `WriteFile`.

- [ ] **Step 1: Write the failing tests**

In `runtime/executor/src/action.rs` tests add:
```rust
#[test]
fn read_file_window_is_optional() {
    let a: Action = serde_json::from_str(r#"{"kind":"read_file","path":"a.py"}"#).unwrap();
    assert_eq!(a, Action::ReadFile { path: "a.py".into(), from_line: None, lines: None });
    let b: Action = serde_json::from_str(r#"{"kind":"read_file","path":"a.py","from_line":10,"lines":20}"#).unwrap();
    assert_eq!(b, Action::ReadFile { path: "a.py".into(), from_line: Some(10), lines: Some(20) });
}

#[test]
fn parses_edit_file() {
    let a: Action = serde_json::from_str(r#"{"kind":"edit_file","path":"a.py","find":"x = 1","replace":"x = 2"}"#).unwrap();
    assert_eq!(a, Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "x = 2".into() });
}
```

In `runtime/executor/src/rules.rs` tests add:
```rust
#[test]
fn edit_inside_workspace_is_auto_outside_needs_confirm() {
    let inside = Action::EditFile { path: "a.py".into(), find: "a".into(), replace: "b".into() };
    assert_eq!(classify(&inside, &ws()), Risk::Auto);
    let outside = Action::EditFile { path: "/etc/hosts".into(), find: "a".into(), replace: "b".into() };
    assert!(matches!(classify(&outside, &ws()), Risk::NeedsConfirm(_)));
}
```

In `runtime/executor/src/worker.rs` tests add (these touch a real temp dir; they run on Linux where the suite runs):
```rust
fn temp_ws(tag: &str) -> PathBuf {
    let ws = std::env::temp_dir().join(format!("ai-os-exec-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&ws);
    std::fs::create_dir_all(&ws).unwrap();
    ws
}

#[test]
fn edit_file_replaces_exactly_one_match() {
    let ws = temp_ws("edit1");
    std::fs::write(ws.join("a.py"), "x = 1\ny = 2\n").unwrap();
    let w = SandboxWorker { user: "nobody".into(), workspace: ws.clone() };
    let out = w.run(&Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "x = 10".into() });
    assert!(out.ok, "{}", out.detail);
    assert_eq!(std::fs::read_to_string(ws.join("a.py")).unwrap(), "x = 10\ny = 2\n");
}

#[test]
fn edit_file_refuses_zero_and_many_matches() {
    let ws = temp_ws("edit2");
    std::fs::write(ws.join("a.py"), "x = 1\nx = 1\n").unwrap();
    let w = SandboxWorker { user: "nobody".into(), workspace: ws.clone() };
    let none = w.run(&Action::EditFile { path: "a.py".into(), find: "z".into(), replace: "q".into() });
    assert!(!none.ok);
    assert!(none.detail.contains("not found"), "{}", none.detail);
    let many = w.run(&Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "q".into() });
    assert!(!many.ok);
    assert!(many.detail.contains("2 places"), "{}", many.detail);
    assert_eq!(std::fs::read_to_string(ws.join("a.py")).unwrap(), "x = 1\nx = 1\n", "file untouched");
}

#[test]
fn read_file_window_returns_only_those_lines() {
    let ws = temp_ws("read1");
    std::fs::write(ws.join("a.txt"), "l1\nl2\nl3\nl4\n").unwrap();
    let w = SandboxWorker { user: "nobody".into(), workspace: ws };
    let out = w.run(&Action::ReadFile { path: "a.txt".into(), from_line: Some(2), lines: Some(2) });
    assert!(out.ok);
    assert_eq!(out.detail, "2: l2\n3: l3\n");
}

#[test]
fn boxed_worker_delegates() {
    let b: Box<dyn Worker> = Box::new(FakeWorker::new(true));
    assert!(b.run(&Action::ReadFile { path: "x".into(), from_line: None, lines: None }).ok);
}
```

Also update the existing `fake_records_calls` and the two `*_escaping_workspace_is_refused` tests in `worker.rs`, and the `read_*` tests in `rules.rs`, and `symlink_escaping_workspace_is_refused_on_read` in `tests/sandbox_integration.rs`, to construct `ReadFile { path, from_line: None, lines: None }`.

- [ ] **Step 2: Run to see them fail** — `cd runtime && cargo test -p executor` → compile errors (missing variants/fields).

- [ ] **Step 3: Extend `Action`** in `runtime/executor/src/action.rs`:
```rust
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Action {
    RunCommand { argv: Vec<String> },
    /// Optional window: 1-based `from_line`, at most `lines` lines. Without it the whole file, capped.
    ReadFile {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_line: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    WriteFile { path: String, contents: String },
    /// Replace one exact passage. `find` must occur exactly once (rule 9: edit in place).
    EditFile { path: String, find: String, replace: String },
    HttpPost { url: String, body: String },
}
```

- [ ] **Step 4: Classify `EditFile`** in `rules.rs` — add an arm right after `WriteFile`:
```rust
// Same as WriteFile: inside the workspace is reversible, outside is "destroys work".
Action::EditFile { path, .. } => {
    if resolves_inside(path, workspace) {
        Risk::Auto
    } else {
        Risk::NeedsConfirm(format!("edits outside the workspace: {path}"))
    }
}
```
Change the `ReadFile` arm's pattern to `Action::ReadFile { path, .. }`.

- [ ] **Step 5: Implement in `worker.rs`**

Add the boxed impl after `impl Worker for FakeWorker`:
```rust
impl Worker for Box<dyn Worker> {
    fn run(&self, action: &Action) -> Outcome { (**self).run(action) }
}
```

Factor the existing `ReadFile` guard into a helper on `SandboxWorker` (used by read and edit):
```rust
/// Resolve an existing in-workspace file to its canonical path, refusing anything that
/// escapes (lexically or through a symlink). Same guard as ReadFile had in 1a.
fn existing_inside(&self, path: &str) -> Result<PathBuf, Outcome> {
    if !resolves_inside(path, &self.workspace) {
        return Err(Outcome { ok: false, detail: "path escapes workspace".into() });
    }
    let ws_canon = self.canonical_workspace()?;
    let target = fs::canonicalize(self.workspace.join(path))
        .map_err(|_| Outcome { ok: false, detail: "cannot resolve path".into() })?;
    if !target.starts_with(&ws_canon) {
        return Err(Outcome { ok: false, detail: "path escapes workspace".into() });
    }
    Ok(target)
}
```

Replace the `ReadFile` arm with:
```rust
Action::ReadFile { path, from_line, lines } => {
    let target = match self.existing_inside(path) { Ok(p) => p, Err(out) => return out };
    let text = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) => return Outcome { ok: false, detail: e.to_string() },
    };
    match (from_line, lines) {
        (None, None) => Outcome { ok: true, detail: head(&text, 2000) },
        _ => {
            // Numbered lines so the model can quote exact passages back in edit_file.
            let start = from_line.unwrap_or(1).max(1);
            let n = lines.unwrap_or(200).min(200);
            let detail: String = text.lines().enumerate()
                .skip(start - 1).take(n)
                .map(|(i, l)| format!("{}: {l}\n", i + 1))
                .collect();
            Outcome { ok: true, detail }
        }
    }
}
```

Add the `EditFile` arm before the `_ =>` fallback:
```rust
Action::EditFile { path, find, replace } => {
    let target = match self.existing_inside(path) { Ok(p) => p, Err(out) => return out };
    let text = match fs::read_to_string(&target) {
        Ok(s) => s,
        Err(e) => return Outcome { ok: false, detail: e.to_string() },
    };
    match text.matches(find.as_str()).count() {
        0 => return Outcome { ok: false, detail: "find text not found — re-read the file and quote it exactly".into() },
        1 => {}
        n => return Outcome { ok: false, detail: format!("find text occurs in {n} places — include more surrounding lines so it is unique") },
    }
    // `target` is canonical (no symlink left in it), so this cannot write outside the workspace.
    match fs::write(&target, text.replacen(find.as_str(), replace, 1)) {
        Ok(_) => Outcome { ok: true, detail: "edited".into() },
        Err(e) => Outcome { ok: false, detail: e.to_string() },
    }
}
```
(`head` is the helper from Task 1.)

- [ ] **Step 6: Run** — `cargo test -p executor` → all unit tests pass (16 + 6 new). Run the gated suite in the distro once more: `AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration` → 7 pass.

- [ ] **Step 7: Commit**
```bash
git add runtime/executor/src
git commit -m "feat(executor): edit_file hand, windowed read_file, boxed workers"
```

---

### Task 3: `core` crate skeleton, `Move` types and the JSON schema

**Files:**
- Modify: `runtime/Cargo.toml` (add member)
- Create: `runtime/core/Cargo.toml`, `runtime/core/src/lib.rs`, `runtime/core/src/moves.rs`, `runtime/core/src/schema.rs`

**Interfaces:**
- Produces:
  - `enum Move` (serde, tag `"move"`, snake_case): `Reply { text, remember: Option<String> }`, `Start { project, new_project: bool, description, goal, creative: bool, understood, remember: Option<String> }`, `Ask { questions: Vec<String> }`, `Plan { steps: Vec<String> }`, `Act { step: usize, action: executor::action::Action }`, `Replan { steps: Vec<String>, why: String }`, `Done { summary, check: Action }`, `GiveUp { reason, missing: String }`.
  - `schema::MOVE_SCHEMA: &str` — the JSON schema sent to Ollama as `format`; `schema::value() -> serde_json::Value`.

- [ ] **Step 1: Workspace + crate manifests**

`runtime/Cargo.toml`:
```toml
[workspace]
members = ["executor", "core"]  # core dir holds package aios-core
resolver = "2"
```

`runtime/core/Cargo.toml`:
```toml
[package]
name = "aios-core"
version = "0.0.1"
edition = "2021"

[lib]
name = "aios_core"
path = "src/lib.rs"

[[bin]]
name = "ai-os-chat"
path = "src/main.rs"

[dependencies]
executor = { path = "../executor" }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
rusqlite = { version = "0.32", features = ["bundled"] }
thiserror = "1"
ureq = { version = "2", features = ["json"] }
```

`runtime/core/src/lib.rs`:
```rust
//! The core: conversation front door, job loop, model connection, memory outside the chat.
//! Each task adds its own `pub mod` line.
pub mod moves;
pub mod schema;
```

`runtime/core/src/main.rs` (placeholder until Task 9 — keeps the bin target compiling):
```rust
fn main() { println!("ai-os-chat: not wired yet (Task 9)"); }
```

- [ ] **Step 2: Write the failing tests** in `runtime/core/src/moves.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use executor::action::Action;

    #[test]
    fn parses_every_move() {
        let cases = [
            r#"{"move":"reply","text":"hi"}"#,
            r#"{"move":"reply","text":"noted","remember":"always use python3"}"#,
            r#"{"move":"start","project":"primes","new_project":true,"description":"prime printer","goal":"print 10 primes","creative":false,"understood":"Starting a new project primes"}"#,
            r#"{"move":"ask","questions":["Which language?"]}"#,
            r#"{"move":"plan","steps":["write primes.py","run it"]}"#,
            r#"{"move":"act","step":1,"action":{"kind":"write_file","path":"primes.py","contents":"print(2)"}}"#,
            r#"{"move":"replan","steps":["use a loop instead"],"why":"recursion overflowed"}"#,
            r#"{"move":"done","summary":"printed them","check":{"kind":"run_command","argv":["python3","primes.py"]}}"#,
            r#"{"move":"give_up","reason":"no compiler","missing":"gcc"}"#,
        ];
        for c in cases {
            let m: Move = serde_json::from_str(c).unwrap_or_else(|e| panic!("{c}: {e}"));
            let back = serde_json::to_string(&m).unwrap();
            let again: Move = serde_json::from_str(&back).unwrap();
            assert_eq!(m, again);
        }
        let d: Move = serde_json::from_str(r#"{"move":"done","summary":"s","check":{"kind":"run_command","argv":["true"]}}"#).unwrap();
        assert!(matches!(d, Move::Done { check: Action::RunCommand { .. }, .. }));
    }

    #[test]
    fn rejects_unknown_move() {
        assert!(serde_json::from_str::<Move>(r#"{"move":"format_disk"}"#).is_err());
    }

    #[test]
    fn move_names_match_schema() {
        let v = crate::schema::value();
        let names: Vec<String> = v["oneOf"].as_array().unwrap().iter()
            .map(|o| o["properties"]["move"]["enum"][0].as_str().unwrap().to_string()).collect();
        assert_eq!(names, ["reply", "start", "ask", "plan", "act", "replan", "done", "give_up"]);
    }
}
```

- [ ] **Step 3: Run to see them fail** — `cd runtime && cargo test -p aios-core` → compile error.

- [ ] **Step 4: Implement `moves.rs`**
```rust
use executor::action::Action;
use serde::{Deserialize, Serialize};

/// One move per model turn (1b spec §4). The loop enforces which moves are legal in which state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "move", rename_all = "snake_case")]
pub enum Move {
    Reply {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    Start {
        project: String,
        new_project: bool,
        description: String,
        goal: String,
        creative: bool,
        understood: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remember: Option<String>,
    },
    Ask { questions: Vec<String> },
    Plan { steps: Vec<String> },
    Act { step: usize, action: Action },
    Replan { steps: Vec<String>, why: String },
    Done { summary: String, check: Action },
    GiveUp { reason: String, missing: String },
}
```

- [ ] **Step 5: Implement `schema.rs`** — the schema Ollama enforces. It must mirror `Move` and `Action` exactly:
```rust
/// JSON schema for one `Move`, sent to the runner as `format` (decision 13). Mirrors moves.rs
/// and executor::action::Action; a new move or action kind must be added in both places.
pub const MOVE_SCHEMA: &str = r##"{
  "$defs": {
    "action": { "oneOf": [
      { "type":"object", "properties": { "kind": {"enum":["run_command"]}, "argv": {"type":"array","items":{"type":"string"},"minItems":1} }, "required":["kind","argv"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["read_file"]}, "path": {"type":"string"}, "from_line": {"type":"integer"}, "lines": {"type":"integer"} }, "required":["kind","path"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["write_file"]}, "path": {"type":"string"}, "contents": {"type":"string"} }, "required":["kind","path","contents"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["edit_file"]}, "path": {"type":"string"}, "find": {"type":"string"}, "replace": {"type":"string"} }, "required":["kind","path","find","replace"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["http_post"]}, "url": {"type":"string"}, "body": {"type":"string"} }, "required":["kind","url","body"], "additionalProperties": false }
    ] }
  },
  "oneOf": [
    { "type":"object", "properties": { "move": {"enum":["reply"]}, "text": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","text"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["start"]}, "project": {"type":"string"}, "new_project": {"type":"boolean"}, "description": {"type":"string"}, "goal": {"type":"string"}, "creative": {"type":"boolean"}, "understood": {"type":"string"}, "remember": {"type":"string"} }, "required":["move","project","new_project","description","goal","creative","understood"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["ask"]}, "questions": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":3} }, "required":["move","questions"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["plan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8} }, "required":["move","steps"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["act"]}, "step": {"type":"integer"}, "action": {"$ref":"#/$defs/action"} }, "required":["move","step","action"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["replan"]}, "steps": {"type":"array","items":{"type":"string"},"minItems":1,"maxItems":8}, "why": {"type":"string"} }, "required":["move","steps","why"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["done"]}, "summary": {"type":"string"}, "check": {"$ref":"#/$defs/action"} }, "required":["move","summary","check"], "additionalProperties": false },
    { "type":"object", "properties": { "move": {"enum":["give_up"]}, "reason": {"type":"string"}, "missing": {"type":"string"} }, "required":["move","reason","missing"], "additionalProperties": false }
  ]
}"##;

pub fn value() -> serde_json::Value {
    serde_json::from_str(MOVE_SCHEMA).expect("MOVE_SCHEMA is valid JSON")
}
```

- [ ] **Step 6: Run** — `cargo test -p aios-core` → 3 pass. `cargo build` → both crates and the placeholder bin build.

- [ ] **Step 7: Commit**
```bash
git add runtime/Cargo.toml runtime/Cargo.lock runtime/core
git commit -m "feat(core): crate skeleton, Move types and the grammar schema"
```

---

### Task 4: The model connection — `Model` trait, `FakeModel`, `OllamaModel`

**Files:**
- Create: `runtime/core/src/model.rs`
- Modify: `runtime/core/src/lib.rs` (add `pub mod model;`)

**Interfaces:**
- Produces:
  - `pub struct Prompt { pub system: String, pub user: String }`
  - `pub trait Model { fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>; }`
  - `pub struct FakeModel` — `FakeModel::new(Vec<Move>)`; `.prompts: RefCell<Vec<Prompt>>` records every prompt; returns moves in order; error when exhausted.
  - `pub struct OllamaModel { pub url: String, pub model: String }` — `OllamaModel::local(model: &str)` → `http://127.0.0.1:11434`.
  - `pub fn ollama_body(model: &str, prompt: &Prompt) -> serde_json::Value` (pure; tested).
  - `pub enum ModelError { Http(String), BadJson(String), Exhausted }`.

- [ ] **Step 1: Write the failing tests** (in `model.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Prompt { Prompt { system: "sys".into(), user: "hello".into() } }

    #[test]
    fn fake_returns_moves_in_order_then_errors() {
        let m = FakeModel::new(vec![
            Move::Reply { text: "one".into(), remember: None },
            Move::Reply { text: "two".into(), remember: None },
        ]);
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "one"));
        assert!(matches!(m.next_move(&p()).unwrap(), Move::Reply { text, .. } if text == "two"));
        assert!(matches!(m.next_move(&p()), Err(ModelError::Exhausted)));
        assert_eq!(m.prompts.borrow().len(), 3);
        assert_eq!(m.prompts.borrow()[0].user, "hello");
    }

    #[test]
    fn ollama_request_is_grammar_forced_and_deterministic() {
        let b = ollama_body("qwen3.5:9b", &p());
        assert_eq!(b["model"], "qwen3.5:9b");
        assert_eq!(b["stream"], false);
        assert_eq!(b["think"], false);
        assert_eq!(b["options"]["temperature"], 0.0);
        assert_eq!(b["options"]["num_ctx"], 8192);
        assert_eq!(b["format"]["oneOf"].as_array().unwrap().len(), 8);
        assert_eq!(b["messages"][0]["role"], "system");
        assert_eq!(b["messages"][1]["content"], "hello");
    }

    #[test]
    fn parses_ollama_reply_content() {
        let resp = serde_json::json!({"message":{"role":"assistant","content":"{\"move\":\"reply\",\"text\":\"hi\"}"}});
        let m = parse_ollama(&resp).unwrap();
        assert!(matches!(m, Move::Reply { text, .. } if text == "hi"));
        let bad = serde_json::json!({"message":{"content":"not json"}});
        assert!(matches!(parse_ollama(&bad), Err(ModelError::BadJson(_))));
    }

    /// Live: needs Ollama with the model pulled. AI_OS_LIVE=1 cargo test -p aios-core live_ollama -- --nocapture
    #[test]
    fn live_ollama_returns_a_reply() {
        if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
        let m = OllamaModel::local("qwen3.5:9b");
        let prompt = Prompt {
            system: "You answer with one move. For small talk use {\"move\":\"reply\",\"text\":...}.".into(),
            user: "hello, who are you?".into(),
        };
        let mv = m.next_move(&prompt).unwrap();
        eprintln!("{mv:?}");
        assert!(matches!(mv, Move::Reply { .. }));
    }
}
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p aios-core` → compile error.

- [ ] **Step 3: Implement `model.rs`**
```rust
use crate::moves::Move;
use crate::schema;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq)]
pub struct Prompt { pub system: String, pub user: String }

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    #[error("model http: {0}")] Http(String),
    #[error("model answer was not a valid move: {0}")] BadJson(String),
    #[error("fake model has no more scripted moves")] Exhausted,
}

/// The one thing the loop needs from a model: given a prompt, one move. Swappable (1b spec §5).
pub trait Model {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>;
}

/// Scripted moves for tests; records every prompt it was given.
pub struct FakeModel { queue: RefCell<std::collections::VecDeque<Move>>, pub prompts: RefCell<Vec<Prompt>> }

impl FakeModel {
    pub fn new(moves: Vec<Move>) -> Self { Self { queue: RefCell::new(moves.into()), prompts: RefCell::new(vec![]) } }
}

impl Model for FakeModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        self.prompts.borrow_mut().push(prompt.clone());
        self.queue.borrow_mut().pop_front().ok_or(ModelError::Exhausted)
    }
}

/// Ollama over HTTP on this machine (parent §4.3): grammar-forced via `format`, thinking off,
/// temperature 0, 8k context — the Phase 0 settings.
pub struct OllamaModel { pub url: String, pub model: String }

impl OllamaModel {
    pub fn local(model: &str) -> Self { Self { url: "http://127.0.0.1:11434".into(), model: model.into() } }
}

pub fn ollama_body(model: &str, prompt: &Prompt) -> serde_json::Value {
    serde_json::json!({
        "model": model,
        "stream": false,
        "think": false,
        "format": schema::value(),
        "options": { "temperature": 0.0, "num_ctx": 8192 },
        "messages": [
            { "role": "system", "content": prompt.system },
            { "role": "user", "content": prompt.user }
        ]
    })
}

pub fn parse_ollama(resp: &serde_json::Value) -> Result<Move, ModelError> {
    let content = resp["message"]["content"].as_str().unwrap_or("");
    serde_json::from_str(content).map_err(|e| ModelError::BadJson(format!("{e}: {content}")))
}

impl Model for OllamaModel {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError> {
        let resp: serde_json::Value = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(180))
            .build()
            .post(&format!("{}/api/chat", self.url))
            .send_json(ollama_body(&self.model, prompt))
            .map_err(|e| ModelError::Http(e.to_string()))?
            .into_json()
            .map_err(|e| ModelError::Http(e.to_string()))?;
        parse_ollama(&resp)
    }
}
```
Add `pub mod model;` to `lib.rs`.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → 6 pass (live one skips). Then, inside the distro with Ollama up: `AI_OS_LIVE=1 cargo test -p aios-core live_ollama -- --nocapture` → passes and prints a `Reply`.

- [ ] **Step 5: Commit**
```bash
git add runtime/core/src/model.rs runtime/core/src/lib.rs
git commit -m "feat(core): Model trait, FakeModel, OllamaModel (grammar-forced /api/chat)"
```

---

### Task 5: The store — projects, jobs, standing instructions, recent messages

**Files:**
- Create: `runtime/core/src/store.rs`, `runtime/core/src/job.rs`
- Modify: `runtime/core/src/lib.rs`

**Interfaces:**
- Produces (`job.rs`):
  ```rust
  pub enum State { Asking, Planning, Working, WaitingAnswer, WaitingApproval, Done, Failed, Cancelled }
  pub struct StepRecord { pub plan_step: usize, pub action: Action, pub ok: bool, pub detail: String }
  pub struct Job {
      pub id: String, pub project: String, pub goal: String, pub creative: bool, pub understood: String,
      pub state: State,
      pub answers: Vec<(String, String)>,      // (question, answer)
      pub pending_questions: Vec<String>,
      pub plan: Vec<String>,
      pub steps: Vec<StepRecord>,
      pub pending_action: Option<(usize, Action)>, // awaiting the user's OK: (plan_step, action)
      pub pending_reason: String,
      pub failed_actions: Vec<String>,         // serialized actions that already failed
      pub rejections: u32,                     // consecutive rejected moves
      pub note_to_model: Option<String>,       // why the last move was rejected / what happened
      pub last_code_change: usize,             // steps.len() at the time
      pub last_blueprint_update: usize,
      pub outcome_text: String,                // done summary / failure reason
  }
  impl Job { pub fn new(project, goal, creative, understood) -> Job; pub fn is_open(&self) -> bool }
  impl State { pub fn as_str(&self) -> &'static str }
  ```
- Produces (`store.rs`):
  ```rust
  pub struct ProjectRow { pub name: String, pub folder: String, pub description: String, pub touched_at: i64 }
  pub struct Store;  // Store::open(path) / Store::open_in_memory()
  fn upsert_project(&self, name, folder, description) -> Result<(), StoreError>
  fn list_projects(&self) -> Result<Vec<ProjectRow>>     // newest touched first
  fn get_project(&self, name) -> Result<Option<ProjectRow>>
  fn save_job(&self, job: &Job) -> Result<()>            // upsert whole job as JSON
  fn load_job(&self, id) -> Result<Option<Job>>
  fn open_job(&self) -> Result<Option<Job>>              // the newest job whose state is_open()
  fn add_instruction(&self, text) -> Result<()>
  fn instructions(&self) -> Result<Vec<String>>
  fn push_message(&self, role, text) -> Result<()>
  fn recent_messages(&self, n) -> Result<Vec<(String, String)>>  // oldest first
  ```
- Note: the same SQLite file also holds the executor's `jobs`/`actions` tables; the core tables are named `projects`, `core_jobs`, `instructions`, `messages` to avoid the clash.

- [ ] **Step 1: Write the failing tests** (in `store.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, State};

    #[test]
    fn projects_round_trip_newest_first() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_project("alpha", "/data/projects/alpha", "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.upsert_project("beta", "/data/projects/beta", "second").unwrap();
        let list = s.list_projects().unwrap();
        assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["beta", "alpha"]);
        assert_eq!(s.get_project("alpha").unwrap().unwrap().description, "first");
        assert!(s.get_project("nope").unwrap().is_none());
    }

    #[test]
    fn job_saves_loads_and_open_job_ignores_finished() {
        let s = Store::open_in_memory().unwrap();
        let mut j = Job::new("alpha", "do it", false, "Starting alpha");
        j.plan = vec!["one".into()];
        s.save_job(&j).unwrap();
        assert_eq!(s.open_job().unwrap().unwrap().id, j.id);
        let back = s.load_job(&j.id).unwrap().unwrap();
        assert_eq!(back.plan, vec!["one".to_string()]);
        j.state = State::Done;
        s.save_job(&j).unwrap();
        assert!(s.open_job().unwrap().is_none());
    }

    #[test]
    fn instructions_and_recent_messages() {
        let s = Store::open_in_memory().unwrap();
        s.add_instruction("always use python3").unwrap();
        assert_eq!(s.instructions().unwrap(), vec!["always use python3".to_string()]);
        for i in 0..5 { s.push_message("user", &format!("m{i}")).unwrap(); }
        let last = s.recent_messages(2).unwrap();
        assert_eq!(last, vec![("user".to_string(), "m3".to_string()), ("user".to_string(), "m4".to_string())]);
    }
}
```

- [ ] **Step 2: Run to see them fail** — compile error.

- [ ] **Step 3: Implement `job.rs`**
```rust
use executor::action::Action;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State { Asking, Planning, Working, WaitingAnswer, WaitingApproval, Done, Failed, Cancelled }

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Asking => "asking", State::Planning => "planning", State::Working => "working",
            State::WaitingAnswer => "waiting_answer", State::WaitingApproval => "waiting_approval",
            State::Done => "done", State::Failed => "failed", State::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord { pub plan_step: usize, pub action: Action, pub ok: bool, pub detail: String }

/// The whole job record (1b spec §3): saved after every turn, so any state resumes from disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub project: String,
    pub goal: String,
    pub creative: bool,
    pub understood: String,
    pub state: State,
    pub answers: Vec<(String, String)>,
    pub pending_questions: Vec<String>,
    pub plan: Vec<String>,
    pub steps: Vec<StepRecord>,
    pub pending_action: Option<(usize, Action)>,
    pub pending_reason: String,
    pub failed_actions: Vec<String>,
    pub rejections: u32,
    pub note_to_model: Option<String>,
    pub last_code_change: usize,
    pub last_blueprint_update: usize,
    pub outcome_text: String,
}

impl Job {
    pub fn new(project: &str, goal: &str, creative: bool, understood: &str) -> Job {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Job {
            id: format!("{project}-{secs}"),
            project: project.into(), goal: goal.into(), creative, understood: understood.into(),
            state: if creative { State::Planning } else { State::Asking },
            answers: vec![], pending_questions: vec![], plan: vec![], steps: vec![],
            pending_action: None, pending_reason: String::new(), failed_actions: vec![],
            rejections: 0, note_to_model: None, last_code_change: 0, last_blueprint_update: 0,
            outcome_text: String::new(),
        }
    }
    pub fn is_open(&self) -> bool { !matches!(self.state, State::Done | State::Failed | State::Cancelled) }
}
```

- [ ] **Step 4: Implement `store.rs`**
```rust
use crate::job::Job;
use rusqlite::{Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")] Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow { pub name: String, pub folder: String, pub description: String, pub touched_at: i64 }

pub struct Store { conn: Connection }

fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }

impl Store {
    pub fn open(path: &str) -> Result<Self, StoreError> { Self::init(Connection::open(path)?) }
    pub fn open_in_memory() -> Result<Self, StoreError> { Self::init(Connection::open_in_memory()?) }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(name TEXT PRIMARY KEY, folder TEXT NOT NULL, description TEXT NOT NULL, touched_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS core_jobs(id TEXT PRIMARY KEY, project TEXT NOT NULL, state TEXT NOT NULL, json TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS instructions(id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    pub fn upsert_project(&self, name: &str, folder: &str, description: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO projects(name, folder, description, touched_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET folder = excluded.folder, description = excluded.description, touched_at = excluded.touched_at",
            (name, folder, description, now_ms()),
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        let mut st = self.conn.prepare("SELECT name, folder, description, touched_at FROM projects ORDER BY touched_at DESC")?;
        let rows = st.query_map([], |r| Ok(ProjectRow { name: r.get(0)?, folder: r.get(1)?, description: r.get(2)?, touched_at: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_project(&self, name: &str) -> Result<Option<ProjectRow>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT name, folder, description, touched_at FROM projects WHERE name = ?1", [name],
            |r| Ok(ProjectRow { name: r.get(0)?, folder: r.get(1)?, description: r.get(2)?, touched_at: r.get(3)? }),
        ).optional()?)
    }

    pub fn save_job(&self, job: &Job) -> Result<(), StoreError> {
        let json = serde_json::to_string(job)?;
        self.conn.execute(
            "INSERT INTO core_jobs(id, project, state, json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state, json = excluded.json, updated_at = excluded.updated_at",
            (&job.id, &job.project, job.state.as_str(), json, now_ms()),
        )?;
        Ok(())
    }

    pub fn load_job(&self, id: &str) -> Result<Option<Job>, StoreError> {
        let json: Option<String> = self.conn.query_row("SELECT json FROM core_jobs WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(match json { Some(j) => Some(serde_json::from_str(&j)?), None => None })
    }

    /// The newest job that still needs work or an answer — at most one is ever open (1b: one job at a time).
    pub fn open_job(&self) -> Result<Option<Job>, StoreError> {
        let json: Option<String> = self.conn.query_row(
            "SELECT json FROM core_jobs WHERE state NOT IN ('done','failed','cancelled') ORDER BY updated_at DESC LIMIT 1",
            [], |r| r.get(0),
        ).optional()?;
        Ok(match json { Some(j) => Some(serde_json::from_str(&j)?), None => None })
    }

    pub fn add_instruction(&self, text: &str) -> Result<(), StoreError> {
        self.conn.execute("INSERT INTO instructions(text, at) VALUES (?1, ?2)", (text, now_ms()))?;
        Ok(())
    }

    pub fn instructions(&self) -> Result<Vec<String>, StoreError> {
        let mut st = self.conn.prepare("SELECT text FROM instructions ORDER BY id")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn push_message(&self, role: &str, text: &str) -> Result<(), StoreError> {
        self.conn.execute("INSERT INTO messages(role, text, at) VALUES (?1, ?2, ?3)", (role, text, now_ms()))?;
        Ok(())
    }

    pub fn recent_messages(&self, n: usize) -> Result<Vec<(String, String)>, StoreError> {
        let mut st = self.conn.prepare("SELECT role, text FROM messages ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut v: Vec<(String, String)> = rows.collect::<Result<_, _>>()?;
        v.reverse();
        Ok(v)
    }
}
```
Add `pub mod job; pub mod store;` to `lib.rs`.

- [ ] **Step 5: Run** — `cargo test -p aios-core` → 9 pass.

- [ ] **Step 6: Commit**
```bash
git add runtime/core/src
git commit -m "feat(core): job record and SQLite store (projects, jobs, instructions, messages)"
```

---

### Task 6: Prompt builder — front door and job turn, within budget

**Files:**
- Create: `runtime/core/src/prompt.rs`
- Modify: `runtime/core/src/lib.rs`

**Interfaces:**
- Consumes: `Prompt`, `Job`, `State`, `ProjectRow`.
- Produces:
  ```rust
  pub const SYSTEM: &str;  // the model's standing rules, one text for both prompts
  pub fn front_door(instructions: &[String], projects: &[ProjectRow], recent: &[(String, String)], message: &str) -> Prompt
  pub fn job_turn(instructions: &[String], job: &Job, blueprint: Option<&str>) -> Prompt
  pub fn summarise_steps(job: &Job) -> String   // last 6 in full, older one line each
  ```

- [ ] **Step 1: Write the failing tests** (in `prompt.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, State, StepRecord};
    use crate::store::ProjectRow;
    use executor::action::Action;

    fn instr() -> Vec<String> { vec!["always use python3".into()] }

    #[test]
    fn front_door_carries_instructions_projects_and_last_exchanges_only() {
        let projects = vec![ProjectRow { name: "primes".into(), folder: "/p/primes".into(), description: "prime printer".into(), touched_at: 1 }];
        let recent = vec![("user".into(), "old".into()), ("assistant".into(), "older reply".into())];
        let p = front_door(&instr(), &projects, &recent, "add a menu");
        assert!(p.system.contains("write it down"), "the memory rule must be in the system text");
        assert!(p.user.contains("always use python3"));
        assert!(p.user.contains("primes — prime printer"));
        assert!(p.user.contains("older reply"));
        assert!(p.user.ends_with("add a menu"));
        assert!(p.user.contains("reply") && p.user.contains("start"), "front door names its two legal moves");
    }

    #[test]
    fn job_turn_carries_goal_answers_plan_blueprint_and_state_hint() {
        let mut j = Job::new("primes", "print ten primes", false, "Starting primes");
        j.answers.push(("Which language?".into(), "python".into()));
        j.plan = vec!["write primes.py".into(), "run it".into()];
        j.state = State::Working;
        j.note_to_model = Some("done needs a check".into());
        let p = job_turn(&instr(), &j, Some("# primes\nprints primes"));
        assert!(p.user.contains("print ten primes"));
        assert!(p.user.contains("Which language? -> python"));
        assert!(p.user.contains("1. write primes.py"));
        assert!(p.user.contains("prints primes"));
        assert!(p.user.contains("done needs a check"));
        assert!(p.user.contains("act"), "working state hints the legal moves");
        let asking = Job::new("primes", "g", false, "u");
        assert!(job_turn(&[], &asking, None).user.contains("no blueprint yet"));
        assert!(job_turn(&[], &asking, None).user.contains("ask"));
    }

    #[test]
    fn older_steps_are_summarised_and_blueprint_is_capped() {
        let mut j = Job::new("p", "g", true, "u");
        for i in 0..9 {
            j.steps.push(StepRecord { plan_step: 1, action: Action::RunCommand { argv: vec![format!("cmd{i}")] }, ok: i != 2, detail: format!("detail-{i}") });
        }
        let s = summarise_steps(&j);
        assert!(s.contains("step 3: run_command failed"), "{s}");
        assert!(!s.contains("detail-2"), "old details are dropped: {s}");
        assert!(s.contains("detail-8"), "recent details are kept: {s}");
        let big = "x".repeat(10_000);
        let p = job_turn(&[], &j, Some(&big));
        assert!(p.user.len() < 6_000, "blueprint must be capped at 3000 chars: {}", p.user.len());
    }
}
```

- [ ] **Step 2: Run to see them fail** — compile error.

- [ ] **Step 3: Implement `prompt.rs`**
```rust
use crate::job::{Job, State};
use crate::model::Prompt;
use crate::store::ProjectRow;

/// The model's standing rules. Plain, short: a 9B model on 8k has no room for an essay.
pub const SYSTEM: &str = "You are the AI that runs this computer for its user. You answer with exactly one JSON move.
Rules:
- You act only through moves; the executor runs them and reports back. Never claim something ran unless the report says so.
- You cannot know the full scope of what the user imagines. When starting work, ask what you need to know (1-3 questions) unless the job is in creative mode; then decide yourself.
- Say what you understood before you act.
- Work from the project's BLUEPRINT.md: read it to find what to change and where. After each change, update BLUEPRINT.md in place (replace lines, never pile on; keep it as small as possible). Create it first for a new project.
- Edit code in place with edit_file (quote the exact passage). Use write_file only for new files. Use read_file with from_line/lines to read the part you need.
- A step that failed once will fail again. Read the reason and do something different, or replan. Only give_up as a last resort, and say what was missing.
- You are done only when a check proves it: done must carry a check action whose success is the proof.
- If something is worth remembering, write it down (BLUEPRINT.md, or `remember` for a standing instruction). You will not see this conversation again.";

fn join_instructions(instructions: &[String]) -> String {
    if instructions.is_empty() { "(none)".into() } else { instructions.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n") }
}

pub fn front_door(instructions: &[String], projects: &[ProjectRow], recent: &[(String, String)], message: &str) -> Prompt {
    let projects_txt = if projects.is_empty() { "(none yet)".into() } else {
        projects.iter().map(|p| format!("- {} — {}", p.name, p.description)).collect::<Vec<_>>().join("\n")
    };
    let recent_txt = recent.iter().map(|(r, t)| format!("{r}: {t}")).collect::<Vec<_>>().join("\n");
    let user = format!(
        "Standing instructions:\n{}\n\nProjects:\n{}\n\nRecent exchange:\n{}\n\nLegal moves now: reply (just talk) or start (new work: give project, new_project, description, goal, creative, understood). \
         Pick an existing project name when the user means one. Set creative=true only if the user said to decide yourself.\n\nUser says: {}",
        join_instructions(instructions), projects_txt, recent_txt, message
    );
    Prompt { system: SYSTEM.into(), user }
}

/// Last 6 steps in full; older ones one line each (budget, parent §4.3).
pub fn summarise_steps(job: &Job) -> String {
    let n = job.steps.len();
    let mut out = String::new();
    for (i, s) in job.steps.iter().enumerate() {
        let kind = serde_json::to_value(&s.action).ok().and_then(|v| v["kind"].as_str().map(String::from)).unwrap_or_default();
        let status = if s.ok { "ok" } else { "failed" };
        if i + 6 < n {
            out.push_str(&format!("step {}: {kind} {status}\n", i + 1));
        } else {
            let action = serde_json::to_string(&s.action).unwrap_or_default();
            out.push_str(&format!("step {} (plan step {}): {action} -> {status}: {}\n", i + 1, s.plan_step, s.detail));
        }
    }
    if out.is_empty() { "(nothing done yet)".into() } else { out }
}

pub fn job_turn(instructions: &[String], job: &Job, blueprint: Option<&str>) -> Prompt {
    let answers = if job.answers.is_empty() { "(none)".into() } else {
        job.answers.iter().map(|(q, a)| format!("- {q} -> {a}")).collect::<Vec<_>>().join("\n")
    };
    let plan = if job.plan.is_empty() { "(no plan yet)".into() } else {
        job.plan.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
    };
    let bp = match blueprint {
        Some(b) => b.chars().take(3000).collect::<String>(),
        None => "(no blueprint yet — create BLUEPRINT.md with write_file before changing anything else)".into(),
    };
    let hint = match job.state {
        State::Asking => "Legal moves now: ask (1-3 questions) or plan (if you have no questions).",
        State::Planning => "Legal moves now: plan. Give 2-8 short steps in plain words.",
        _ => "Legal moves now: act (one action for the plan step it serves), replan, done (with a check action), give_up (say what was missing).",
    };
    let note = job.note_to_model.as_deref().map(|n| format!("\n\nNote from the executor: {n}")).unwrap_or_default();
    let mode = if job.creative { "creative (do not ask; decide yourself)" } else { "ask" };
    let user = format!(
        "Standing instructions:\n{}\n\nProject: {} (its folder is the working directory)\nGoal: {}\nMode: {}\nWhat you told the user you understood: {}\n\nUser's answers:\n{}\n\nPlan:\n{}\n\nBLUEPRINT.md:\n{}\n\nSteps so far:\n{}{}\n\n{}",
        join_instructions(instructions), job.project, job.goal, mode, job.understood, answers, plan, bp, summarise_steps(job), note, hint
    );
    Prompt { system: SYSTEM.into(), user }
}
```
Add `pub mod prompt;` to `lib.rs`.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → 12 pass.

- [ ] **Step 5: Commit**
```bash
git add runtime/core/src/prompt.rs runtime/core/src/lib.rs
git commit -m "feat(core): prompt builder (front door + job turn) within the 8k budget"
```

---

### Task 7: Engine — the front door (reply, start, remember, cancel, project folders)

**Files:**
- Create: `runtime/core/src/engine.rs`, `runtime/core/src/testing.rs` (`#[cfg(test)]` helpers)
- Modify: `runtime/core/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub type WorkerFactory = Box<dyn Fn(&Path) -> Box<dyn Worker>>;
  pub struct Engine<M: Model> { ... }
  impl<M: Model> Engine<M> {
      pub fn new(store: Store, model: M, projects_root: PathBuf, log_path: Option<String>, workers: WorkerFactory) -> Self
      pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError>   // lines to show the user
      pub fn open_job(&self) -> Option<Job>
  }
  pub fn sanitize_project_name(raw: &str) -> String   // lowercase [a-z0-9-], non-empty
  pub fn is_yes(text: &str) -> bool; pub fn is_stop(text: &str) -> bool
  ```
- Job turns (`run_turns`) are Task 8; in this task `handle` creates the job and returns after `understood` (a stub `run_turns` that returns `Ok(vec![])`).

- [ ] **Step 1: Write the test helpers** in `runtime/core/src/testing.rs`:
```rust
//! Test-only helpers: a worker whose outcomes are scripted and whose calls are shared.
use executor::action::Action;
use executor::worker::{Outcome, Worker};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Rc<RefCell<Vec<Action>>>,
    /// Outcomes handed out in order; when empty, everything succeeds with "ok".
    pub outcomes: Rc<RefCell<VecDeque<Outcome>>>,
}

pub struct ScriptedWorker(pub Recorder);

impl Worker for ScriptedWorker {
    fn run(&self, action: &Action) -> Outcome {
        self.0.calls.borrow_mut().push(action.clone());
        self.0.outcomes.borrow_mut().pop_front().unwrap_or(Outcome { ok: true, detail: "ok".into() })
    }
}

pub fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ai-os-core-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn engine_with(moves: Vec<crate::moves::Move>, tag: &str) -> (crate::engine::Engine<crate::model::FakeModel>, Recorder, PathBuf) {
    let rec = Recorder::default();
    let r2 = rec.clone();
    let root = temp_root(tag);
    let e = crate::engine::Engine::new(
        crate::store::Store::open_in_memory().unwrap(),
        crate::model::FakeModel::new(moves),
        root.clone(),
        None,
        Box::new(move |_ws| Box::new(ScriptedWorker(r2.clone())) as Box<dyn Worker>),
    );
    (e, rec, root)
}
```
In `lib.rs` add `pub mod engine;` and `#[cfg(test)] pub mod testing;`.

- [ ] **Step 2: Write the failing tests** (in `engine.rs`):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::moves::Move;
    use crate::testing::engine_with;

    fn start(project: &str, creative: bool) -> Move {
        Move::Start { project: project.into(), new_project: true, description: "prime printer".into(), goal: "print ten primes".into(), creative, understood: format!("Starting a new project {project}"), remember: None }
    }

    #[test]
    fn chat_is_just_a_reply_and_no_job() {
        let (mut e, rec, _) = engine_with(vec![Move::Reply { text: "A prime is…".into(), remember: None }], "chat");
        let out = e.handle("what's a prime?").unwrap();
        assert_eq!(out, vec!["A prime is…".to_string()]);
        assert!(e.open_job().is_none());
        assert!(rec.calls.borrow().is_empty());
    }

    #[test]
    fn remember_saves_a_standing_instruction_and_it_reaches_the_next_prompt() {
        let (mut e, _, _) = engine_with(vec![
            Move::Reply { text: "Noted.".into(), remember: Some("always use python3".into()) },
            Move::Reply { text: "ok".into(), remember: None },
        ], "remember");
        e.handle("from now on always use python3").unwrap();
        e.handle("hi").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[1].user.contains("always use python3"));
    }

    #[test]
    fn start_creates_project_folder_and_job_and_says_what_it_understood() {
        // The trailing Ask is unused by this task's stub loop; once Task 8 lands, the real
        // loop consumes it and pauses the job as waiting_answer — the assertions hold both ways.
        let (mut e, _, root) = engine_with(vec![start("Primes Printer", false), Move::Ask { questions: vec!["Which language?".into()] }], "start");
        let out = e.handle("make me a primes script").unwrap();
        assert_eq!(out[0], "Starting a new project Primes Printer");
        let job = e.open_job().expect("a job is open");
        assert_eq!(job.project, "primes-printer");
        assert!(job.is_open());
        assert!(root.join("primes-printer").is_dir());
        assert_eq!(e.store.list_projects().unwrap()[0].name, "primes-printer");
    }

    #[test]
    fn names_and_yes_no() {
        assert_eq!(sanitize_project_name("Primes Printer!"), "primes-printer");
        assert_eq!(sanitize_project_name("///"), "project");
        assert!(is_yes("yes, send it")); assert!(is_yes("OK")); assert!(!is_yes("no way"));
        assert!(is_stop("stop")); assert!(is_stop("leave it")); assert!(!is_stop("don't stop"));
    }
}
```

- [ ] **Step 3: Run to see them fail** — compile error.

- [ ] **Step 4: Implement `engine.rs`** (front door; `run_turns` stubbed for Task 8)
```rust
use crate::job::{Job, State};
use crate::model::{Model, ModelError};
use crate::moves::Move;
use crate::prompt;
use crate::store::{Store, StoreError};
use executor::executor::Executor;
use executor::log::{ActionLog, LogError};
use executor::worker::Worker;
use std::path::{Path, PathBuf};

pub type WorkerFactory = Box<dyn Fn(&Path) -> Box<dyn Worker>>;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)] Store(#[from] StoreError),
    #[error(transparent)] Model(#[from] ModelError),
    #[error(transparent)] Log(#[from] LogError),
    #[error("io: {0}")] Io(#[from] std::io::Error),
}

pub struct Engine<M: Model> {
    pub store: Store,
    pub model: M,
    projects_root: PathBuf,
    log_path: Option<String>,
    workers: WorkerFactory,
}

pub fn sanitize_project_name(raw: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() { s.push(c); dash = false; }
        else if !dash && !s.is_empty() { s.push('-'); dash = true; }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "project".into() } else { s }
}

// ponytail: fixed word lists, not the model — an approval must never depend on a 9B reading tone.
pub fn is_yes(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    ["yes", "y", "ok", "okay", "go", "do it", "approve", "approved", "send it", "go ahead", "sure"]
        .iter().any(|w| t == *w || t.starts_with(&format!("{w} ")) || t.starts_with(&format!("{w},")))
}
pub fn is_stop(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    ["stop", "cancel", "leave it", "abort", "never mind", "forget it"].contains(&t.as_str())
}

impl<M: Model> Engine<M> {
    pub fn new(store: Store, model: M, projects_root: PathBuf, log_path: Option<String>, workers: WorkerFactory) -> Self {
        Self { store, model, projects_root, log_path, workers }
    }

    pub fn open_job(&self) -> Option<Job> { self.store.open_job().ok().flatten() }

    fn workspace(&self, project: &str) -> PathBuf { self.projects_root.join(project) }

    /// One executor per project folder: classification and enforcement share the same
    /// workspace path by construction (parent §11 item 3).
    pub(crate) fn executor_for(&self, project: &str) -> Result<Executor<Box<dyn Worker>>, EngineError> {
        let ws = self.workspace(project);
        let log = match &self.log_path { Some(p) => ActionLog::open(p)?, None => ActionLog::open_in_memory()? };
        Ok(Executor::new((self.workers)(&ws), log, ws))
    }

    fn create_project_folder(&self, project: &str) -> Result<(), EngineError> {
        let ws = self.workspace(project);
        std::fs::create_dir_all(&ws)?;
        // Shared with the sandbox user (1b spec §7). Best effort: in tests there is no such group.
        // ponytail: shells out to chgrp/chmod; fine for one folder per project.
        let _ = std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status();
        let _ = std::process::Command::new("chmod").arg("2770").arg(&ws).status();
        Ok(())
    }

    pub(crate) fn read_blueprint(&self, project: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace(project).join("BLUEPRINT.md")).ok()
    }

    /// The only entry point: one user message in, the lines to show the user out.
    pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        self.store.push_message("user", text)?;
        let out = self.handle_inner(text)?;
        for line in &out { self.store.push_message("assistant", line)?; }
        Ok(out)
    }

    fn handle_inner(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        if let Some(mut job) = self.open_job() {
            if is_stop(text) {
                job.state = State::Cancelled;
                job.outcome_text = "stopped by the user".into();
                self.store.save_job(&job)?;
                return Ok(vec![format!("Stopped the job in {}.", job.project)]);
            }
            match job.state {
                State::WaitingAnswer => {
                    let q = job.pending_questions.join(" / ");
                    job.answers.push((q, text.to_string()));
                    job.pending_questions.clear();
                    job.state = if job.plan.is_empty() { State::Planning } else { State::Working };
                    job.rejections = 0;
                    self.store.save_job(&job)?;
                    return self.run_turns(job);
                }
                State::WaitingApproval => {
                    let approved = is_yes(text);
                    return self.resume_after_approval(job, approved, text);
                }
                _ => {
                    // A job left mid-work (crash/restart): carry on with it.
                    return self.run_turns(job);
                }
            }
        }
        // Idle: the model decides — chat, or work.
        let p = prompt::front_door(&self.store.instructions()?, &self.store.list_projects()?, &self.store.recent_messages(4)?, text);
        match self.model.next_move(&p)? {
            Move::Reply { text, remember } => {
                let mut out = vec![text];
                if let Some(r) = remember { self.store.add_instruction(&r)?; out.push(format!("(Noted for the future: {r})")); }
                Ok(out)
            }
            Move::Start { project, new_project: _, description, goal, creative, understood, remember } => {
                let name = sanitize_project_name(&project);
                let existing = self.store.get_project(&name)?;
                if existing.is_none() { self.create_project_folder(&name)?; }
                let desc = existing.map(|p| p.description).unwrap_or(description);
                self.store.upsert_project(&name, &self.workspace(&name).display().to_string(), &desc)?;
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                let job = Job::new(&name, &goal, creative, &understood);
                self.store.save_job(&job)?;
                let mut out = vec![understood];
                out.extend(self.run_turns(job)?);
                Ok(out)
            }
            other => Ok(vec![format!("(I answered out of turn — {other:?} — please say that again.)")]),
        }
    }

    /// Task 8 fills this in.
    fn run_turns(&mut self, _job: Job) -> Result<Vec<String>, EngineError> { Ok(vec![]) }
    fn resume_after_approval(&mut self, _job: Job, _approved: bool, _text: &str) -> Result<Vec<String>, EngineError> { Ok(vec![]) }
}
```

- [ ] **Step 5: Run** — `cargo test -p aios-core` → 16 pass.

- [ ] **Step 6: Commit**
```bash
git add runtime/core/src
git commit -m "feat(core): engine front door — reply, start, remember, cancel, project folders"
```

---

### Task 8: Engine — the job loop with every rule

**Files:**
- Modify: `runtime/core/src/engine.rs` (replace the two stubs; add tests)

**Interfaces:**
- Consumes: everything above. `Executor::execute(job_id, &Action, approved) -> Result<ExecOutcome, LogError>`.
- Produces: `run_turns` and `resume_after_approval` — the loop of 1b spec §3–§4.

- [ ] **Step 1: Write the failing tests** (append inside `mod tests` in `engine.rs`):
```rust
    use executor::action::Action;
    use executor::worker::Outcome;

    fn write(path: &str) -> Action { Action::WriteFile { path: path.into(), contents: "x".into() } }
    fn run(cmd: &str) -> Action { Action::RunCommand { argv: vec![cmd.into()] } }
    fn plan() -> Move { Move::Plan { steps: vec!["write it".into(), "run it".into()] } }
    fn act(step: usize, a: Action) -> Move { Move::Act { step, action: a } }
    fn done(check: Action) -> Move { Move::Done { summary: "finished".into(), check } }

    /// A creative job that writes the blueprint, writes code, updates the blueprint, and proves it.
    fn happy_path() -> Vec<Move> {
        vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(1, write("primes.py")), act(1, write("BLUEPRINT.md")), done(run("python3"))]
    }

    #[test]
    fn full_job_runs_through_the_executor_and_ends_done() {
        let (mut e, rec, _) = engine_with(happy_path(), "happy");
        let out = e.handle("make it, decide yourself").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(e.open_job().is_none());
        assert_eq!(rec.calls.borrow().len(), 4, "3 acts + the check all went through the executor");
    }

    #[test]
    fn creative_start_goes_straight_to_planning() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "creative");
        e.handle("just make it, decide yourself").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[1].user.contains("Legal moves now: plan"), "no asking stage in creative mode: {}", prompts[1].user);
    }

    #[test]
    fn existing_project_is_reused_not_recreated() {
        let again = Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "add menu".into(), creative: true, understood: "Continuing p".into(), remember: None };
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
            again, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "reuse");
        e.handle("make p").unwrap();
        assert!(e.open_job().is_none());
        e.handle("add a menu to p").unwrap();
        assert_eq!(e.store.list_projects().unwrap().len(), 1, "same project row, not a second one");
        assert_eq!(e.store.list_projects().unwrap()[0].description, "prime printer", "the original description is kept");
    }

    #[test]
    fn stop_cancels_the_open_job_without_asking_the_model() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()] }], "stop");
        e.handle("make p").unwrap();
        assert_eq!(e.open_job().unwrap().state, State::WaitingAnswer);
        let out = e.handle("stop").unwrap();
        assert!(out[0].contains("Stopped"));
        assert!(e.open_job().is_none());
        assert_eq!(e.model.prompts.borrow().len(), 2, "cancel is deterministic, no model call");
    }

    #[test]
    fn asks_then_answer_resumes_and_answer_is_in_the_prompt() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false),
            Move::Ask { questions: vec!["Which language?".into()] },
            plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "ask");
        let out = e.handle("make it").unwrap();
        assert!(out.iter().any(|l| l.contains("Which language?")), "{out:?}");
        assert_eq!(e.open_job().unwrap().state, State::WaitingAnswer);
        let out = e.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("Which language? -> python"));
    }

    #[test]
    fn ask_in_creative_mode_is_rejected_then_model_complies() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true), Move::Ask { questions: vec!["?".into()] }, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "creative-ask");
        let out = e.handle("decide yourself").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: this job is in creative mode"), "rejection note reaches the model: {}", prompts[2].user);
    }

    #[test]
    fn act_before_plan_and_done_without_blueprint_update_are_rejected() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true),
            act(1, write("a.py")),                 // rejected: no plan yet
            plan(),
            act(1, write("BLUEPRINT.md")),
            act(1, write("a.py")),
            done(run("true")),                     // rejected: blueprint older than the code change
            act(1, write("BLUEPRINT.md")),
            done(run("true")),
        ], "order");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: give a plan first"), "act-before-plan note: {}", prompts[2].user);
        assert!(prompts[6].user.contains("rejected: update BLUEPRINT.md"), "blueprint note: {}", prompts[6].user);
    }

    #[test]
    fn failing_check_sends_the_model_back_to_work() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("python3")), done(run("python3")),
        ], "check");
        rec.outcomes.borrow_mut().extend([
            Outcome { ok: true, detail: "ok".into() },                 // blueprint write
            Outcome { ok: false, detail: "Traceback… NameError".into() }, // first check fails
            Outcome { ok: true, detail: "2 3 5 7".into() },             // second check passes
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.contains("NameError"), "the check's failure reason is fed back: {}", prompts[4].user);
    }

    #[test]
    fn identical_retry_of_a_failed_action_is_refused_with_the_reason() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            act(1, run("gcc")), act(1, run("gcc")),   // second one is identical → refused, not run
            act(1, run("cc")), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "retry");
        rec.outcomes.borrow_mut().extend([
            Outcome { ok: true, detail: "ok".into() },
            Outcome { ok: false, detail: "gcc: not found".into() },
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let calls = rec.calls.borrow();
        assert_eq!(calls.iter().filter(|a| **a == run("gcc")).count(), 1, "the identical retry never reached the executor");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[5].user.contains("gcc: not found"), "{}", prompts[5].user);
    }

    #[test]
    fn three_different_failures_on_one_step_give_up() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, run("a")), act(1, run("b")), act(1, run("c")), act(1, run("d")),
        ], "three");
        for _ in 0..4 { rec.outcomes.borrow_mut().push_back(Outcome { ok: false, detail: "boom".into() }); }
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), 3);
        assert!(e.open_job().is_none());
    }

    #[test]
    fn two_rejected_moves_in_a_row_fail_the_job() {
        let (mut e, _, _) = engine_with(vec![start("p", true), act(1, run("x")), act(1, run("x"))], "reject");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
    }

    #[test]
    fn step_cap_fails_the_job() {
        let mut moves = vec![start("p", true), plan()];
        for i in 0..30 { moves.push(act(1, write(&format!("f{i}")))); }
        let (mut e, rec, _) = engine_with(moves, "cap");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), 25);
    }

    #[test]
    fn replan_replaces_the_plan() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), Move::Replan { steps: vec!["other way".into()], why: "first way failed".into() },
            act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "replan");
        e.handle("go").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[3].user.contains("1. other way"));
        assert!(!prompts[3].user.contains("write it"));
    }

    #[test]
    fn give_up_reports_reason_and_missing() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), Move::GiveUp { reason: "no compiler".into(), missing: "gcc".into() }], "giveup");
        let out = e.handle("go").unwrap();
        let last = out.last().unwrap();
        assert!(last.contains("no compiler") && last.contains("gcc"), "{last}");
    }

    #[test]
    fn risky_action_waits_for_ok_and_runs_after_yes() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()), done(run("true")),
        ], "approve");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("Needs your OK"), "{out:?}");
        assert_eq!(e.open_job().unwrap().state, State::WaitingApproval);
        assert_eq!(rec.calls.borrow().len(), 1, "the risky action did not run");
        let out = e.handle("yes, send it").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(rec.calls.borrow().contains(&post));
    }

    #[test]
    fn declined_risky_action_is_told_to_the_model() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()), done(run("true")),
        ], "decline");
        e.handle("go").unwrap();
        let out = e.handle("no, don't").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!rec.calls.borrow().contains(&post));
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.to_lowercase().contains("declined"), "{}", prompts[4].user);
    }

    #[test]
    fn a_waiting_job_resumes_from_the_store_after_a_restart() {
        let (mut e, _, root) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Language?".into()] }], "restart");
        e.handle("make it").unwrap();
        let store = std::mem::replace(&mut e.store, crate::store::Store::open_in_memory().unwrap());
        drop(e);
        // New engine, same store: the answer must land on the saved job.
        let rec = crate::testing::Recorder::default();
        let r2 = rec.clone();
        let mut e2 = Engine::new(store, crate::model::FakeModel::new(vec![plan(), act(1, write("BLUEPRINT.md")), done(run("true"))]), root, None,
            Box::new(move |_| Box::new(crate::testing::ScriptedWorker(r2.clone())) as Box<dyn Worker>));
        let out = e2.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(e2.open_job().is_none());
    }
```

- [ ] **Step 2: Run to see them fail** — `cargo test -p aios-core` → the new tests fail (stubs return nothing).

- [ ] **Step 3: Implement the loop** — replace the two stubs in `engine.rs` with:
```rust
    const MAX_STEPS: usize = 25;
    const MAX_FAILS_PER_STEP: usize = 3;
    const MAX_REJECTIONS: u32 = 2;

    fn finish(&self, mut job: Job, state: State, text: String) -> Result<Vec<String>, EngineError> {
        job.state = state;
        job.outcome_text = text.clone();
        self.store.save_job(&job)?;
        Ok(vec![text])
    }

    fn reject(&self, job: &mut Job, why: &str) -> Result<Option<Vec<String>>, EngineError> {
        job.rejections += 1;
        job.note_to_model = Some(format!("your last move was rejected: {why}"));
        if job.rejections >= Self::MAX_REJECTIONS {
            let text = format!("I gave up on {}: I kept answering in a way the system could not accept ({why}).", job.project);
            return Ok(Some(self.finish(job.clone(), State::Failed, text)?));
        }
        self.store.save_job(job)?;
        Ok(None)
    }

    fn is_blueprint(action: &Action) -> Option<bool> {
        match action {
            Action::WriteFile { path, .. } | Action::EditFile { path, .. } => Some(Path::new(path).file_name().map(|n| n == "BLUEPRINT.md").unwrap_or(false)),
            _ => None,
        }
    }

    /// Run one action through the executor's door and record what happened on the job.
    /// Returns the lines to show the user if the job must stop here (waiting for OK / gave up).
    fn perform(&self, job: &mut Job, plan_step: usize, action: Action, approved: bool) -> Result<Option<Vec<String>>, EngineError> {
        let key = serde_json::to_string(&action).unwrap_or_default();
        if !approved && job.failed_actions.contains(&key) {
            let earlier = job.steps.iter().rev().find(|s| serde_json::to_string(&s.action).unwrap_or_default() == key).map(|s| s.detail.clone()).unwrap_or_default();
            return self.reject(job, &format!("that exact action already failed with: {earlier} — work around it or replan"));
        }
        let exec = self.executor_for(&job.project)?;
        match exec.execute(&job.id, &action, approved)? {
            ExecOutcome::Blocked(reason) => {
                job.pending_action = Some((plan_step, action));
                job.pending_reason = reason.clone();
                job.state = State::WaitingApproval;
                self.store.save_job(job)?;
                Ok(Some(vec![format!("Needs your OK: {reason}. Say yes to allow it, or anything else to refuse.")]))
            }
            ExecOutcome::Ran(outcome) => {
                job.rejections = 0;
                job.note_to_model = None;
                job.steps.push(StepRecord { plan_step, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
                if outcome.ok {
                    match Self::is_blueprint(&action) {
                        Some(true) => job.last_blueprint_update = job.steps.len(),
                        Some(false) => job.last_code_change = job.steps.len(),
                        None => {}
                    }
                } else {
                    job.failed_actions.push(key);
                    let fails = job.steps.iter().filter(|s| s.plan_step == plan_step && !s.ok).count();
                    if fails >= Self::MAX_FAILS_PER_STEP {
                        let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", job.project, outcome.detail);
                        return Ok(Some(self.finish(job.clone(), State::Failed, text)?));
                    }
                }
                self.store.save_job(job)?;
                Ok(None)
            }
        }
    }

    fn resume_after_approval(&mut self, mut job: Job, approved: bool, text: &str) -> Result<Vec<String>, EngineError> {
        let (plan_step, action) = match job.pending_action.take() { Some(p) => p, None => { job.state = State::Working; return self.run_turns(job); } };
        job.state = State::Working;
        if approved {
            if let Some(stop) = self.perform(&mut job, plan_step, action, true)? { return Ok(stop); }
        } else {
            job.note_to_model = Some(format!("the user declined that action ({}): \"{text}\". Do not repeat it; find another way or finish without it.", job.pending_reason));
            job.failed_actions.push(serde_json::to_string(&action).unwrap_or_default());
            self.store.save_job(&job)?;
        }
        self.run_turns(job)
    }

    /// Turn after turn until the job is done, failed, or needs the user (1b spec §3–§4).
    fn run_turns(&mut self, mut job: Job) -> Result<Vec<String>, EngineError> {
        loop {
            if job.steps.len() >= Self::MAX_STEPS {
                return self.finish(job, State::Failed, format!("I gave up on {}: {} steps without finishing.", job.project, Self::MAX_STEPS));
            }
            let p = prompt::job_turn(&self.store.instructions()?, &job, self.read_blueprint(&job.project).as_deref());
            let mv = self.model.next_move(&p)?;
            let rejected = match (job.state, mv) {
                (State::Asking, Move::Ask { questions }) | (State::Working, Move::Ask { questions }) if !job.creative => {
                    job.pending_questions = questions.clone();
                    job.state = State::WaitingAnswer;
                    job.rejections = 0;
                    job.note_to_model = None;
                    self.store.save_job(&job)?;
                    return Ok(questions.iter().map(|q| format!("Question: {q}")).collect());
                }
                (_, Move::Ask { .. }) if job.creative => Some("this job is in creative mode: decide yourself instead of asking".to_string()),
                (_, Move::Ask { .. }) => Some("ask only before planning or while working".to_string()),
                (State::Asking, Move::Plan { steps }) | (State::Planning, Move::Plan { steps }) => {
                    job.plan = steps; job.state = State::Working; job.rejections = 0; job.note_to_model = None;
                    self.store.save_job(&job)?; None
                }
                (State::Working, Move::Plan { .. }) => Some("you already have a plan; use replan to change it".to_string()),
                (State::Working, Move::Replan { steps, why }) => {
                    job.plan = steps; job.rejections = 0;
                    job.note_to_model = Some(format!("plan revised because: {why}"));
                    self.store.save_job(&job)?; None
                }
                (State::Working, Move::Act { step, action }) => {
                    if let Some(stop) = self.perform(&mut job, step, action, false)? { return Ok(stop); }
                    None
                }
                (State::Working, Move::Done { summary, check }) => {
                    if job.last_code_change > job.last_blueprint_update {
                        Some("update BLUEPRINT.md for what you changed before saying done".to_string())
                    } else {
                        let key_step = job.plan.len().max(1);
                        match self.perform(&mut job, key_step, check, false)? {
                            Some(stop) => return Ok(stop),
                            None => {
                                if job.steps.last().map(|s| s.ok).unwrap_or(false) {
                                    return self.finish(job, State::Done, summary);
                                }
                                job.note_to_model = Some("your check failed — read its output above, fix the work, then say done again with a check".to_string());
                                self.store.save_job(&job)?;
                                None
                            }
                        }
                    }
                }
                (State::Working, Move::GiveUp { reason, missing }) => {
                    return self.finish(job.clone(), State::Failed, format!("I gave up on {}: {reason}. Missing: {missing}.", job.project));
                }
                (_, Move::Act { .. }) | (_, Move::Done { .. }) | (_, Move::Replan { .. }) | (_, Move::GiveUp { .. }) => Some("give a plan first".to_string()),
                (_, Move::Plan { .. }) => Some("not now".to_string()),
                (_, Move::Reply { .. }) | (_, Move::Start { .. }) => Some("a job is running: use ask, plan, act, replan, done or give_up".to_string()),
            };
            if let Some(why) = rejected {
                if let Some(stop) = self.reject(&mut job, &why)? { return Ok(stop); }
            }
        }
    }
```
Add the needed imports at the top of `engine.rs`: `use crate::job::StepRecord; use executor::action::Action; use executor::executor::ExecOutcome;`.

Note for the implementer: `perform`'s `plan_step` for the check uses the last plan step; the `Done` arm rejects with a blueprint note *before* running the check, so an unrecorded blueprint costs no executor call.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → all pass (16 + 17). If a test's `prompts[i]` index is off by one because of an extra rejected turn, fix the *test's* index only after confirming the engine's behaviour matches the spec — never loosen the assertion.

- [ ] **Step 5: Commit**
```bash
git add runtime/core/src/engine.rs
git commit -m "feat(core): job loop — ask/plan/act/replan/done-with-check, all loop rules, approvals, resume"
```

---

### Task 9: `ai-os-chat` binary and the live acceptance test

**Files:**
- Modify: `runtime/core/src/main.rs`
- Create: `runtime/core/tests/live_primes.rs`
- Modify: `docs/superpowers/specs/2026-09-16-phase1b-core-loop-design.md` (§11 status line) — after the run

**Interfaces:**
- Consumes: `Engine`, `OllamaModel`, `Store`, `SandboxWorker`.
- Produces: `ai-os-chat` — reads a line from stdin, prints the engine's lines; `AI_OS_DB` (default `/data/ai-os.db`), `AI_OS_MODEL` (default `qwen3.5:9b`), `AI_OS_PROJECTS` (default `/data/projects`).

- [ ] **Step 1: Write the live test** `runtime/core/tests/live_primes.rs`:
```rust
// The Phase 1b acceptance test (1b spec §9): the real local model, the real sandbox, one job,
// no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_primes -- --nocapture
use aios_core::engine::Engine;
use aios_core::job::State;
use aios_core::model::OllamaModel;
use aios_core::store::Store;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

#[test]
fn the_model_writes_and_proves_a_primes_script() {
    if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
    let root = PathBuf::from("/data/projects");
    let db = "/data/ai-os-live.db";
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root.join("primes"));
    let store = Store::open(db).unwrap();
    let mut e = Engine::new(store, OllamaModel::local("qwen3.5:9b"), root.clone(), Some(db.into()),
        Box::new(|ws| Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>));
    let mut out = e.handle("Start a new project called primes: make a Python script that prints the first ten prime numbers, one per line, and prove it runs. Decide the details yourself.").unwrap();
    for line in &out { eprintln!("AI: {line}"); }
    // If it asks anyway, answer once; a second question is a failure of the creative rule.
    if e.open_job().map(|j| j.state == State::WaitingAnswer).unwrap_or(false) {
        out = e.handle("You decide.").unwrap();
        for line in &out { eprintln!("AI: {line}"); }
    }
    let job = e.store.open_job().unwrap();
    assert!(job.is_none(), "job should be finished, still open: {job:?}");
    let name = e.store.list_projects().unwrap()[0].name.clone();
    let folder = root.join(&name);
    assert!(folder.join("BLUEPRINT.md").exists(), "blueprint must exist");
    let py = std::fs::read_dir(&folder).unwrap().filter_map(|d| d.ok()).any(|d| d.path().extension().map(|x| x == "py").unwrap_or(false));
    assert!(py, "a .py file must exist in {folder:?}");
    assert!(out.last().unwrap().to_lowercase().contains("gave up") == false, "{out:?}");
    // The proof: the check's real output holds the tenth prime.
    let last_line = out.last().unwrap().clone();
    eprintln!("RESULT: {last_line}");
}
```

- [ ] **Step 2: Write the binary** `runtime/core/src/main.rs`:
```rust
use aios_core::engine::Engine;
use aios_core::model::OllamaModel;
use aios_core::store::Store;
use executor::worker::{SandboxWorker, Worker};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn main() {
    let db = std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into());
    let model = std::env::var("AI_OS_MODEL").unwrap_or_else(|_| "qwen3.5:9b".into());
    let root = PathBuf::from(std::env::var("AI_OS_PROJECTS").unwrap_or_else(|_| "/data/projects".into()));
    let store = Store::open(&db).expect("open store");
    let mut engine = Engine::new(store, OllamaModel::local(&model), root, Some(db),
        Box::new(|ws| Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>));
    // Builder's front (1b spec §9): the 1d rail draws this same conversation as cards.
    let stdin = std::io::stdin();
    loop {
        print!("you> "); std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 { break; }
        let text = line.trim();
        if text.is_empty() { continue; }
        match engine.handle(text) {
            Ok(lines) => for l in lines { println!("ai> {l}"); },
            Err(e) => println!("ai> (something went wrong: {e})"),
        }
    }
}
```

- [ ] **Step 3: Build and unit-test** — `cd runtime && cargo build && cargo test` → everything passes; the live tests skip.

- [ ] **Step 4: Run the acceptance test inside the distro** (Ollama running; `sudo bash trial/setup-sandbox-user.sh` done once):
`cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && AI_OS_LIVE=1 cargo test -p aios-core --test live_primes -- --nocapture`
Expected: PASS, printing the AI's lines and `RESULT: …`. If the model misbehaves (asks twice, never says done), that is a **finding about the prompt or the model, not the loop** — record what it did in the commit message and adjust `prompt::SYSTEM` wording only; never loosen the loop's rules.

- [ ] **Step 5: Show the record.** Print the job's story for the report: `sqlite3 /data/ai-os-live.db "select json from core_jobs"` and `select action_json, outcome from actions` — keep these outputs for the final report (plain-language summary + the record).

- [ ] **Step 6: Commit**
```bash
git add runtime/core/src/main.rs runtime/core/tests/live_primes.rs
git commit -m "feat(core): ai-os-chat builder front + live primes acceptance test"
```

---

## Self-review

**Spec coverage (1b spec):** §2 conversation → Task 7 (front door; stop/answer/approval routing) ✓. §3 job life, modes, limits, resume → Tasks 5, 8 ✓. §4 moves and every loop rule → Tasks 3, 8 (tests named per rule) ✓; failure tails → Task 1 ✓. §5 model connection → Task 4 ✓. §6.1 instructions → Tasks 5, 6, 7 (`remember`) ✓; §6.2 blueprint read/enforce → Tasks 6, 8 ✓; §6.3 job record → Task 5 ✓; §6.4 projects → Tasks 5, 7 ✓; §6.5 replan / give_up-with-missing → Tasks 3, 8 ✓. §7 jail + shared folder + one workspace reference → Tasks 1, 7 (`executor_for`) ✓. §8 install hands → 1c, out of scope by design. §9 proof → Tasks 8 (fake) and 9 (live) ✓. `edit_file` + windowed `read_file` → Task 2 ✓.

**Placeholder scan:** none; every step has code. Task 1's fallback mount recipe is explicit.

**Type consistency:** `Move` variants and field names match between `moves.rs`, `schema.rs`, `prompt.rs` tests and `engine.rs`; `Action::ReadFile` has three fields everywhere after Task 2; `Engine::new` signature identical in Tasks 7, 8 (restart test) and 9; `Recorder`/`ScriptedWorker` used consistently. `Store::open_job` vs `Engine::open_job` — both exist, the engine's wraps the store's.
