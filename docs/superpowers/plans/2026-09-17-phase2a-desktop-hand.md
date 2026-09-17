# Phase 2a — The Desktop Hand Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give the model a third hand that works any program's controls through the accessibility bus — `look`, `press`, `type`, `read`, `open_app` — with nothing per-app, decision 9 applied to a press as code, the invisible session as a unit, and a live acceptance on a window the user opened and one the AI opened.

**Architecture:** The `executor` crate gains five `Action` variants, a `Lane::Desktop`, a fixed risky-press word list in `rules`, and a `DesktopWorker` over zbus's blocking API with four hand-written AT-SPI proxies; its id table and bus connection live in an `Rc<RefCell<DesktopState>>` the engine's worker factory captures, because the engine builds workers fresh per action. `aios-core` widens `housekeep` to cover a window, exempts repeated presses from the "just succeeded" refusal, computes the windows a job worked at Done, and asks the model for its context size to set the look cap. `proto` and the rail carry `windows` on `done` and print the one-line undo notice. Two systemd user units (the invisible shell, the engine after it) and one setup script wire the workshop.

**Tech Stack:** Rust 2021 workspace at `runtime/` (rustc 1.98); `zbus` 5 with the `blocking-api` feature (new, executor only); serde/serde_json (`preserve_order`), rusqlite, ureq, gtk4 (existing); GNOME Shell 50 headless with `--wayland-display`; `gtk-launch`; systemd user units; bash setup script.

**Spec:** `docs/superpowers/specs/2026-09-17-phase2a-desktop-hand-design.md` (read first; §3 action table, §4 budget, §5 wiring, §6 word list, §12 proofs, §14 audit are the contract). Parent spec `2026-09-15-ai-os-design.md` §4.4, §4.7, decisions 8, 9, 12.

## Global Constraints

- **Nothing per-app.** No application name appears in the executor, the engine or the prompt except as an example of a desktop-entry id. A test may name an app; the code never does.
- The discriminator is the first key of every JSON object the model or a client parses: `kind` on actions and events, `move` on moves. A new `Action` variant is added to `MOVE_SCHEMA` in the same order and name, and `action_kinds_match_the_enum` pins it.
- `classify` stays a pure function of `(action, workspace)`. The worker verifies a press's echoed `name` against its table before anything runs.
- `zbus` is linked only in `executor`; `proto` and the rail build without it. Nothing runs as root except the existing wrapper; the desktop worker runs inside the engine process as user `ai`.
- Every existing test keeps passing unedited (executor 16+4, core 93+, proto, rail). A failing existing test is a finding about the code. Never adjust a test to make it pass.
- All text files LF. Commit after every task on branch `phase2a-desktop-hand`; never push, tag or merge without his word.
- Tests run inside the distro (no Rust on Windows), through a script file (Git Bash rewrites `/mnt/c` paths; PowerShell mangles quotes): write the command to `trial/run-tests.sh` once (Task 1) and run it with PowerShell: `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh <cargo args…>`. Root-only steps go through `wsl -d ai-os -u root -- bash -l /mnt/c/…/trial/setup-desktop.sh`.
- The distro dies about 60 s after the last `wsl` command exits (Phase 0 finding); a long gap between commands means a cold start. Nothing here depends on state in `/tmp`.

---

## File map

| File | Responsibility |
|---|---|
| `runtime/executor/Cargo.toml` | `zbus = { version = "5", features = ["blocking-api"] }` |
| `runtime/executor/src/action.rs` | `Look`, `Press`, `Type`, `Read`, `OpenApp`; `valid_app_name` |
| `runtime/executor/src/rules.rs` | `RISKY_PRESS`, `risky_press`, `classify` arms |
| `runtime/executor/src/executor.rs` | `Lane::Desktop`, third worker, routing |
| `runtime/executor/src/desktop.rs` (new) | the pure half: `Node`, `IdTable`, `select`, `render_look`, `look_cap`, `Displays` |
| `runtime/executor/src/atspi.rs` (new) | the bus half: zbus proxies, `DesktopState`, `DesktopWorker` |
| `runtime/executor/tests/desktop_live.rs` (new) | the gated live check of the worker on a real app |
| `runtime/core/src/engine.rs` | factory triple, `executor_for`, repeat exemption, `windows` at Done |
| `runtime/core/src/testing.rs` | `Recorder.desktop_calls`, `DesktopRecorder`, triple factory |
| `runtime/core/src/model.rs` | `Model::context_tokens` |
| `runtime/core/src/event.rs` | `describe` for the five; the undo note line |
| `runtime/core/src/prompt.rs` | hand rules; `housekeep` widened; `compact_action` for `type` |
| `runtime/core/src/schema.rs` | five action entries |
| `runtime/core/src/bin/ai-os-engine.rs`, `tests/live_1c.rs`, `tests/live_1d.rs`, `tests/live_primes.rs` | factory triple |
| `runtime/core/tests/live_2a.rs` (new) | the live acceptance |
| `runtime/proto/src/lib.rs` | `Done { windows }`, `window_note` |
| `runtime/rail/src/cards.rs`, `src/main.rs`, `tests/cards.rs` | Done card with windows and the note |
| `trial/ai-os-desktop.service`, `trial/ai-os-desktop-env`, `trial/setup-desktop.sh` (new), `trial/ai-os-engine.service`, `trial/run-tests.sh` (new) | workshop wiring |

---

### Task 1: The five actions in the executor and the grammar

**Files:**
- Modify: `runtime/executor/src/action.rs`
- Modify: `runtime/core/src/schema.rs`
- Modify: `runtime/core/src/event.rs` (`describe`)
- Modify: `runtime/core/src/prompt.rs` (`compact_action`)
- Create: `trial/run-tests.sh`

**Interfaces:**
- Produces: `Action::Look { window: Option<String>, find: Option<String> }`, `Action::Press { control: u32, name: String }`, `Action::Type { control: u32, text: String, replace: bool }`, `Action::Read { control: u32, from_line: Option<usize>, lines: Option<usize> }`, `Action::OpenApp { name: String, visible: bool }`; `pub fn valid_app_name(&str) -> bool`. Wire names: `look`, `press`, `type`, `read`, `open_app`.

- [ ] **Step 1: The test runner script** — `trial/run-tests.sh`:

```bash
#!/usr/bin/env bash
# Runs cargo inside the distro from the Windows-mounted repo. Usage: run-tests.sh test -p executor
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo "$@" 2>&1 | grep -vE '^\s+(Compiling|Checking|Finished|Running)' | tail -60
```

Check: `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p executor` prints `test result: ok` twice (unit + integration).

- [ ] **Step 2: Failing tests** in `runtime/executor/src/action.rs` `mod tests`:

```rust
    #[test]
    fn desktop_actions_round_trip_with_kind_first() {
        let a = Action::Press { control: 7, name: "Bold".into() };
        let s = serde_json::to_string(&a).unwrap();
        assert!(s.starts_with(r#"{"kind":"press""#), "{s}");
        assert_eq!(serde_json::from_str::<Action>(&s).unwrap(), a);
        let t: Action = serde_json::from_str(r#"{"kind":"type","control":3,"text":"hi"}"#).unwrap();
        assert_eq!(t, Action::Type { control: 3, text: "hi".into(), replace: false });
        let o: Action = serde_json::from_str(r#"{"kind":"open_app","name":"org.gnome.Calculator"}"#).unwrap();
        assert_eq!(o, Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false });
        let l: Action = serde_json::from_str(r#"{"kind":"look"}"#).unwrap();
        assert_eq!(l, Action::Look { window: None, find: None });
        let r: Action = serde_json::from_str(r#"{"kind":"read","control":9,"from_line":5,"lines":20}"#).unwrap();
        assert_eq!(r, Action::Read { control: 9, from_line: Some(5), lines: Some(20) });
    }

    #[test]
    fn app_names_carry_dots_and_capitals_but_never_paths() {
        for ok in ["org.gnome.TextEditor", "org.gnome.Calculator", "libreoffice-writer", "gnome_calc2"] { assert!(valid_app_name(ok), "{ok}"); }
        for bad in ["", "../x", "a/b", "a b", ".hidden", "-x", "x;rm"] { assert!(!valid_app_name(bad), "{bad}"); }
    }
```

- [ ] **Step 3: Run** `run-tests.sh test -p executor action` — FAIL: no variant `Press`, no `valid_app_name`.

- [ ] **Step 4: Implement** in `action.rs`, after `SetSetting { key: String, value: String },` inside the enum:

```rust
    // The desktop hand (2a design §3). Ids come from the worker's own table; `name` on a press is
    // the model echoing what `look` reported, verified by the worker before anything runs, so the
    // risk rule can read a name without leaving `classify` pure.
    /// No window: list the open windows. A window: its controls, narrowed by `find`.
    Look {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        find: Option<String>,
    },
    Press { control: u32, name: String },
    /// Appends at the end; `replace` clears the control first.
    Type {
        control: u32,
        text: String,
        #[serde(default)]
        replace: bool,
    },
    /// Windowed like `ReadFile`.
    Read {
        control: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_line: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        lines: Option<usize>,
    },
    /// A desktop-entry id (`org.gnome.TextEditor`); on the invisible display unless `visible`.
    OpenApp {
        name: String,
        #[serde(default)]
        visible: bool,
    },
```

and after `valid_name`:

```rust
/// A desktop-entry id: `^[A-Za-z0-9][A-Za-z0-9._-]*$`. Dots and capitals are normal there
/// (`org.gnome.TextEditor`); a slash, a space or a leading dot or dash never is.
pub fn valid_app_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && b[0].is_ascii_alphanumeric() && b.iter().all(|&c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
}
```

`serde_json` is not yet a dependency of `executor`'s tests? It is (`serde_json = "1"` in its Cargo.toml). The `match` arms in `rules.rs`, `executor.rs` and `event.rs` now fail to compile: that is the point (listed, not `_`). Add the minimal arms so this task compiles, and Task 2 gives them their real bodies:

- `rules.rs` `classify`: `Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. } => Risk::Auto,`
- `executor.rs` `lane`: add `Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. } => Lane::Sandbox,` (temporary; Task 2 replaces it).
- `core/src/event.rs` `describe`:

```rust
        Action::Look { window: None, .. } => "looked at the open windows".into(),
        Action::Look { window: Some(w), find: None } => format!("looked at {w}"),
        Action::Look { window: Some(w), find: Some(f) } => format!("looked for {f} in {w}"),
        Action::Press { name, .. } => format!("pressed {name}"),
        Action::Type { text, control, .. } => format!("typed {} into control {control}", plural(text.lines().count().max(1), "line")),
        Action::Read { control, .. } => format!("read control {control}"),
        Action::OpenApp { name, .. } => format!("opened {name}"),
```

with, in `event.rs`, `fn plural(n: usize, w: &str) -> String { if n == 1 { format!("1 {w}") } else { format!("{n} {w}s") } }`.

- `core/src/prompt.rs` `compact_action`, before `other =>`: `Action::Type { control, text, .. } => format!("type into {control} ({} chars)", text.chars().count()),`.

- [ ] **Step 5: The grammar** — `runtime/core/src/schema.rs`, after the `set_setting` entry (comma on the previous line):

```json
      { "type":"object", "properties": { "kind": {"enum":["look"]}, "window": {"type":"string"}, "find": {"type":"string"} }, "required":["kind"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["press"]}, "control": {"type":"integer","minimum":1}, "name": {"type":"string"} }, "required":["kind","control","name"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["type"]}, "control": {"type":"integer","minimum":1}, "text": {"type":"string"}, "replace": {"type":"boolean"} }, "required":["kind","control","text"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["read"]}, "control": {"type":"integer","minimum":1}, "from_line": {"type":"integer","minimum":1}, "lines": {"type":"integer","minimum":1} }, "required":["kind","control"], "additionalProperties": false },
      { "type":"object", "properties": { "kind": {"enum":["open_app"]}, "name": {"type":"string"}, "visible": {"type":"boolean"} }, "required":["kind","name"], "additionalProperties": false }
```

and extend `action_kinds_match_the_enum`'s expected list with `"look", "press", "type", "read", "open_app"` after `"set_setting"`.

- [ ] **Step 6: A describe test** in `core/src/event.rs` tests (create `mod tests` if none):

```rust
    #[test]
    fn desktop_steps_read_as_plain_words() {
        assert_eq!(describe(&Action::Press { control: 4, name: "Bold".into() }), "pressed Bold");
        assert_eq!(describe(&Action::Type { control: 2, text: "a\nb".into(), replace: false }), "typed 2 lines into control 2");
        assert_eq!(describe(&Action::Look { window: Some("Text Editor".into()), find: None }), "looked at Text Editor");
        assert_eq!(describe(&Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }), "opened org.gnome.Calculator");
    }
```

- [ ] **Step 7: Run** `run-tests.sh test -p executor` and `run-tests.sh test -p aios-core` — all green, including `move_is_always_the_first_key` and `action_kinds_match_the_enum`.

- [ ] **Step 8: Commit**

```bash
git checkout -b phase2a-desktop-hand
git add runtime/executor/src/action.rs runtime/executor/src/rules.rs runtime/executor/src/executor.rs runtime/core/src/schema.rs runtime/core/src/event.rs runtime/core/src/prompt.rs trial/run-tests.sh
git commit -m "feat(executor): the five desktop actions, their grammar and their words"
```

---

### Task 2: Decision 9 for a press, the desktop lane, the third worker

**Files:**
- Modify: `runtime/executor/src/rules.rs`
- Modify: `runtime/executor/src/executor.rs`
- Modify: `runtime/executor/src/worker.rs` (`FakeWorker` unchanged; doc line)

**Interfaces:**
- Produces: `rules::risky_press(name: &str) -> Option<&'static str>` (the reason, or none); `executor::Lane::Desktop`; `Executor::new(sandbox: W, admin: W, desktop: W, log, workspace)`.
- Consumes: Task 1's variants.

- [ ] **Step 1: Failing tests** in `rules.rs` `mod tests`:

```rust
    #[test]
    fn a_press_is_free_unless_its_name_means_leaving_or_destroying() {
        let press = |n: &str| Action::Press { control: 1, name: n.into() };
        assert_eq!(classify(&press("Bold"), &ws()), Risk::Auto);
        assert_eq!(classify(&press("File"), &ws()), Risk::Auto);
        for n in ["Send", "send email", "Publish", "Upload", "Share", "Pay now", "Buy", "Submit", "Order"] {
            assert!(matches!(classify(&press(n), &ws()), Risk::NeedsConfirm(r) if r.contains("leaves the machine")), "{n}");
        }
        for n in ["Close", "Quit", "Discard", "Don't Save", "Don’t save", "Delete", "Revert", "Replace", "close window"] {
            assert!(matches!(classify(&press(n), &ws()), Risk::NeedsConfirm(r) if r.contains("unsaved work")), "{n}");
        }
        // A word inside another word is not the word: "Closed captions" is not Close.
        assert_eq!(classify(&press("Closed captions"), &ws()), Risk::Auto);
    }

    #[test]
    fn the_other_desktop_actions_are_free_and_open_app_checks_its_name() {
        assert_eq!(classify(&Action::Look { window: None, find: None }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::Type { control: 1, text: "x".into(), replace: false }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::Read { control: 1, from_line: None, lines: None }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::OpenApp { name: "org.gnome.Calculator".into(), visible: true }, &ws()), Risk::Auto);
        assert!(matches!(classify(&Action::OpenApp { name: "../x".into(), visible: false }, &ws()), Risk::NeedsConfirm(_)));
    }
```

and in `executor.rs` `mod tests` (the existing `exec` helper gains a third `FakeWorker`; every existing `Executor::new(...)` call in that module gets one too):

```rust
    #[test]
    fn desktop_actions_take_the_desktop_lane() {
        let ws = PathBuf::from("/data/jobs/j1");
        for a in [Action::Look { window: None, find: None }, Action::Press { control: 1, name: "Bold".into() },
                  Action::Type { control: 1, text: "x".into(), replace: false }, Action::Read { control: 1, from_line: None, lines: None },
                  Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }] {
            assert_eq!(lane(&a, &ws, false), Lane::Desktop, "{a:?}");
            assert_eq!(lane(&a, &ws, true), Lane::Desktop, "{a:?}");
        }
    }

    #[test]
    fn a_press_runs_on_the_desktop_worker_and_a_risky_one_is_blocked_until_approved() {
        let e = exec(true);
        let ok = Action::Press { control: 1, name: "Bold".into() };
        assert!(matches!(e.execute("j", &ok, false).unwrap(), ExecOutcome::Ran(o) if o.ok));
        assert_eq!(e.desktop.calls.borrow().as_slice(), &[ok.clone()]);
        assert!(e.sandbox.calls.borrow().is_empty() && e.admin.calls.borrow().is_empty());
        let close = Action::Press { control: 2, name: "Close".into() };
        assert!(matches!(e.execute("j", &close, false).unwrap(), ExecOutcome::Blocked(r) if r.contains("Close")));
        assert!(matches!(e.execute("j", &close, true).unwrap(), ExecOutcome::Ran(_)));
        assert_eq!(e.desktop.calls.borrow().len(), 2);
    }
```

- [ ] **Step 2: Run** `run-tests.sh test -p executor` — FAIL (no `Lane::Desktop`, wrong arity, `Bold` classified... the temporary arm makes the press test fail on `Close`).

- [ ] **Step 3: Implement** — `rules.rs`, above `classify`:

```rust
/// Decision 9 for a press, as code: a control whose name means leaving the machine or
/// destroying unsaved work is asked about first. Whole words, case-insensitive, so "Closed
/// captions" is not "Close". Ceiling, stated in the design: a button called "Go" that sends
/// is not caught — this is the rule the executor can apply without asking the model.
pub const LEAVES_MACHINE: [&str; 9] = ["send", "post", "publish", "upload", "share", "pay", "buy", "submit", "order"];
pub const DESTROYS_WORK: [&str; 7] = ["close", "quit", "discard", "don't save", "delete", "revert", "replace"];

pub fn risky_press(name: &str) -> Option<&'static str> {
    // Curly apostrophes and case folded; then whole-word containment on a padded string.
    let folded: String = name.to_lowercase().replace(['’', '‘'], "'");
    let words: Vec<&str> = folded.split(|c: char| !c.is_alphanumeric() && c != '\'').filter(|w| !w.is_empty()).collect();
    let has = |phrase: &str| {
        let p: Vec<&str> = phrase.split(' ').collect();
        words.windows(p.len()).any(|w| w == p.as_slice())
    };
    if LEAVES_MACHINE.iter().any(|p| has(p)) { return Some("leaves the machine"); }
    if DESTROYS_WORK.iter().any(|p| has(p)) { return Some("may throw away unsaved work"); }
    None
}
```

and the `classify` arms (replacing Task 1's temporary one):

```rust
        // The desktop hand (2a §6). `look`, `read` and `type` change nothing the user has not
        // already put in front of a program; a press is judged by the name the worker verifies.
        Action::Look { .. } | Action::Type { .. } | Action::Read { .. } => Risk::Auto,
        Action::Press { name, .. } => match risky_press(name) {
            Some(why) => Risk::NeedsConfirm(format!("press {name}: it {why}")),
            None => Risk::Auto,
        },
        Action::OpenApp { name, .. } => if valid_app_name(name) { Risk::Auto } else { Risk::NeedsConfirm(format!("invalid application name: {name}")) },
```

(`use crate::action::{valid_app_name, valid_name, Action};` at the top.) Note "don't save" as two words: the splitter keeps the apostrophe inside `don't`, so the phrase is `["don't", "save"]` and matches `Don’t save` after the curly fold.

`executor.rs`: `Lane::Desktop` variant; in `lane`, replace the temporary arm with `Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. } => Lane::Desktop,`; the struct gains `desktop: W` (pub(crate) fields stay as they are — the tests are in-module); `pub fn new(sandbox: W, admin: W, desktop: W, log: ActionLog, workspace: PathBuf)`; in `execute`'s worker match: `Lane::Desktop => &self.desktop,`. Update the `exec` helper: `Executor::new(FakeWorker::new(ok), FakeWorker::new(ok), FakeWorker::new(ok), ...)`.

Everything in `core` that calls `Executor::new` (`engine.rs::executor_for`) now fails to compile: Task 3 fixes it. To keep this task's commit green, run only `-p executor` here.

- [ ] **Step 4: Run** `run-tests.sh test -p executor` — green (the 16+4 plus the new four).

- [ ] **Step 5: Commit**

```bash
git add runtime/executor
git commit -m "feat(executor): the desktop lane, a third worker, and decision 9 for a press as a word list"
```

---

### Task 3: The engine takes three hands (and a repeated press is legal)

**Files:**
- Modify: `runtime/core/src/engine.rs` (`WorkerFactory`, `executor_for`, `perform`)
- Modify: `runtime/core/src/testing.rs`
- Modify: `runtime/core/src/bin/ai-os-engine.rs`, `runtime/core/tests/live_1c.rs`, `live_1d.rs`, `live_primes.rs`

**Interfaces:**
- Produces: `pub type WorkerFactory = Box<dyn Fn(&Path) -> (Box<dyn Worker>, Box<dyn Worker>, Box<dyn Worker>)>` (sandbox, admin, desktop); `testing::Recorder.desktop_calls: Rc<RefCell<Vec<Action>>>`, `desktop_outcomes`; `testing::DesktopRecorder(pub Recorder)`.

- [ ] **Step 1: Failing engine test** in `engine.rs` `mod tests` (uses the module's `engine_with`, `start`, `plan`, `act`, `done`, `run` helpers already there):

```rust
    /// 2a §5: a repeated digit or a second Next is normal desktop work — 1d's "that exact action
    /// just succeeded" refusal exempts `press` and `type`.
    #[test]
    fn the_same_press_twice_in_a_row_runs_twice() {
        let press = Action::Press { control: 3, name: "1".into() };
        let (mut e, rec, _) = engine_with(vec![start("p", true), plan(), act(1, press.clone()), act(1, press.clone()), done(run("python3"))], "press-twice");
        e.handle("make p").unwrap();
        assert_eq!(rec.desktop_calls.borrow().len(), 2, "both presses reached the desktop hand");
        let run_twice = Action::RunCommand { argv: vec!["ls".into()] };
        let (mut e2, rec2, _) = engine_with(vec![start("q", true), plan(), act(1, run_twice.clone()), act(1, run_twice.clone()), done(run("python3"))], "run-twice");
        e2.handle("make q").unwrap();
        assert_eq!(rec2.calls.borrow().iter().filter(|a| **a == run_twice).count(), 1, "a repeated command is still refused");
    }
```

- [ ] **Step 2: Run** `run-tests.sh test -p aios-core the_same_press` — FAIL to compile (arity; no `desktop_calls`).

- [ ] **Step 3: Implement**

`engine.rs`:

```rust
/// One project folder in, the three hands that serve it out: (sandbox, admin, desktop).
pub type WorkerFactory = Box<dyn Fn(&Path) -> (Box<dyn Worker>, Box<dyn Worker>, Box<dyn Worker>)>;
```

`executor_for`: `let (sandbox, admin, desktop) = (self.workers)(&ws); Ok(Executor::new(sandbox, admin, desktop, log, ws))`.

`perform`, the repeat check:

```rust
        // A second press of the same button, or the same text typed again, is ordinary desktop
        // work (a repeated digit, a second Next): exempt like a check is (2a §5).
        let repeatable = matches!(action, Action::Press { .. } | Action::Type { .. });
        if !is_check && !repeatable && job.steps.last().is_some_and(|s| s.ok && serde_json::to_string(&s.action).unwrap_or_default() == key) {
```

`testing.rs`: `Recorder` gains `pub desktop_calls: Rc<RefCell<Vec<Action>>>, pub desktop_outcomes: Rc<RefCell<VecDeque<Outcome>>>,`; a recorder for the hand:

```rust
/// The desktop hand: records only — no bus in a unit test.
pub struct DesktopRecorder(pub Recorder);

impl Worker for DesktopRecorder {
    fn run(&self, action: &Action) -> Outcome {
        self.0.desktop_calls.borrow_mut().push(action.clone());
        self.0.desktop_outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("ok"))
    }
}
```

and `scripted_workers` returns the triple with `Box::new(DesktopRecorder(rec.clone())) as Box<dyn Worker>` third.

`bin/ai-os-engine.rs`, `tests/live_1c.rs`, `live_1d.rs`, `live_primes.rs`: the closure returns a triple whose third element is, for now, `Box::new(executor::worker::FakeWorker::new(false)) as Box<dyn Worker>` with a `// Task 5 puts the real hand here` comment. (`FakeWorker` is `pub` in `worker.rs`.)

- [ ] **Step 4: Run** `run-tests.sh test -p aios-core` and `run-tests.sh test --workspace --no-run` — every core test green, the live tests compile.

- [ ] **Step 5: Commit**

```bash
git add runtime/core
git commit -m "feat(core): three hands from the factory; a repeated press is legal"
```

---

### Task 4: The pure half of the desktop worker — ids, selection, rendering, cap

**Files:**
- Create: `runtime/executor/src/desktop.rs`
- Modify: `runtime/executor/src/lib.rs` (`pub mod desktop;`)

**Interfaces:**
- Produces:

```rust
pub struct Node { pub app: String, pub path: String, pub role: String, pub name: String, pub showing: bool, pub sensitive: bool, pub editable: bool, pub checked: bool, pub focused: bool, pub text: Option<String> }
pub struct IdTable { … }  // new(), id_for(&Node) -> u32 (same object → same id), get(u32) -> Option<&Entry>
pub struct Entry { pub app: String, pub path: String, pub name: String }
pub fn look_cap(context_tokens: usize) -> usize
pub fn select<'a>(nodes: &'a [Node], find: Option<&str>) -> Vec<&'a Node>   // interesting, showing, focused first
pub fn render_look(window: &str, chosen: &[&Node], ids: &mut IdTable, cap: usize) -> String
pub struct Displays { pub invisible: String, pub visible: String }  // Displays::from_env()
```

- [ ] **Step 1: Failing tests** in `desktop.rs` `mod tests`:

```rust
    fn node(role: &str, name: &str, showing: bool) -> Node {
        Node { app: "app".into(), path: format!("/o/{role}/{name}"), role: role.into(), name: name.into(), showing, sensitive: true, editable: false, checked: false, focused: false, text: None }
    }

    #[test]
    fn the_cap_follows_the_context_and_never_drops_below_twenty() {
        assert_eq!(look_cap(8192), 40);
        assert_eq!(look_cap(32768), 163);
        assert_eq!(look_cap(1000), 20);
    }

    #[test]
    fn select_keeps_controls_drops_furniture_and_puts_the_focused_one_first() {
        let mut save = node("push button", "Save", true); save.focused = true;
        let nodes = vec![node("panel", "", true), node("filler", "", true), node("push button", "Bold", true),
                         node("menu item", "Paste", false), node("label", "1 word, 17 characters", true),
                         node("label", &"x".repeat(200), true), save.clone(), node("text", "", true)];
        let got: Vec<&str> = select(&nodes, None).iter().map(|n| n.name.as_str()).collect();
        assert_eq!(got, vec!["Save", "Bold", "1 word, 17 characters", ""], "focused first, then tree order; hidden menu item and long label dropped");
        let found: Vec<&str> = select(&nodes, Some("bold")).iter().map(|n| n.name.as_str()).collect();
        assert_eq!(found, vec!["Bold"]);
        let by_role: Vec<&str> = select(&nodes, Some("text")).iter().map(|n| n.role.as_str()).collect();
        assert_eq!(by_role, vec!["text"], "find matches the role too");
    }

    #[test]
    fn ids_are_stable_per_object_and_never_reused() {
        let mut t = IdTable::new();
        let a = node("push button", "Bold", true);
        let b = node("push button", "Save", true);
        assert_eq!(t.id_for(&a), 1);
        assert_eq!(t.id_for(&b), 2);
        assert_eq!(t.id_for(&a), 1, "same object, same id");
        assert_eq!(t.get(2).unwrap().name, "Save");
        assert!(t.get(3).is_none());
    }

    #[test]
    fn render_caps_and_says_how_many_more() {
        let mut t = IdTable::new();
        let nodes: Vec<Node> = (0..5).map(|i| node("push button", &format!("B{i}"), true)).collect();
        let chosen: Vec<&Node> = nodes.iter().collect();
        let s = render_look("Editor", &chosen, &mut t, 3);
        assert!(s.starts_with("controls of Editor:\n1 [push button] B0\n2 [push button] B1\n3 [push button] B2\n"), "{s}");
        assert!(s.ends_with("2 more, narrow with find\n"), "{s}");
        let mut checked = node("toggle button", "Bold", true); checked.checked = true; checked.focused = true;
        let mut text = node("text", "", true); text.text = Some("hello world".into()); text.editable = true;
        let s2 = render_look("Editor", &[&checked, &text], &mut t, 10);
        assert!(s2.contains("[toggle button] Bold (checked, focused)"), "{s2}");
        assert!(s2.contains(r#"[text] "hello world" (editable)"#), "{s2}");
    }
```

- [ ] **Step 2: Run** `run-tests.sh test -p executor desktop` — FAIL: module missing.

- [ ] **Step 3: Implement** `desktop.rs`:

```rust
//! The desktop hand's pure half (2a design §3–§4): what a window's tree becomes for the model,
//! and the id table. No bus here; `atspi.rs` fills `Node`s from the real one.

/// One accessible object, as much of it as the hand reads.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub app: String,
    pub path: String,
    pub role: String,
    pub name: String,
    pub showing: bool,
    pub sensitive: bool,
    pub editable: bool,
    pub checked: bool,
    pub focused: bool,
    /// The first 60 characters of a text control's contents, when it has any.
    pub text: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry { pub app: String, pub path: String, pub name: String }

/// Short ids for the model, one per object for the life of the process; never reused, never
/// cleared (2a §3: an id is only ever the object it was handed out for).
#[derive(Debug, Default)]
pub struct IdTable { entries: Vec<Entry> }

impl IdTable {
    pub fn new() -> Self { Self::default() }
    pub fn id_for(&mut self, n: &Node) -> u32 {
        if let Some(i) = self.entries.iter().position(|e| e.app == n.app && e.path == n.path) {
            self.entries[i].name = n.name.clone();
            return i as u32 + 1;
        }
        self.entries.push(Entry { app: n.app.clone(), path: n.path.clone(), name: n.name.clone() });
        self.entries.len() as u32
    }
    pub fn get(&self, id: u32) -> Option<&Entry> { id.checked_sub(1).and_then(|i| self.entries.get(i as usize)) }
}

/// The context budget divided by 200, never below 20 (2a §4): 40 on the 8k workshop.
pub fn look_cap(context_tokens: usize) -> usize { (context_tokens / 200).max(20) }

/// The roles the model can do something with or learn something from.
const INTERESTING: [&str; 21] = [
    "push button", "toggle button", "button", "check box", "radio button", "menu", "menu item", "check menu item",
    "radio menu item", "entry", "password text", "text", "paragraph", "document text", "combo box", "page tab",
    "label", "spin button", "slider", "link", "list item",
];

/// Interactive and readable controls only, showing only, the focused one first, then tree order;
/// `find` keeps those whose name or role contains it (case-insensitive). Long labels are furniture.
pub fn select<'a>(nodes: &'a [Node], find: Option<&str>) -> Vec<&'a Node> {
    let f = find.map(|s| s.to_lowercase());
    let mut v: Vec<&Node> = nodes.iter().filter(|n| n.showing && INTERESTING.contains(&n.role.as_str()))
        .filter(|n| n.role != "label" || n.name.chars().count() <= 80)
        .filter(|n| f.as_ref().map_or(true, |f| n.name.to_lowercase().contains(f) || n.role.to_lowercase().contains(f)))
        .collect();
    // ponytail: stable partition keeps tree order within each half.
    let (focused, rest): (Vec<&Node>, Vec<&Node>) = v.drain(..).partition(|n| n.focused);
    focused.into_iter().chain(rest).collect()
}

/// The text the model reads for `look` on a window.
pub fn render_look(window: &str, chosen: &[&Node], ids: &mut IdTable, cap: usize) -> String {
    let mut out = format!("controls of {window}:\n");
    for n in chosen.iter().take(cap) {
        let id = ids.id_for(n);
        let mut flags = vec![];
        if n.checked { flags.push("checked"); }
        if n.focused { flags.push("focused"); }
        if n.editable { flags.push("editable"); }
        if !n.sensitive { flags.push("disabled"); }
        let flags = if flags.is_empty() { String::new() } else { format!(" ({})", flags.join(", ")) };
        match &n.text {
            Some(t) if n.name.is_empty() => out.push_str(&format!("{id} [{}] {:?}{flags}\n", n.role, t)),
            Some(t) => out.push_str(&format!("{id} [{}] {} {:?}{flags}\n", n.role, n.name, t)),
            None => out.push_str(&format!("{id} [{}] {}{flags}\n", n.role, n.name)),
        }
    }
    if chosen.len() > cap { out.push_str(&format!("{} more, narrow with find\n", chosen.len() - cap)); }
    out
}

/// Where `open_app` puts a window (2a §5): the invisible session unless the user asked to see it.
#[derive(Debug, Clone, PartialEq)]
pub struct Displays { pub invisible: String, pub visible: String }

impl Displays {
    pub fn from_env() -> Self {
        Self {
            invisible: std::env::var("AI_OS_DISPLAY_INVISIBLE").unwrap_or_else(|_| "wayland-ai".into()),
            visible: std::env::var("AI_OS_DISPLAY_VISIBLE").unwrap_or_else(|_| "wayland-0".into()),
        }
    }
}
```

`{:?}` on a `String` prints it quoted with escapes, which is what the test's `"hello world"` expects.

- [ ] **Step 4: Run** `run-tests.sh test -p executor desktop` — green.

- [ ] **Step 5: Commit**

```bash
git add runtime/executor/src/desktop.rs runtime/executor/src/lib.rs
git commit -m "feat(executor): the desktop hand's id table, selection, rendering and cap"
```

---

### Task 5: The bus half — zbus proxies, `DesktopState`, `DesktopWorker`, and its live check

**Files:**
- Modify: `runtime/executor/Cargo.toml`
- Create: `runtime/executor/src/atspi.rs`
- Create: `runtime/executor/tests/desktop_live.rs`
- Modify: `runtime/executor/src/lib.rs` (`pub mod atspi;`)
- Modify: `runtime/core/src/bin/ai-os-engine.rs`, `tests/live_1c.rs`, `live_1d.rs`, `live_primes.rs` (the real third hand)

**Interfaces:**
- Produces: `atspi::DesktopState::new(cap: usize, displays: Displays) -> Rc<RefCell<DesktopState>>`; `atspi::DesktopWorker(pub Rc<RefCell<DesktopState>>)` implementing `Worker`. Outcomes: `look` detail is the window list or `render_look`; `press` detail `pressed {name}`; `type` detail `typed N characters into control {id}; it now holds M characters`; `read` detail `(lines a-b of N)\n…`; `open_app` detail `opened {name}; its window is {title}` or `opened {name}; no window appeared within 10 s, look again later`. Errors: `that control is gone; look again`, `control {id} was never handed out; look first`, `control {id} is named {table} now, not {echoed}; look again`, `no window matches {w}`, `{n} windows match {w}: …; say which`.

- [ ] **Step 1: Dependency** — `runtime/executor/Cargo.toml`:

```toml
zbus = { version = "5", features = ["blocking-api"] }
```

Run `run-tests.sh build -p executor` once so the crate downloads (network from the distro works; `cargo search` found zbus 5.19).

- [ ] **Step 2: Check the AT-SPI state bits** the hand reads, in the distro: `grep -nE "ATSPI_STATE_(CHECKED|EDITABLE|FOCUSED|SENSITIVE|SHOWING)\b" /usr/include/at-spi-2.0/atspi/atspi-constants.h` (install `libatspi2.0-dev` as root if the header is missing). Expected, from the enum's order: checked 4, editable 7, focused 12, sensitive 24, showing 25. If the header says otherwise, the constants below take the header's numbers.

- [ ] **Step 3: Implement** `atspi.rs`:

```rust
//! The desktop hand's bus half (2a design §5): four AT-SPI interfaces over zbus's blocking API,
//! one shared state the engine's worker factory captures, and the worker that performs the five
//! actions. No app is named here.
use crate::action::{valid_app_name, Action};
use crate::desktop::{look_cap, render_look, select, Displays, IdTable, Node};
use crate::worker::{Outcome, Worker};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};
use zbus::blocking::Connection;
use zbus::proxy;
use zbus::zvariant::OwnedObjectPath;

type Ref = (String, OwnedObjectPath);

#[proxy(interface = "org.a11y.atspi.Accessible", gen_async = false)]
trait Accessible {
    #[zbus(property)] fn name(&self) -> zbus::Result<String>;
    #[zbus(property)] fn child_count(&self) -> zbus::Result<i32>;
    fn get_role_name(&self) -> zbus::Result<String>;
    fn get_state(&self) -> zbus::Result<Vec<u32>>;
    fn get_children(&self) -> zbus::Result<Vec<Ref>>;
}

#[proxy(interface = "org.a11y.atspi.Action", gen_async = false)]
trait ActionIface {
    fn get_nactions(&self) -> zbus::Result<i32>;
    fn do_action(&self, index: i32) -> zbus::Result<bool>;
}

#[proxy(interface = "org.a11y.atspi.Text", gen_async = false)]
trait Text {
    #[zbus(property)] fn character_count(&self) -> zbus::Result<i32>;
    fn get_text(&self, start: i32, end: i32) -> zbus::Result<String>;
}

#[proxy(interface = "org.a11y.atspi.EditableText", gen_async = false)]
trait EditableText {
    fn insert_text(&self, position: i32, text: &str, length: i32) -> zbus::Result<bool>;
    fn delete_text(&self, start: i32, end: i32) -> zbus::Result<bool>;
}

// AT-SPI StateType bit numbers (atspi-constants.h; checked in the plan's Task 5 step 2).
const CHECKED: u32 = 4;
const EDITABLE: u32 = 7;
const FOCUSED: u32 = 12;
const SENSITIVE: u32 = 24;
const SHOWING: u32 = 25;
fn has(state: &[u32], bit: u32) -> bool { state.get((bit / 32) as usize).is_some_and(|w| w & (1 << (bit % 32)) != 0) }

const REGISTRY: &str = "org.a11y.atspi.Registry";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
/// The walk's node bound, as the Phase 0 probe's. ponytail: one round-trip per node; the
/// `org.a11y.atspi.Cache` interface (all nodes of an app in one call) is the upgrade if a big
/// window's look takes too long.
const MAX_NODES: usize = 6000;

/// Shared by every `DesktopWorker` the factory hands out: the connection opens on first use,
/// the id table lives as long as the process.
pub struct DesktopState { conn: Option<Connection>, pub ids: IdTable, cap: usize, displays: Displays }

impl DesktopState {
    pub fn new(cap: usize, displays: Displays) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self { conn: None, ids: IdTable::new(), cap, displays }))
    }
    pub fn for_model(context_tokens: usize) -> Rc<RefCell<Self>> { Self::new(look_cap(context_tokens), Displays::from_env()) }

    /// The accessibility bus, found through the session bus's `org.a11y.Bus.GetAddress`.
    fn conn(&mut self) -> Result<&Connection, String> {
        if self.conn.is_none() {
            let session = Connection::session().map_err(|e| format!("no session bus: {e}"))?;
            let reply = session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())
                .map_err(|e| format!("no accessibility bus: {e}"))?;
            let addr: String = reply.body().deserialize().map_err(|e| format!("bad a11y address: {e}"))?;
            let conn = zbus::blocking::connection::Builder::address(addr.as_str()).and_then(|b| b.build())
                .map_err(|e| format!("cannot join the accessibility bus: {e}"))?;
            self.conn = Some(conn);
        }
        Ok(self.conn.as_ref().unwrap())
    }
}

fn acc<'a>(conn: &'a Connection, r: &Ref) -> zbus::Result<AccessibleProxyBlocking<'a>> {
    AccessibleProxyBlocking::builder(conn).destination(r.0.as_str())?.path(r.1.clone())?
        .cache_properties(zbus::proxy::CacheProperties::No).build()
}

/// One node, read through the bus. `None` if the object does not answer (gone).
fn read_node(conn: &Connection, r: &Ref, with_text: bool) -> Option<Node> {
    let a = acc(conn, r).ok()?;
    let role = a.get_role_name().ok()?;
    let state = a.get_state().ok()?;
    let name = a.name().unwrap_or_default();
    let editable = has(&state, EDITABLE);
    let text = if with_text && matches!(role.as_str(), "text" | "entry" | "paragraph" | "document text" | "label") {
        TextProxyBlocking::builder(conn).destination(r.0.as_str()).ok()?.path(r.1.clone()).ok()?
            .cache_properties(zbus::proxy::CacheProperties::No).build().ok()
            .and_then(|t| t.get_text(0, 60).ok()).filter(|s| !s.is_empty())
    } else { None };
    Some(Node { app: r.0.clone(), path: r.1.to_string(), role, name, showing: has(&state, SHOWING), sensitive: has(&state, SENSITIVE),
                editable, checked: has(&state, CHECKED), focused: has(&state, FOCUSED), text })
}

/// Depth-first over a subtree, bounded.
fn walk(conn: &Connection, from: &Ref) -> Vec<Node> {
    let mut out = vec![];
    let mut stack = vec![from.clone()];
    while let Some(r) = stack.pop() {
        if out.len() >= MAX_NODES { break; }
        if let Some(n) = read_node(conn, &r, true) { out.push(n); }
        if let Ok(a) = acc(conn, &r) {
            if let Ok(kids) = a.get_children() { stack.extend(kids.into_iter().rev()); }
        }
    }
    out
}

/// (app name, window title, window ref) for every top-level window on the bus.
fn windows(conn: &Connection) -> Result<Vec<(String, String, Ref)>, String> {
    let root = acc(conn, &(REGISTRY.to_string(), OwnedObjectPath::try_from(ROOT).unwrap())).map_err(|e| e.to_string())?;
    let mut v = vec![];
    for app in root.get_children().map_err(|e| e.to_string())? {
        let Ok(a) = acc(conn, &app) else { continue };
        let app_name = a.name().unwrap_or_default();
        let Ok(frames) = a.get_children() else { continue };
        for f in frames {
            if let Some(n) = read_node(conn, &f, false) {
                if n.showing { v.push((app_name.clone(), n.name, f)); }
            }
        }
    }
    Ok(v)
}

pub struct DesktopWorker(pub Rc<RefCell<DesktopState>>);

impl DesktopWorker {
    fn find_window(conn: &Connection, wanted: &str) -> Result<(String, Ref), String> {
        let w = wanted.to_lowercase();
        let all = windows(conn)?;
        let hits: Vec<_> = all.iter().filter(|(app, title, _)| app.to_lowercase().contains(&w) || title.to_lowercase().contains(&w)).collect();
        match hits.len() {
            0 => Err(format!("no window matches {wanted}; look with no window to see what is open")),
            1 => Ok((format!("{} — {}", hits[0].0, hits[0].1), hits[0].2.clone())),
            n => Err(format!("{n} windows match {wanted}: {}; say which", hits.iter().map(|(a, t, _)| format!("{a} — {t}")).collect::<Vec<_>>().join(", "))),
        }
    }

    /// The object behind an id, after the echoed name (if any) and liveness are checked.
    fn resolve(st: &DesktopState, conn: &Connection, id: u32, echoed: Option<&str>) -> Result<Ref, String> {
        let e = st.ids.get(id).ok_or_else(|| format!("control {id} was never handed out; look first"))?;
        if let Some(n) = echoed {
            if n != e.name { return Err(format!("control {id} is named {} now, not {n}; look again", e.name)); }
        }
        let r: Ref = (e.app.clone(), OwnedObjectPath::try_from(e.path.as_str()).map_err(|e| e.to_string())?);
        match acc(conn, &r).ok().and_then(|a| a.get_role_name().ok()) {
            Some(_) => Ok(r),
            None => Err("that control is gone; look again".into()),
        }
    }

    fn run_inner(&self, action: &Action) -> Result<String, String> {
        let mut st = self.0.borrow_mut();
        let cap = st.cap;
        let displays = st.displays.clone();
        match action {
            Action::OpenApp { name, visible } => {
                if !valid_app_name(name) { return Err(format!("invalid application name: {name}")); }
                let entry = format!("{name}.desktop");
                let dirs = ["/usr/share/applications", "/usr/local/share/applications", "/home/ai/.local/share/applications"];
                if !dirs.iter().any(|d| std::path::Path::new(d).join(&entry).is_file()) {
                    return Err(format!("no application called {name} is installed (no {entry} in the application folders)"));
                }
                let display = if *visible { &displays.visible } else { &displays.invisible };
                let conn = st.conn()?;
                let before = windows(conn)?.len();
                std::process::Command::new("gtk-launch").arg(name)
                    .env("WAYLAND_DISPLAY", display).env("GNOME_ACCESSIBILITY", "1").env_remove("DISPLAY")
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
                    .spawn().map_err(|e| format!("could not launch {name}: {e}"))?;
                let t = Instant::now();
                while t.elapsed() < Duration::from_secs(10) {
                    std::thread::sleep(Duration::from_millis(500));
                    let now = windows(conn)?;
                    if now.len() > before {
                        let (app, title, _) = now.last().unwrap();
                        return Ok(format!("opened {name}; its window is {app} — {title}"));
                    }
                }
                Ok(format!("opened {name}; no window appeared within 10 s, look again later"))
            }
            Action::Look { window: None, .. } => {
                let conn = st.conn()?;
                let all = windows(conn)?;
                if all.is_empty() { return Ok("no windows are open".into()); }
                Ok(format!("windows:\n{}", all.iter().map(|(a, t, _)| format!("- {a} — {t}")).collect::<Vec<_>>().join("\n")))
            }
            Action::Look { window: Some(w), find } => {
                let conn = st.conn()?.clone();
                let (title, frame) = Self::find_window(&conn, w)?;
                let nodes = walk(&conn, &frame);
                let chosen = select(&nodes, find.as_deref());
                Ok(render_look(&title, &chosen, &mut st.ids, cap))
            }
            Action::Press { control, name } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, Some(name))?;
                let a = ActionIfaceProxyBlocking::builder(&conn).destination(r.0.as_str()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.build())
                    .map_err(|e| e.to_string())?;
                if a.get_nactions().unwrap_or(0) < 1 { return Err(format!("{name} has no action to press")); }
                a.do_action(0).map_err(|e| format!("press failed: {e}"))?;
                Ok(format!("pressed {name}"))
            }
            Action::Type { control, text, replace } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, None)?;
                let t = TextProxyBlocking::builder(&conn).destination(r.0.as_str()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build()).map_err(|e| e.to_string())?;
                let e = EditableTextProxyBlocking::builder(&conn).destination(r.0.as_str()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.build()).map_err(|e| e.to_string())?;
                let mut count = t.character_count().map_err(|_| "that control takes no text".to_string())?;
                if *replace && count > 0 { e.delete_text(0, count).map_err(|e| format!("could not clear it: {e}"))?; count = 0; }
                let n = text.chars().count() as i32;
                e.insert_text(count, text, n).map_err(|e| format!("could not type: {e}"))?;
                let after = t.character_count().unwrap_or(count + n);
                Ok(format!("typed {n} characters into control {control}; it now holds {after} characters"))
            }
            Action::Read { control, from_line, lines } => {
                let conn = st.conn()?.clone();
                let r = Self::resolve(&st, &conn, *control, None)?;
                let t = TextProxyBlocking::builder(&conn).destination(r.0.as_str()).and_then(|b| b.path(r.1.clone())).and_then(|b| b.cache_properties(zbus::proxy::CacheProperties::No).build()).map_err(|e| e.to_string())?;
                let count = t.character_count().map_err(|_| "that control has no text".to_string())?;
                let all = t.get_text(0, count).map_err(|e| e.to_string())?;
                let v: Vec<&str> = all.lines().collect();
                let from = from_line.unwrap_or(1).max(1);
                let n = lines.unwrap_or(200).min(200);
                let slice: Vec<&str> = v.iter().skip(from - 1).take(n).copied().collect();
                Ok(format!("(lines {}-{} of {})\n{}", from, from + slice.len().saturating_sub(1), v.len(), slice.join("\n")))
            }
            other => Err(format!("not a desktop action: {other:?}")),
        }
    }
}

impl Worker for DesktopWorker {
    fn run(&self, action: &Action) -> Outcome {
        match self.run_inner(action) { Ok(d) => Outcome::ok(d), Err(e) => Outcome::err(e) }
    }
}
```

`conn()` returns a borrow of `st`; the arms clone the `Connection` (it is an `Arc` inside) where `st.ids` is borrowed mutably afterwards. If the borrow checker still objects in `Look`, take `let conn = st.conn()?.clone();` before any use of `st.ids`, as written. Names of the zbus items (`zbus::blocking::connection::Builder`, `proxy::CacheProperties`, `#[proxy(gen_async = false)]`, `ProxyBlocking` suffix) are zbus 5's; confirm against `docs.rs/zbus/5` if the build disagrees and adapt the call, not the design.

Wire the real hand: `bin/ai-os-engine.rs`:

```rust
use executor::atspi::{DesktopState, DesktopWorker};
…
        let model = OllamaModel::local(&model_name);
        let desktop = DesktopState::for_model(model.context_tokens());
        Engine::new(store, model, root, Some(db),
            Box::new(move |ws| (
                Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
                Box::new(AdminWorker) as Box<dyn Worker>,
                Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
            )),
```

(`context_tokens` arrives in Task 7; until then use `DesktopState::for_model(8192)` and change it there.) The three live tests get the same third element.

- [ ] **Step 4: The live check** — `runtime/executor/tests/desktop_live.rs`, gated, the smallest thing that fails if the bus half is wrong:

```rust
// The desktop hand on a real window (2a §12 item 1's live half). Inside the distro with the
// invisible session up:  AI_OS_DESKTOP=1 cargo test -p executor --test desktop_live -- --nocapture
use executor::action::Action;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::desktop::Displays;
use executor::worker::Worker;

#[test]
fn opens_an_editor_looks_types_and_reads_back() {
    if std::env::var("AI_OS_DESKTOP").is_err() { eprintln!("skipped: AI_OS_DESKTOP not set"); return; }
    let w = DesktopWorker(DesktopState::new(40, Displays::from_env()));
    let o = w.run(&Action::OpenApp { name: "org.gnome.TextEditor".into(), visible: false });
    assert!(o.ok, "{}", o.detail);
    let looked = w.run(&Action::Look { window: Some("Text Editor".into()), find: Some("text".into()) });
    assert!(looked.ok && looked.detail.contains("[text]"), "{}", looked.detail);
    let id: u32 = looked.detail.lines().find(|l| l.contains("[text]")).unwrap().split(' ').next().unwrap().parse().unwrap();
    let typed = w.run(&Action::Type { control: id, text: "hello from ai-os".into(), replace: true });
    assert!(typed.ok, "{}", typed.detail);
    let read = w.run(&Action::Read { control: id, from_line: None, lines: None });
    assert!(read.ok && read.detail.contains("hello from ai-os"), "{}", read.detail);
    // The title mirrors the buffer's first line (the 2a probe's proof that the insert committed).
    let again = w.run(&Action::Look { window: None, find: None });
    assert!(again.detail.contains("hello from ai"), "{}", again.detail);
    let closes = w.run(&Action::Look { window: Some("Text Editor".into()), find: Some("close".into()) });
    println!("{}", closes.detail);
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-text-editor"]).status();
}
```

Run it with the session up (Task 8 makes that a unit; until then `trial/headless-session.sh` minus its sudo line, as the probe did) through a script `trial/run-desktop-live.sh`:

```bash
#!/usr/bin/env bash
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export AI_OS_DISPLAY_INVISIBLE=${AI_OS_DISPLAY_INVISIBLE:-wayland-ai} AI_OS_DISPLAY_VISIBLE=wayland-0 AI_OS_DESKTOP=1
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo test -p executor --test desktop_live -- --nocapture 2>&1 | tail -30
```

Expected: pass; the printed `close` look shows the window's Close control with its id, which is what script 3 of the acceptance will press.

- [ ] **Step 5: Run** `run-tests.sh test --workspace --no-run` (everything compiles) and `run-tests.sh test -p executor` (unit tests green; the live one skips without its env).

- [ ] **Step 6: Commit**

```bash
git add runtime/executor runtime/core/src/bin runtime/core/tests trial/run-desktop-live.sh
git commit -m "feat(executor): the desktop worker over the accessibility bus, with its live check"
```

---

### Task 6: `done` carries the windows; the rail and the terminal say the undo line

**Files:**
- Modify: `runtime/proto/src/lib.rs`
- Modify: `runtime/rail/src/cards.rs`, `runtime/rail/src/main.rs`, `runtime/rail/tests/cards.rs`
- Modify: `runtime/core/src/event.rs` (`lines`)

**Interfaces:**
- Produces: `Event::Done { job_id, text, check, files, #[serde(default)] windows: Vec<String> }`; `pub fn window_note(windows: &[String]) -> Option<String>` in `aios_proto`; `CardKind::Done { text, check, files, windows }`.

- [ ] **Step 1: Failing tests** — `proto/src/lib.rs` `mod tests`:

```rust
    #[test]
    fn an_old_done_without_windows_still_parses_and_the_note_reads_right() {
        let old = r#"{"kind":"done","job_id":"j","text":"t","check":null,"files":[]}"#;
        let e: Event = serde_json::from_str(old).unwrap();
        assert!(matches!(&e, Event::Done { windows, .. } if windows.is_empty()));
        assert_eq!(window_note(&[]), None);
        assert_eq!(window_note(&["Text Editor".into()]).unwrap(), "What I did inside Text Editor can't be undone by me.");
        assert_eq!(window_note(&["Writer".into(), "Calculator".into()]).unwrap(), "What I did inside Writer and Calculator can't be undone by me.");
    }
```

`rail/tests/cards.rs`:

```rust
#[test]
fn a_window_job_done_card_names_the_window_and_says_it_cannot_undo() {
    let mut cards = Cards::default();
    cards.apply(&Event::Understood { job_id: j(), name: "housekeeping".into(), text: "Taking your editor".into(), housekeeping: true });
    cards.apply(&Event::Done { job_id: j(), text: "Added the line and saved.".into(), check: None, files: vec![], windows: vec!["Text Editor".into()] });
    let c = cards.list.last().unwrap();
    assert!(matches!(&c.kind, CardKind::Done { windows, .. } if windows == &vec!["Text Editor".to_string()]));
    assert!(c.text.ends_with("What I did inside Text Editor can't be undone by me."), "{}", c.text);
    assert!(c.opens.is_empty());
}
```

`core/src/event.rs` tests:

```rust
    #[test]
    fn the_terminal_prints_the_undo_note_after_a_window_job() {
        let e = Event::Done { job_id: "j".into(), text: "done".into(), check: None, files: vec![], windows: vec!["Calculator".into()] };
        assert_eq!(lines(&e), vec!["done".to_string(), "What I did inside Calculator can't be undone by me.".to_string()]);
        let plain = Event::Done { job_id: "j".into(), text: "done".into(), check: None, files: vec![], windows: vec![] };
        assert_eq!(lines(&plain), vec!["done".to_string()], "no windows, no note: 1d's lines unchanged");
    }
```

- [ ] **Step 2: Run** `run-tests.sh test -p aios-proto` — FAIL (no field, no function).

- [ ] **Step 3: Implement** — `proto/src/lib.rs`:

```rust
    Done { job_id: String, text: String, check: Option<String>, files: Vec<ChangedFile>, #[serde(default)] windows: Vec<String> },
```

and after the `Event` impl:

```rust
/// The one line every Done card of a job that used the desktop hand carries (2a design §7).
pub fn window_note(windows: &[String]) -> Option<String> {
    if windows.is_empty() { return None; }
    Some(format!("What I did inside {} can't be undone by me.", windows.join(" and ")))
}
```

Every `Event::Done { .. }` constructor in `core` (`engine.rs::finish`, tests) gains `windows: vec![]` for now (Task 7 fills it); every pattern that lists fields without `..` gains the field.

`rail/src/cards.rs`: `Done { text: String, check: Option<String>, files: Vec<ChangedFile>, windows: Vec<String> }`; the `Event::Done` arm:

```rust
            Event::Done { job_id, text, check, files, windows } => {
                let mut ch = self.close_building();
                let shown = match aios_proto::window_note(windows) { Some(n) => format!("{text}\n{n}"), None => text.clone() };
                ch.extend(self.push(result_card(CardKind::Done { text: text.clone(), check: check.clone(), files: files.clone(), windows: windows.clone() }, &shown, files, job_id)));
                ch
            }
```

`rail/src/main.rs` Done arm: `CardKind::Done { text: t, check, files, windows } => { … if let Some(n) = aios_proto::window_note(windows) { let l = text(&n); l.add_css_class("dim"); b.append(&l); } file_rows(&b, files); }`. Any `rebuild`/`State` code that builds a `CardKind::Done` gains `windows: vec![]`.

`core/src/event.rs` `lines`: split `Event::Done` out of the first arm:

```rust
        Event::Done { text, windows, .. } => { let mut v = vec![text.clone()]; v.extend(aios_proto::window_note(windows)); v }
```

- [ ] **Step 4: Run** `run-tests.sh test -p aios-proto`, `-p aios-rail`, `-p aios-core` — green.

- [ ] **Step 5: Commit**

```bash
git add runtime/proto runtime/rail runtime/core
git commit -m "feat(proto,rail): done carries the windows a job worked, and the rail says they cannot be undone"
```

---

### Task 7: The engine computes the windows, asks the model its context, and teaches the hand

**Files:**
- Modify: `runtime/core/src/engine.rs` (`finish`, `windows_worked`)
- Modify: `runtime/core/src/model.rs` (`Model::context_tokens`)
- Modify: `runtime/core/src/prompt.rs` (`SYSTEM`, `front_door`, `job_turn`)
- Modify: `runtime/core/src/bin/ai-os-engine.rs` (use `context_tokens`)

**Interfaces:**
- Produces: `Model::context_tokens(&self) -> usize` (default 8192; `OllamaModel` returns its `NUM_CTX`); `engine::windows_worked(&Job) -> Vec<String>` (pub(crate)).

- [ ] **Step 1: Failing tests** — `engine.rs` `mod tests`:

```rust
    /// 2a §7: the windows a job worked are the ones it looked into or opened, each once, from
    /// the steps that succeeded — nothing is stored on the job for it.
    #[test]
    fn done_lists_the_windows_the_job_looked_into_or_opened() {
        let look = |w: &str| Action::Look { window: Some(w.into()), find: None };
        let (mut e, _, _) = engine_with(vec![
            Move::Housekeep { goal: "take the editor".into(), understood: "Taking the editor".into(), remember: None }, plan(),
            act(1, Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }),
            act(1, look("Text Editor")), act(1, Action::Press { control: 1, name: "Save".into() }), act(1, look("Text Editor")),
            done(Action::Read { control: 2, from_line: None, lines: None }),
        ], "windows-worked");
        let ev = e.handle_events("take my editor").unwrap();
        let Event::Done { windows, .. } = ev.last().unwrap() else { panic!("{ev:?}") };
        assert_eq!(windows, &vec!["org.gnome.Calculator".to_string(), "Text Editor".to_string()]);
    }

    #[test]
    fn a_project_job_with_no_desktop_steps_lists_no_windows() {
        let (mut e, _, _) = engine_with(happy_path(), "no-windows");
        let ev = e.handle_events("make p").unwrap();
        assert!(matches!(ev.last().unwrap(), Event::Done { windows, .. } if windows.is_empty()), "{ev:?}");
    }
```

`model.rs` tests:

```rust
    #[test]
    fn the_ollama_connection_says_its_context_and_sends_the_same_number() {
        let m = OllamaModel::local("x");
        assert_eq!(m.context_tokens(), 8192);
        let b = ollama_body("x", &Prompt { system: String::new(), user: String::new(), allowed: vec![] });
        assert_eq!(b["options"]["num_ctx"], m.context_tokens());
        assert_eq!(FakeModel::new(vec![]).context_tokens(), 8192, "the trait default");
    }
```

`prompt.rs` tests:

```rust
    #[test]
    fn system_rules_teach_the_desktop_hand_and_name_no_application() {
        for w in ["`look`", "`press`", "`type`", "`read`", "`open_app`", "Look before you act"] { assert!(SYSTEM.contains(w), "{w}"); }
        assert!(!SYSTEM.contains("Writer") && !SYSTEM.contains("Calculator") && !SYSTEM.contains("Text Editor"), "nothing per-app");
        let p = front_door(&[], &[], &[], "take my editor window");
        assert!(p.user.contains("a window the user named"), "{}", p.user);
        let job = Job::new_housekeeping("/data/housekeeping", "take the editor", "Taking it");
        let t = job_turn(&[], &job, None, None);
        assert!(t.user.contains("found with `look` first"), "{}", t.user);
    }
```

- [ ] **Step 2: Run** `run-tests.sh test -p aios-core done_lists_the_windows` — FAIL (`windows` empty; no `context_tokens`; prompt lacks the words).

- [ ] **Step 3: Implement**

`engine.rs`, near `changed_files`:

```rust
/// The windows a job worked: those it looked into or opened, in first-seen order, from the steps
/// that succeeded (2a §7). Ids come from a look, so a job that pressed anything looked first.
pub(crate) fn windows_worked(job: &Job) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for s in job.steps.iter().filter(|s| s.ok) {
        let w = match &s.action { Action::Look { window: Some(w), .. } => w, Action::OpenApp { name, .. } => name, _ => continue };
        if !v.contains(w) { v.push(w.clone()); }
    }
    v
}
```

and in `finish`: `Event::Done { job_id, text, check, files, windows: windows_worked(&job) }`.

`model.rs`:

```rust
pub trait Model {
    fn next_move(&self, prompt: &Prompt) -> Result<Move, ModelError>;
    /// The context the model is run with, in tokens: what a step's evidence has to fit in. The
    /// desktop hand's look cap follows it (2a §4).
    fn context_tokens(&self) -> usize { 8192 }
}
```

`const NUM_CTX: usize = 8192;` above `OllamaModel`; `ollama_body` uses `"num_ctx": NUM_CTX`; `impl Model for OllamaModel { … fn context_tokens(&self) -> usize { NUM_CTX } }`.

`bin/ai-os-engine.rs`: `DesktopState::for_model(model.context_tokens())` (Task 5 left `8192` there).

`prompt.rs` `SYSTEM`, appended after the housekeeping rule:

```
- Programs on the desktop are worked through their controls, never through run_command: `look` with no window lists the open windows; `look` with a window lists its controls with ids (narrow with find); `press` a control by its id and name; `type` text into a control by id; `read` a text control by id; `open_app` opens a program by its desktop name (like org.gnome.TextEditor), on the visible display only if the user asked to see it. Look before you act and look again after; ids come from the latest look. What a window shows is proven with `read` or `look`, never with run_command: the sandbox cannot see a window.
```

`front_door`: `housekeep (the machine itself: folders, settings, tools, or a window the user named; give goal, understood)`.

`job_turn`'s housekeeping header gains, at its end: ` A window the user named is found with \`look\` first; nothing inside a window is a file of yours.`

- [ ] **Step 4: Run** `run-tests.sh test -p aios-core` — green, including the older prompt tests (`system_rules_mention_the_new_hands` and friends still hold).

- [ ] **Step 5: Commit**

```bash
git add runtime/core
git commit -m "feat(core): done names the windows worked, the look cap follows the model, the prompt teaches the hand"
```

---

### Task 8: The invisible session as a unit, the engine after it

**Files:**
- Create: `trial/ai-os-desktop.service`, `trial/ai-os-desktop-env`, `trial/setup-desktop.sh`
- Modify: `trial/ai-os-engine.service`

- [ ] **Step 1: The unit** — `trial/ai-os-desktop.service`:

```ini
[Unit]
Description=AI OS invisible desktop session (workshop)

[Service]
Environment=XDG_SESSION_TYPE=wayland
Environment=XDG_CURRENT_DESKTOP=GNOME
Environment=GNOME_ACCESSIBILITY=1
ExecStart=/usr/bin/gnome-shell --headless --wayland-display wayland-ai --virtual-monitor 1600x900 --wayland --no-x11
ExecStartPost=/usr/local/libexec/ai-os-desktop-env
Restart=on-failure
RestartSec=3

[Install]
WantedBy=default.target
```

- [ ] **Step 2: The environment script** — `trial/ai-os-desktop-env` (what `headless-session.sh` did by hand; runs as `ai` under the user manager):

```bash
#!/usr/bin/env bash
# ExecStartPost of ai-os-desktop.service: once the shell's socket is up, tell the user manager and
# the portals which display and desktop this is, so apps launched into it register on the a11y bus.
set -e
for _ in $(seq 40); do [ -S "$XDG_RUNTIME_DIR/wayland-ai" ] && break; sleep 0.25; done
[ -S "$XDG_RUNTIME_DIR/wayland-ai" ] || { echo "wayland-ai never appeared" >&2; exit 1; }
export WAYLAND_DISPLAY=wayland-ai
gsettings set org.gnome.desktop.interface toolkit-accessibility true || true
systemctl --user set-environment XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-ai GNOME_ACCESSIBILITY=1
dbus-update-activation-environment --systemd XDG_CURRENT_DESKTOP=GNOME XDG_SESSION_TYPE=wayland WAYLAND_DISPLAY=wayland-ai GNOME_ACCESSIBILITY=1
mkdir -p ~/.config/systemd/user/graphical-session.target.d
printf '[Unit]\nRefuseManualStart=no\n' > ~/.config/systemd/user/graphical-session.target.d/manual.conf
systemctl --user daemon-reload
systemctl --user start graphical-session.target || true
systemctl --user restart xdg-desktop-portal-gnome xdg-desktop-portal 2>/dev/null || true
```

- [ ] **Step 3: The engine unit** — `trial/ai-os-engine.service` gains, under `[Unit]`: `After=ai-os-desktop.service` and `Wants=ai-os-desktop.service`; under `[Service]`: `Environment=AI_OS_DISPLAY_INVISIBLE=wayland-ai` and `Environment=AI_OS_DISPLAY_VISIBLE=wayland-0`.

- [ ] **Step 4: The setup script** — `trial/setup-desktop.sh` (root, idempotent, the shape of `setup-rail.sh`):

```bash
#!/usr/bin/env bash
# Phase 2a workshop setup. Run as root inside the ai-os distro after a release build. Idempotent.
# WORKSHOP ONLY (parent spec §11): files are copied from the Windows-mounted repo.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
apt-get install -y gnome-text-editor gnome-calculator >/dev/null
install -m 0755 -o root -g root "$repo/trial/ai-os-desktop-env" /usr/local/libexec/ai-os-desktop-env
sed -i 's/\r$//' /usr/local/libexec/ai-os-desktop-env
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
for u in ai-os-desktop.service ai-os-engine.service; do
  install -m 0644 -o ai -g ai "$repo/trial/$u" /home/ai/.config/systemd/user/$u
  sed -i 's/\r$//' /home/ai/.config/systemd/user/$u
done
if [ -f "$repo/runtime/target/release/ai-os-engine" ]; then install -m 0755 -o root -g root "$repo/runtime/target/release/ai-os-engine" /usr/local/bin/ai-os-engine; fi
# The Phase 0 hand-run session must not sit beside the unit on the same bus.
systemctl --user -M ai@ stop ai-headless-shell.service 2>/dev/null || true
systemctl --user -M ai@ reset-failed ai-headless-shell.service 2>/dev/null || true
loginctl enable-linger ai
systemctl --user -M ai@ daemon-reload
systemctl --user -M ai@ enable ai-os-desktop.service ai-os-engine.service
systemctl --user -M ai@ restart ai-os-desktop.service
systemctl --user -M ai@ restart ai-os-engine.service
sleep 3
systemctl --user -M ai@ is-active ai-os-desktop.service ai-os-engine.service
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 busctl --user list | grep -E 'org.a11y.Bus|org.gnome.Mutter.ScreenCast' || true
echo "desktop ready"
```

- [ ] **Step 5: Run it and the survival check** — build first: `run-tests.sh build --release -p aios-core`. Then, with PowerShell: `wsl -d ai-os -u root -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-desktop.sh`. Expected: `active` twice, `org.a11y.Bus` listed, `desktop ready`. Then wait 90 s (a `Start-Sleep 90` in PowerShell, no wsl call meanwhile) and run `wsl -d ai-os -u ai -- bash -lc "systemctl --user is-active ai-os-desktop.service ai-os-engine.service; uptime -s"`.

Expected: `active` twice and the same boot time. If either unit is `inactive` or the boot time moved: the design's §8 check has found the fault. Diagnose with `journalctl --user -u ai-os-desktop -u ai-os-engine --since -5min` and `journalctl -u user@1000 -u systemd-logind --since -5min` as the probe did (`session.env`-style env: `XDG_RUNTIME_DIR=/run/user/1000`); the two known candidates are the user manager stopping with the last login session despite linger (look for `Stopping user@1000` right after the launching session closes; if so, `KillUserProcesses`/`StopIdleSessionSec` in `/etc/systemd/logind.conf` is the place to look) and the distro's idle shutdown (the boot time moves; then the answer is the keep-alive the parent spec §4.2 already calls for, out of 2a's scope but recorded). Record what was found in the design's Results block either way.

- [ ] **Step 6: The worker's live check on the unit** — `wsl -d ai-os -u ai -- bash -l /mnt/c/…/trial/run-desktop-live.sh` (Task 5's script; the display is `wayland-ai` now). Expected: pass.

- [ ] **Step 7: Commit**

```bash
git add trial/ai-os-desktop.service trial/ai-os-desktop-env trial/setup-desktop.sh trial/ai-os-engine.service
git commit -m "feat(workshop): the invisible session as a unit; the engine starts after it with its displays"
```

---

### Task 9: The live acceptance, the results, the status

**Files:**
- Create: `runtime/core/tests/live_2a.rs`, `trial/run-live-2a.sh`
- Modify: `docs/superpowers/specs/2026-09-17-phase2a-desktop-hand-design.md` (`### Results`), `docs/superpowers/specs/2026-09-15-ai-os-design.md` (`## Phase 2a status`)

- [ ] **Step 1: The test** — `runtime/core/tests/live_2a.rs`, the 1d scaffold with the real third hand:

```rust
// Phase 2a acceptance (2a design §12 item 4): the real model, three hands, the service on a temp
// socket, no human. Inside the distro with the desktop unit up:
//   trial/run-live-2a.sh   (sets the displays and AI_OS_LIVE=1)
use aios_core::engine::Engine;
use aios_core::model::{Model, OllamaModel};
use aios_core::service;
use aios_core::store::Store;
use aios_proto::{Client, Event};
use executor::admin::AdminWorker;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DB: &str = "/data/ai-os-live-2a.db";
const NOTES: &str = "/data/housekeeping/live-2a-notes.txt";

fn start_service() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ai-os-live-2a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    std::thread::spawn(move || service::run(listener, Box::new(|sink| {
        let model = OllamaModel::local("qwen3.5:9b");
        let desktop = DesktopState::for_model(model.context_tokens());
        Engine::new(Store::open(DB).unwrap(), model, PathBuf::from("/data/projects"), Some(DB.into()),
            Box::new(move |ws| (
                Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
                Box::new(AdminWorker) as Box<dyn Worker>,
                Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
            )),
            PathBuf::from("/data/housekeeping"), PathBuf::from("/data/snapshots")).with_sink(sink)
    })));
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

/// Say it, print every event, return once `stop` matches. Panics after `limit` seconds.
fn say_until(c: &mut Client, text: &str, limit: u64, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    println!("you> {text}");
    c.set_read_timeout(Some(Duration::from_secs(limit))).unwrap();
    c.say(text).unwrap();
    let t = Instant::now();
    let mut got = vec![];
    while let Some(e) = c.next_event() {
        println!("ev> {}", serde_json::to_string(&e).unwrap());
        let done = stop(&e);
        got.push(e);
        if done { return got; }
        assert!(t.elapsed().as_secs() < limit, "no end after {limit}s: {got:?}");
    }
    panic!("connection closed or nothing said for {limit}s: {got:?}");
}

fn ended(e: &Event) -> bool { matches!(e, Event::Done { .. } | Event::Failed { .. } | Event::Stopped { .. }) }

fn editor_running() -> bool {
    std::process::Command::new("pgrep").args(["-u", "ai", "-f", "gnome-text-editor"]).output().map(|o| !o.stdout.is_empty()).unwrap_or(false)
}

#[test]
fn a_window_of_the_users_a_window_of_its_own_and_a_risky_press() {
    if std::env::var("AI_OS_LIVE").is_err() { eprintln!("skipped: AI_OS_LIVE not set"); return; }
    let _ = std::fs::remove_file(DB);
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-text-editor|gnome-calculator"]).status();
    std::fs::write(NOTES, "first line\n").unwrap();
    // The user's window: on the visible display, opened by the user (here: the test), unsaved nothing yet.
    std::process::Command::new("gnome-text-editor").arg(NOTES)
        .env("WAYLAND_DISPLAY", "wayland-0").env("GNOME_ACCESSIBILITY", "1").env_remove("DISPLAY")
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(6));
    let sock = start_service();
    let mut c = Client::connect(&sock).unwrap();
    c.hello().unwrap();
    assert_eq!(c.next_event(), Some(Event::State { job: None }));

    // Script 1: the user's window.
    let ev = say_until(&mut c, "take my Text Editor window, add a line saying reviewed at the end and save it; decide everything yourself and do not ask", 500, ended);
    assert!(matches!(ev.last().unwrap(), Event::Done { .. }), "script 1: {ev:?}");
    let saved = std::fs::read_to_string(NOTES).unwrap();
    assert!(saved.to_lowercase().contains("reviewed"), "the file on disk has the line: {saved:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Done { windows, .. } if !windows.is_empty())), "the Done card names the window");

    // Script 2: the AI's own window, invisible.
    let ev = say_until(&mut c, "open the calculator and tell me what 12 times 34 is; decide everything yourself and do not ask", 500, ended);
    let Event::Done { text, .. } = ev.last().unwrap() else { panic!("script 2: {ev:?}") };
    assert!(text.contains("408") || ev.iter().any(|e| matches!(e, Event::Said { text } if text.contains("408"))), "the answer is said: {ev:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Step { text, .. } if text.starts_with("opened "))), "it opened the app itself");

    // Script 3: the risky press.
    let ev = say_until(&mut c, "take my Text Editor window and close it without saving; decide everything yourself and do not ask", 300,
        |e| matches!(e, Event::NeedsOk { .. }) || ended(e));
    let Event::NeedsOk { what, why, .. } = ev.last().unwrap() else { panic!("script 3 stopped at an OK: {ev:?}") };
    assert!(what.starts_with("pressed ") && why.contains("unsaved work"), "{what} / {why}");
    let ev = say_until(&mut c, "yes", 300, ended);
    let t = Instant::now();
    while editor_running() && t.elapsed() < Duration::from_secs(15) { std::thread::sleep(Duration::from_millis(500)); }
    assert!(!editor_running(), "the editor is gone after the yes: {ev:?}");
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-calculator"]).status();
}
```

`trial/run-live-2a.sh`:

```bash
#!/usr/bin/env bash
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export AI_OS_DISPLAY_INVISIBLE=wayland-ai AI_OS_DISPLAY_VISIBLE=wayland-0 AI_OS_LIVE=1
systemctl --user is-active ai-os-desktop.service || { echo "desktop unit not up: run trial/setup-desktop.sh as root"; exit 1; }
cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && timeout 1500 cargo test -p aios-core --test live_2a -- --nocapture 2>&1 | tail -120
```

- [ ] **Step 2: Run it** — `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-live-2a.sh` (PowerShell timeout 600000 ms; the script's own `timeout` is the real bound). The editor window appears on the Windows desktop during script 1 and 3; the calculator never does.

Expected: pass. A failure is analysed like 1c's and 1d's: a product gap is fixed in the code (the prompt, the hand, the rendering, the rules) and the run repeated; the assertion is never loosened to pass. Record every run and its cause in the design's Results block. Likely first gaps, from the probe: the model reaching for `run_command` to open an app (a prompt rule), the model pressing before looking (the "never handed out" refusal teaches it), a Save that opens a dialog because the editor considers the file untitled (then the acceptance's file must be one the editor opened by path, as written).

- [ ] **Step 3: The full suite** — `run-tests.sh test --workspace` inside the distro: every crate green (the gated tests skip without their env).

- [ ] **Step 4: Write the results** — in the 2a design add `### Results (2026-09-…)` under §12 with: runs, seconds per script, what failed and what was changed, the survival check's outcome from Task 8. In the parent spec add `## Phase 2a status` (what shipped, the §4.4 correction now in code, carry-forwards from the design's §13).

- [ ] **Step 5: Commit**

```bash
git add runtime/core/tests/live_2a.rs trial/run-live-2a.sh docs
git commit -m "test: Phase 2a live acceptance through the service; results recorded"
```

Then STOP: report to him in plain words; merging to master waits for his word, and item 5 of §12 (his own window) is his.

---

## Self-review against the spec

- §2 window job as `housekeep`: Task 7 (front door and job prompt); no new move (audit).
- §3 five actions and their fields: Task 1; ids and "gone": Tasks 4–5; press echoes and verifies the name: Task 5's `resolve`; `open_app` name rule and `.desktop` check: Tasks 1, 5; which display: Task 4's `Displays`, Task 5.
- §4 budget: `look_cap` Task 4, `context_tokens` Task 7; `read` windowed and `type` compacted: Tasks 5, 1.
- §5 worker in-process, zbus blocking, factory triple, shared state, environment, repeated press: Tasks 2, 3, 5, 8.
- §6 word list and reasons: Task 2; verified name before anything runs: Task 5.
- §7 undo line, `windows` with `#[serde(default)]`, computed from steps: Tasks 6, 7.
- §8 unit, pinned display, engine after it, survival check: Task 8.
- §10 ripples: every file named there has a task; `FakeWorker` unchanged, `DesktopRecorder` added (Task 3).
- §11 prompt: Task 7.
- §12 proofs: 1 Tasks 1, 2, 4, 5; 2 Tasks 3, 7; 3 Task 6; 4 Task 9; 5 his.
- Names used across tasks: `Action::{Look, Press, Type, Read, OpenApp}`, `valid_app_name`, `risky_press`, `Lane::Desktop`, `Executor::new(sandbox, admin, desktop, log, ws)`, `WorkerFactory` triple, `Recorder.desktop_calls`, `DesktopRecorder`, `desktop::{Node, IdTable, Entry, look_cap, select, render_look, Displays}`, `atspi::{DesktopState, DesktopWorker}`, `Model::context_tokens`, `windows_worked`, `Event::Done { windows }`, `window_note`, `CardKind::Done { windows }` — consistent throughout.
