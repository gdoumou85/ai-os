# Phase 1d — Engine Service and Chat Rail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the engine's prose lines into typed events, run the engine as a user service on a private socket, and give the user a native GTK4 rail window that draws the conversation as cards.

**Architecture:** A new tiny crate `proto` holds the wire types (`Event`, `JobState`) and the socket client; `aios-core` emits `Event`s through a sink as they happen (a stop flag lands between steps; a question at a Needs-your-OK is answered without counting as yes/no; Done carries the changed files) and gains a `service` module plus the `ai-os-engine` binary; `ai-os-chat` becomes a thin client; a new crate `rail` (GTK4) builds cards from events. Every front door is a socket client.

**Tech Stack:** Rust 2021 workspace at `runtime/`; serde/serde_json (`preserve_order`), rusqlite, ureq (existing); `gtk4` 0.9 + `glib` (new, rail only); std Unix sockets and threads (no async runtime); systemd user unit; bash setup script.

**Spec:** `docs/superpowers/specs/2026-09-16-phase1d-rail-design.md` (read first; §2 event table, §3.2 protocol, §4.2 card table, §7 proofs are the contract). Parent spec `2026-09-15-ai-os-design.md` §4.9.

## Global Constraints

- The discriminator is the first key of every JSON object a client or the model parses: `kind` on events, `move` on moves. `serde_json` `preserve_order` stays on in every crate that builds JSON for the wire.
- `handle(&mut self, &str) -> Result<Vec<String>, EngineError>` keeps returning exactly today's lines; the 93 existing core tests are not edited. A failing existing test is a finding about the code.
- Yes, no, stop and undo are fixed word lists, never the model. A question at a Needs-your-OK never approves or declines.
- The rail never builds a path from user text; it opens only paths the service sent.
- The socket is `$XDG_RUNTIME_DIR/ai-os.sock`, mode 0600, stale file removed at start. No new sudo grant; nothing runs as root except the existing wrapper.
- GTK is linked only in `runtime/rail`; `proto`, `executor`, `aios-core` build with no display libraries.
- All text files LF (`.gitattributes`). Commit after every task on branch `phase1d-rail`; never push, tag or merge without his word.
- Tests run inside the distro (no Rust on Windows): `wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo test -p <crate>"`. Root-only steps go through `wsl -d ai-os -u root -- bash -c "..."` and only via `trial/setup-rail.sh`.
- Never adjust a test to make it pass.

---

## File map

| File | Responsibility |
|---|---|
| `runtime/Cargo.toml` | workspace members `executor`, `proto`, `core`, `rail` |
| `runtime/proto/Cargo.toml`, `src/lib.rs` (new) | package `aios-proto`: `Event`, `ChangedFile`, `JobState`, `Waiting`; the `Client` (connect/say/hello/events) |
| `runtime/core/src/event.rs` (new) | `describe(&Action)`, `render::lines(&Event)` |
| `runtime/core/src/engine.rs` | `sink`, `out`, `emit`, `handle_events`, stop flag, `resume`, `is_refusal`, approval question, changed files, `state()` |
| `runtime/core/src/job.rs` | `started_at`, `ok_questions` |
| `runtime/core/src/prompt.rs` | `approval_question` |
| `runtime/core/src/service.rs` (new) | the socket service: client threads, engine thread, busy, mirror state, seq |
| `runtime/core/src/bin/ai-os-engine.rs` (new) | wires the real engine into `service::run` |
| `runtime/core/src/main.rs` | `ai-os-chat`, thin client |
| `runtime/core/src/testing.rs` | `events_of`, `FakeModel` unchanged |
| `runtime/core/tests/service.rs` (new) | two clients over a temp socket |
| `runtime/core/tests/live_1d.rs` (new) | live acceptance through the service |
| `runtime/rail/Cargo.toml`, `src/cards.rs`, `src/main.rs`, `tests/cards.rs` (new) | package `aios-rail`: card model + GTK window |
| `trial/setup-rail.sh`, `trial/ai-os-engine.service` (new) | workshop install of the three binaries and the user unit |
| `docs/superpowers/findings/phase1d-rail.png` | the screenshot |

---

### Task 1: The `proto` crate — events, job state, socket client

**Files:**
- Create: `runtime/proto/Cargo.toml`, `runtime/proto/src/lib.rs`
- Modify: `runtime/Cargo.toml`

**Interfaces:**
- Produces: `aios_proto::{Event, ChangedFile, FileKind, JobState, StepView, Waiting, UndoLine, Request, Client, Reader, Writer}` exactly as below; `Client::split(self) -> (Reader, Writer)`. Every later task uses these names.

- [ ] **Step 1: Add the crate to the workspace**

`runtime/Cargo.toml`:
```toml
[workspace]
members = ["executor", "proto", "core"]  # core dir holds package aios-core; "rail" is added in Task 9
resolver = "2"
```
(`rail` joins `members` in Task 9, when it exists; earlier and `cargo` fails.)

`runtime/proto/Cargo.toml`:
```toml
[package]
name = "aios-proto"
version = "0.0.1"
edition = "2021"

[lib]
name = "aios_proto"
path = "src/lib.rs"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = { version = "1", features = ["preserve_order"] }
```

- [ ] **Step 2: Write the failing tests** (in `src/lib.rs`, `#[cfg(test)] mod tests`)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_is_the_first_key_of_every_event() {
        let samples = vec![
            Event::Said { text: "hi".into() },
            Event::You { text: "yo".into() },
            Event::Understood { job_id: "j".into(), name: "p".into(), text: "Starting p".into(), housekeeping: false },
            Event::Plan { job_id: "j".into(), steps: vec!["a".into()] },
            Event::Step { job_id: "j".into(), plan_step: 1, text: "wrote a".into(), ok: true },
            Event::NeedsAnswer { job_id: "j".into(), questions: vec!["?".into()] },
            Event::NeedsOk { job_id: "j".into(), what: "http post to x".into(), why: "network".into() },
            Event::Done { job_id: "j".into(), text: "done".into(), check: Some("ran true".into()), files: vec![ChangedFile { path: "/a".into(), kind: FileKind::Text, size: 1 }] },
            Event::Failed { job_id: "j".into(), text: "gave up".into(), files: vec![] },
            Event::Stopped { job_id: "j".into(), text: "Stopped".into(), files: vec![] },
            Event::Undone { job_id: "j".into(), name: "p".into(), lines: vec![UndoLine { text: "put back".into(), ok: true }], notes: vec!["n".into()] },
            Event::Busy { job_id: "j".into(), text: "working".into() },
            Event::State { job: None },
            Event::Error { text: "bad".into() },
        ];
        for e in samples {
            let s = serde_json::to_string(&e).unwrap();
            assert!(s.starts_with("{\"kind\":\""), "{s}");
            let back: Event = serde_json::from_str(&s).unwrap();
            assert_eq!(back, e);
        }
    }

    #[test]
    fn state_round_trips_with_waiting() {
        let st = JobState {
            id: "j".into(), name: "p".into(), housekeeping: false, understood: "u".into(),
            plan: vec!["a".into()],
            steps: vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }],
            waiting: Waiting::Ok { what: "w".into(), why: "y".into() },
        };
        let e = Event::State { job: Some(st.clone()) };
        let back: Event = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
        let none: Waiting = serde_json::from_str("\"none\"").unwrap();
        assert_eq!(none, Waiting::None);
    }

    #[test]
    fn file_kind_from_extension() {
        assert_eq!(FileKind::of("a.png"), FileKind::Image);
        assert_eq!(FileKind::of("a.JPG"), FileKind::Image);
        assert_eq!(FileKind::of("a.py"), FileKind::Text);
        assert_eq!(FileKind::of("BLUEPRINT.md"), FileKind::Text);
        assert_eq!(FileKind::of("a.bin"), FileKind::Other);
        assert_eq!(FileKind::of("Makefile"), FileKind::Other);
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo test -p aios-proto"`
Expected: compile error, `Event` not found.

- [ ] **Step 4: Write the crate**

`runtime/proto/src/lib.rs`:
```rust
//! The wire between the engine service and its front doors (1d design §2, §3.2): typed events
//! out, plain text in. No engine types here — a client (the rail) must build without the
//! engine's dependencies.
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileKind { Text, Image, Other }

impl FileKind {
    pub fn of(path: &str) -> FileKind {
        let ext = Path::new(path).extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "svg" => FileKind::Image,
            "txt" | "md" | "py" | "rs" | "sh" | "js" | "ts" | "json" | "toml" | "yaml" | "yml" | "html" | "css" | "csv" | "c" | "h" | "cpp" | "go" | "java" | "rb" | "sql" | "ini" | "cfg" | "conf" | "log" | "xml" => FileKind::Text,
            _ => FileKind::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangedFile { pub path: String, pub kind: FileKind, pub size: u64 }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UndoLine { pub text: String, pub ok: bool }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepView { pub plan_step: usize, pub text: String, pub ok: bool }

/// What the open job is waiting for. `"none"` on the wire when nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Waiting { None, Answer { questions: Vec<String> }, Ok { what: String, why: String } }

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JobState {
    pub id: String,
    pub name: String,
    pub housekeeping: bool,
    pub understood: String,
    pub plan: Vec<String>,
    pub steps: Vec<StepView>,
    pub waiting: Waiting,
}

/// One event, `kind` first (the same first-key rule the move grammar lives by).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Said { text: String },
    /// Echo of a client's `say`, added by the service so every client shows it.
    You { text: String },
    Understood { job_id: String, name: String, text: String, housekeeping: bool },
    Plan { job_id: String, steps: Vec<String> },
    Step { job_id: String, plan_step: usize, text: String, ok: bool },
    NeedsAnswer { job_id: String, questions: Vec<String> },
    NeedsOk { job_id: String, what: String, why: String },
    Done { job_id: String, text: String, check: Option<String>, files: Vec<ChangedFile> },
    Failed { job_id: String, text: String, files: Vec<ChangedFile> },
    Stopped { job_id: String, text: String, files: Vec<ChangedFile> },
    Undone { job_id: String, name: String, lines: Vec<UndoLine>, notes: Vec<String> },
    Busy { job_id: String, text: String },
    State { job: Option<JobState> },
    /// The service could not read a client line. Sent to that client only.
    Error { text: String },
}

impl Event {
    /// The job this event belongs to, if any.
    pub fn job_id(&self) -> Option<&str> {
        match self {
            Event::Understood { job_id, .. } | Event::Plan { job_id, .. } | Event::Step { job_id, .. }
            | Event::NeedsAnswer { job_id, .. } | Event::NeedsOk { job_id, .. } | Event::Done { job_id, .. }
            | Event::Failed { job_id, .. } | Event::Stopped { job_id, .. } | Event::Undone { job_id, .. }
            | Event::Busy { job_id, .. } => Some(job_id),
            _ => None,
        }
    }
}

/// A line from a client: `{"say":"…"}` or `{"hello":{}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Request {
    #[serde(rename = "say")] Say(String),
    #[serde(rename = "hello")] Hello(serde_json::Map<String, serde_json::Value>),
}

/// One connection: a writer half and a line-reader half over the same socket. `split` hands the
/// two halves to two threads (a front door types on one and listens on the other); a second
/// connection for that would be a client the service broadcasts to and nobody reads.
pub struct Client { writer: Writer, reader: Reader }
pub struct Writer { stream: UnixStream }
pub struct Reader { lines: BufReader<UnixStream> }

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Client> {
        let s = UnixStream::connect(socket)?;
        Ok(Client { reader: Reader { lines: BufReader::new(s.try_clone()?) }, writer: Writer { stream: s } })
    }
    pub fn split(self) -> (Reader, Writer) { (self.reader, self.writer) }
    pub fn say(&mut self, text: &str) -> std::io::Result<()> { self.writer.say(text) }
    pub fn hello(&mut self) -> std::io::Result<()> { self.writer.hello() }
    pub fn next_event(&mut self) -> Option<Event> { self.reader.next_event() }
}

impl Writer {
    fn send(&mut self, r: &Request) -> std::io::Result<()> {
        let mut line = serde_json::to_string(r).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.stream.write_all(line.as_bytes())
    }
    pub fn say(&mut self, text: &str) -> std::io::Result<()> { self.send(&Request::Say(text.to_string())) }
    pub fn hello(&mut self) -> std::io::Result<()> { self.send(&Request::Hello(Default::default())) }
}

impl Reader {
    /// The next event, or `None` when the service closed the connection. A line that is not an
    /// event is skipped (a newer service may send kinds this client does not know). The
    /// service's extra `seq` key is ignored by serde.
    pub fn next_event(&mut self) -> Option<Event> {
        loop {
            let mut line = String::new();
            match self.lines.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => if let Ok(e) = serde_json::from_str::<Event>(&line) { return Some(e); },
            }
        }
    }
}
```
(Add the tests block from Step 2 at the bottom.)

- [ ] **Step 5: Run to verify they pass**

Run: same command. Expected: 3 passed. Also `cargo build --workspace` still green.

- [ ] **Step 6: Commit**

```bash
git checkout -b phase1d-rail
git add runtime/Cargo.toml runtime/proto
git commit -m "feat(proto): typed events, job state and the socket client"
```

---

### Task 2: The engine emits events

**Files:**
- Create: `runtime/core/src/event.rs`
- Modify: `runtime/core/Cargo.toml` (+ `aios-proto = { path = "../proto" }`), `runtime/core/src/lib.rs` (+ `pub mod event;`), `runtime/core/src/engine.rs`, `runtime/core/src/testing.rs`

**Interfaces:**
- Consumes: `aios_proto::Event`.
- Produces: `Engine::with_sink(self, Box<dyn FnMut(&Event)>) -> Self`; `Engine::handle_events(&mut self, &str) -> Result<Vec<Event>, EngineError>` (emits each event to the sink the moment it is produced, returns them all at the end); `Engine::handle` unchanged in signature and output; `event::describe(&Action) -> String`; `event::lines(&Event) -> Vec<String>`; `testing::events_of(&mut Engine<FakeModel>, &str) -> Vec<Event>`.

- [ ] **Step 1: Write the failing tests** (append to `engine.rs`'s `mod tests`)

```rust
    use aios_proto::Event;
    use crate::testing::events_of;

    #[test]
    fn chat_emits_said() {
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "A prime is…".into(), remember: Some("use python3".into()) }], "ev-said");
        let ev = events_of(&mut e, "what's a prime?");
        assert_eq!(ev, vec![Event::Said { text: "A prime is…".into() }, Event::Said { text: "(Noted for the future: use python3)".into() }]);
    }

    #[test]
    fn a_job_emits_understood_plan_steps_and_done_with_the_check() {
        let (mut e, _, _) = engine_with(happy_path(), "ev-job");
        let ev = events_of(&mut e, "make it, decide yourself");
        let job_id = e.store.last_undoable_job().unwrap().unwrap().id;
        assert!(matches!(&ev[0], Event::Understood { job_id: j, name, text, housekeeping: false } if j == &job_id && name == "p" && text == "Starting a new project p"));
        assert!(matches!(&ev[1], Event::Plan { steps, .. } if steps == &vec!["write it".to_string(), "run it".to_string()]));
        let steps: Vec<&Event> = ev.iter().filter(|e| matches!(e, Event::Step { .. })).collect();
        assert_eq!(steps.len(), 4, "3 acts + the check: {ev:?}");
        assert!(matches!(steps[0], Event::Step { plan_step: 1, text, ok: true, .. } if text == "wrote BLUEPRINT.md"));
        assert!(matches!(steps[3], Event::Step { text, .. } if text == "ran python3"));
        assert!(matches!(ev.last().unwrap(), Event::Done { text, check: Some(c), .. } if text == "finished" && c == "ran python3: ok"));
    }

    #[test]
    fn events_stream_to_the_sink_as_they_happen() {
        use std::cell::RefCell; use std::rc::Rc;
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let s2 = seen.clone();
        let (e, _, _) = engine_with(happy_path(), "ev-sink");
        let mut e = e.with_sink(Box::new(move |ev| s2.borrow_mut().push(serde_json::to_string(ev).unwrap())));
        let ev = e.handle_events("make it").unwrap();
        assert_eq!(seen.borrow().len(), ev.len());
        assert!(seen.borrow()[0].starts_with("{\"kind\":\"understood\""));
    }

    #[test]
    fn questions_and_approvals_are_events() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Which language?".into()] }], "ev-ask");
        let ev = events_of(&mut e, "make it");
        assert!(matches!(ev.last().unwrap(), Event::NeedsAnswer { questions, .. } if questions == &vec!["Which language?".to_string()]));

        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, Action::HttpPost { url: "https://x".into(), body: "b".into() })], "ev-ok");
        let ev = events_of(&mut e, "post it");
        assert!(matches!(ev.last().unwrap(), Event::NeedsOk { what, why, .. } if what == "http post to https://x" && !why.is_empty()), "{ev:?}");
    }

    #[test]
    fn stop_and_give_up_are_events_and_lines_match_the_old_prose() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()] }], "ev-stop");
        e.handle("make p").unwrap();
        let ev = events_of(&mut e, "stop");
        assert!(matches!(&ev[0], Event::Stopped { text, .. } if text == "Stopped the job in p."));
        assert_eq!(crate::event::lines(&ev[0]), vec!["Stopped the job in p.".to_string()]);

        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), Move::GiveUp { reason: "no compiler".into(), missing: "gcc".into() }], "ev-giveup");
        let ev = events_of(&mut e, "make p");
        assert!(matches!(ev.last().unwrap(), Event::Failed { text, .. } if text == "I gave up on p: no compiler. Missing: gcc."));
    }

    #[test]
    fn describe_names_every_action_in_plain_words() {
        use crate::event::describe;
        use executor::action::{Manager, ServiceDo};
        assert_eq!(describe(&write("a.py")), "wrote a.py");
        assert_eq!(describe(&Action::EditFile { path: "a.py".into(), find: "x".into(), replace: "y".into() }), "edited a.py");
        assert_eq!(describe(&Action::ReadFile { path: "a.py".into(), from_line: None, lines: None }), "read a.py");
        assert_eq!(describe(&Action::RunCommand { argv: vec!["python3".into(), "a.py".into()] }), "ran python3 a.py");
        assert_eq!(describe(&Action::HttpPost { url: "https://x".into(), body: "".into() }), "http post to https://x");
        assert_eq!(describe(&Action::Install { packages: vec!["cowsay".into()] }), "installed cowsay");
        assert_eq!(describe(&Action::Remove { packages: vec!["cowsay".into()] }), "removed cowsay");
        assert_eq!(describe(&Action::Service { name: "nginx".into(), action: ServiceDo::Restart }), "service nginx restart");
        assert_eq!(describe(&Action::MakeDir { path: "/data/work".into() }), "made folder /data/work");
        assert_eq!(describe(&Action::FetchPackages { manager: Manager::Pip, packages: vec!["tabulate".into()] }), "fetched tabulate with pip");
        assert_eq!(describe(&Action::SetSetting { key: "projects_root".into(), value: "/data/work".into() }), "set projects_root = /data/work");
    }
```
Field names are those of `runtime/executor/src/action.rs` (`Service { name, action }`, `ReadFile { path, from_line, lines }`); the path check confirmed every other variant in `describe` matches.

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p aios-core` (in the distro). Expected: compile errors (`events_of`, `with_sink`, `event` missing).

- [ ] **Step 3: `event.rs`**

```rust
//! Plain words for actions, and the lines the terminal prints for an event (1d design §2.1).
//! `lines` reproduces, word for word, what `Engine::handle` printed before events existed.
use aios_proto::Event;
use executor::action::{Action, Manager, ServiceDo};

pub fn describe(action: &Action) -> String {
    match action {
        Action::WriteFile { path, .. } => format!("wrote {path}"),
        Action::EditFile { path, .. } => format!("edited {path}"),
        Action::ReadFile { path, .. } => format!("read {path}"),
        Action::RunCommand { argv } => format!("ran {}", argv.join(" ")),
        Action::HttpPost { url, .. } => format!("http post to {url}"),
        Action::Install { packages } => format!("installed {}", packages.join(" ")),
        Action::Remove { packages } => format!("removed {}", packages.join(" ")),
        // The field is `action` in Rust; `do` is only its serde name (action.rs:24).
        Action::Service { name, action } => format!("service {name} {}", match action { ServiceDo::Enable => "enable", ServiceDo::Disable => "disable", ServiceDo::Restart => "restart" }),
        Action::MakeDir { path } => format!("made folder {path}"),
        Action::FetchPackages { manager, packages } => format!("fetched {} with {}", packages.join(" "), match manager { Manager::Pip => "pip", Manager::Npm => "npm", Manager::Cargo => "cargo" }),
        Action::SetSetting { key, value } => format!("set {key} = {value}"),
    }
}

/// The terminal's lines for one event. `plan`, `step` and `state` print nothing: the terminal
/// never showed steps, and the rail is what draws them.
pub fn lines(event: &Event) -> Vec<String> {
    match event {
        Event::Said { text } | Event::Understood { text, .. } | Event::Done { text, .. }
        | Event::Failed { text, .. } | Event::Stopped { text, .. } | Event::Busy { text, .. } => vec![text.clone()],
        Event::You { .. } | Event::Plan { .. } | Event::Step { .. } | Event::State { .. } => vec![],
        Event::NeedsAnswer { questions, .. } => questions.iter().map(|q| format!("Question: {q}")).collect(),
        Event::NeedsOk { why, .. } => vec![format!("Needs your OK: {why}. Say yes to allow it, or anything else to refuse.")],
        Event::Undone { name, lines, notes, .. } => {
            let mut v = vec![format!("Undoing the last job ({name}):")];
            v.extend(lines.iter().map(|l| l.text.clone()));
            v.extend(notes.iter().cloned());
            v
        }
        Event::Error { text } => vec![format!("(error: {text})")],
    }
}
```
If the `Action` enum's variant fields differ from the names above, follow `action.rs`; the match must stay exhaustive (no `_` arm), so a new action kind is a compile error here.

- [ ] **Step 4: The engine refactor**

In `engine.rs`:

1. Fields and constructor:
```rust
pub struct Engine<M: Model> {
    // … existing fields …
    /// Where events go the moment they happen (the service's broadcast). Default: nowhere.
    sink: Box<dyn FnMut(&Event)>,
    /// The events of the `handle_events` call in progress, returned at its end.
    out: Vec<Event>,
}
// in new(): sink: Box::new(|_| {}), out: vec![],
pub fn with_sink(mut self, sink: Box<dyn FnMut(&Event)>) -> Self { self.sink = sink; self }
fn emit(&mut self, ev: Event) { (self.sink)(&ev); self.out.push(ev); }
```
2. Entry points:
```rust
/// One user message in, the lines to show the user out — the terminal's view of `handle_events`.
pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
    Ok(self.handle_events(text)?.iter().flat_map(crate::event::lines).collect())
}
/// The same, as typed events; each one reached the sink as it happened.
pub fn handle_events(&mut self, text: &str) -> Result<Vec<Event>, EngineError> {
    self.store.push_message("user", text)?;
    self.out.clear();
    let r = self.handle_inner(text);
    // The events already reached the sink (and every client) even when `r` is an error, so the
    // conversation history must hold them too — record before propagating.
    let out = std::mem::take(&mut self.out);
    for l in out.iter().flat_map(crate::event::lines) { self.store.push_message("assistant", &l)?; }
    r?;
    Ok(out)
}
```
3. Return types: every private function that returned `Result<Vec<String>, EngineError>` (`handle_inner`, `finish`, `undo_last`, `resume_after_approval`, `run_turns`) now returns `Result<(), EngineError>` and emits instead; every one that returned `Result<Option<Vec<String>>, EngineError>` (`perform`, `reject`) returns `Result<bool, EngineError>` where `true` means "the job stopped here, return". `finish`, `perform`, `reject` take `&mut self`. Every `return Ok(vec![…])` becomes `self.emit(…); return Ok(())`; every `if let Some(stop) = … { return Ok(stop); }` becomes `if self.perform(…)? { return Ok(()); }`.
4. Which event where (the mapping is the contract; wording of `text` unchanged):
   - `Reply` → `Said{text}`; the remember note → `Said{"(Noted for the future: …)"}` (both arms, Start/Housekeep too).
   - `Start`/`Housekeep` accepted → `Understood{job_id: job.id, name: display_name(&job), text: understood, housekeeping}` FIRST, then the `Said` note, then `run_turns` — the same order as today's lines (`start_creates_project_folder_and_job_and_says_what_it_understood` pins `out[0]`). The "I could not start *name*: …" lines → `Said`.
   - `Plan` accepted (both the `Plan` arm and `Replan`) → `Plan{job_id, steps: job.plan.clone()}`.
   - In `perform`, after a `StepRecord` is pushed (both the `SetSetting` branch and the `Ran` branch) → `Step{job_id, plan_step, text: describe(&action), ok}`.
   - `Blocked(reason)` → `NeedsOk{job_id, what: describe(&action), why: reason}`.
   - `Ask` accepted → `NeedsAnswer{job_id, questions}`.
   - `finish(job, State::Done, text)` → `Done{job_id, text, check, files: vec![]}` where `check = job.steps.last().map(|s| format!("{}: {}", describe(&s.action), if s.ok {"ok"} else {"failed"}))`; `State::Failed` → `Failed{…, files: vec![]}`; `State::Cancelled` → `Stopped{…, files: vec![]}`. (`files` is filled in Task 3.)
   - The pre-1c "predates the folder record" job → `Failed`.
   - `undo_last`: "Nothing left to undo." and "Finish or stop the current job first…" → `Said`; otherwise ONE `Undone{job_id, name: display_name(&job), lines, notes}` built up and emitted at the end of the function, where each row's sentence is `UndoLine{text: <the same sentence as today>, ok}` and `notes` holds "Undo stopped early: …" (when it happens; then return after emitting), the "Not covered: …" line and the "Files in *…* were not covered…" line.
   - `(I answered out of turn — …)` → `Said`.
5. `testing.rs`:
```rust
pub fn events_of(e: &mut crate::engine::Engine<crate::model::FakeModel>, text: &str) -> Vec<aios_proto::Event> {
    e.handle_events(text).unwrap()
}
```
6. `lib.rs`: `pub mod event;`. `Cargo.toml`: `aios-proto = { path = "../proto" }`.

- [ ] **Step 5: Run the whole core suite**

Run: `cargo test -p aios-core`. Expected: all 93 existing tests pass untouched, plus the 6 new ones. If an existing test fails, the rendering differs from the old prose: fix `lines` or the mapping, never the test.

- [ ] **Step 6: Commit**

```bash
git add runtime/core
git commit -m "feat(core): the engine emits typed events; handle() renders them to the old lines"
```

---

### Task 3: Changed files on Done, Failed and Stopped

**Files:**
- Modify: `runtime/core/src/job.rs`, `runtime/core/src/engine.rs`

**Interfaces:**
- Produces: `Job.started_at: u64` (unix seconds, `serde(default)`); `engine::changed_files(folder: &Path, since: u64) -> Vec<ChangedFile>` (pub(crate)); `Done/Failed/Stopped.files` filled.

- [ ] **Step 1: Failing tests**

`job.rs` tests:
```rust
    #[test]
    fn started_at_is_set_and_optional_on_the_wire() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        assert!(job.started_at > 1_700_000_000);
        let mut value = serde_json::to_value(&job).unwrap();
        value.as_object_mut().unwrap().remove("started_at");
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(back.started_at, 0);
    }
```
`engine.rs` tests:
```rust
    #[test]
    fn done_lists_the_files_the_job_changed() {
        // A new project refuses a folder that already exists, so the old files are planted
        // AFTER a first job created the folder, and the second job is the one measured.
        let again = Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "add more".into(), creative: true, understood: "Continuing p".into(), remember: None };
        let (mut e, _, root) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
            again, plan(), act(1, write("BLUEPRINT.md")), act(1, write("primes.py")), act(1, write("BLUEPRINT.md")), done(run("python3")),
        ], "files");
        e.handle("make p").unwrap();
        let old = root.join("p"); std::fs::create_dir_all(old.join("node_modules")).unwrap();
        std::fs::write(old.join("old.txt"), "x").unwrap();
        std::fs::write(old.join("node_modules").join("lib.js"), "x").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        for p in [old.join("old.txt"), old.join("node_modules").join("lib.js"), old.join("BLUEPRINT.md")] {
            std::fs::File::open(&p).unwrap().set_modified(past).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1100)); // started_at is whole seconds
        let ev = events_of(&mut e, "add primes to p");
        let Event::Done { files, .. } = ev.last().unwrap() else { panic!("{ev:?}") };
        let names: Vec<&str> = files.iter().map(|f| f.path.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, vec!["BLUEPRINT.md", "primes.py"], "{files:?}");
        assert!(files.iter().all(|f| f.path.starts_with(old.to_str().unwrap())), "absolute paths");
        assert!(matches!(files[1].kind, aios_proto::FileKind::Text));
    }

    #[test]
    fn a_failed_job_does_not_list_the_engines_own_last_run_note() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), Move::GiveUp { reason: "r".into(), missing: "m".into() }], "files-lastrun");
        let ev = events_of(&mut e, "make p");
        let Event::Failed { files, .. } = ev.last().unwrap() else { panic!("{ev:?}") };
        assert!(files.iter().all(|f| !f.path.ends_with("LAST_RUN.md")), "{files:?}");
    }

    #[test]
    fn changed_files_is_capped_and_sorted() {
        let root = crate::testing::temp_root("files-cap");
        for i in 0..25 { std::fs::write(root.join(format!("f{i:02}.txt")), "x").unwrap(); }
        let files = changed_files(&root, 0);
        assert_eq!(files.len(), 20);
        assert!(files[0].path.ends_with("f00.txt"));
        assert!(files[19].path.ends_with("f19.txt"));
    }
```
(`set_modified` is stable since Rust 1.75; the distro has 1.98.)

- [ ] **Step 2: Run to verify they fail** — `cargo test -p aios-core files` → compile error.

- [ ] **Step 3: Implement**

`job.rs`: add to `Job`:
```rust
    /// When the job began (unix seconds): the Done card lists what changed since then.
    /// `#[serde(default)]`: a job saved before 1d has no such key.
    #[serde(default)]
    pub started_at: u64,
```
and in `Job::new`: `started_at: millis as u64 / 1000` — note `millis` is the existing `u128`; write `started_at: (millis / 1000) as u64`.

`engine.rs`:
```rust
/// What the job left behind: entries under `folder` modified at or after `since` (unix
/// seconds), hidden entries and dependency folders skipped, sorted by path, at most 20.
/// ponytail: mtime in whole seconds, not content — a file touched but unchanged is listed, and
/// so is one the user touched in the same second the job began; content diffing against the
/// snapshot is the upgrade if that ever misleads.
pub(crate) fn changed_files(folder: &Path, since: u64) -> Vec<ChangedFile> {
    const SKIP: [&str; 2] = ["node_modules", "target"]; // hidden names (.git, .venv) are skipped below
    fn walk(dir: &Path, since: u64, out: &mut Vec<ChangedFile>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) { continue; }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() { walk(&path, since, out); continue; }
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            if mtime >= since {
                let p = path.display().to_string();
                out.push(ChangedFile { kind: FileKind::of(&p), path: p, size: meta.len() });
            }
        }
    }
    let mut out = vec![];
    walk(folder, since, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.truncate(20);
    out
}
```
In `finish`: `let files = changed_files(&self.workspace(&job), job.started_at);` computed BEFORE `write_or_wipe_last_run` runs (that writes `LAST_RUN.md` into the same folder on Failed/Cancelled, and the engine's own note is not a file the job changed), then put on the Done/Failed/Stopped event (housekeeping jobs: `self.workspace(&job)` is the housekeeping folder already, since `Job.folder` holds it). Import `aios_proto::{ChangedFile, FileKind}`.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → all pass.

- [ ] **Step 5: Commit** — `git commit -am "feat(core): Done, Failed and Stopped carry the files the job changed"`

---

### Task 4: The stop flag and `resume`

**Files:**
- Modify: `runtime/core/src/engine.rs`

**Interfaces:**
- Produces: `Engine::stop_flag(&self) -> Arc<AtomicBool>` (clone of the engine's own); `Engine::resume(&mut self) -> Result<Vec<Event>, EngineError>` (carries on a job left mid-work; emits; `Ok(vec![])` when nothing to resume).

- [ ] **Step 1: Failing tests**

```rust
    #[test]
    fn a_raised_stop_flag_cancels_the_job_between_steps() {
        // The model would write two files; the flag is raised by the first step's worker call.
        let (mut e, rec, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(1, write("b.py")), done(run("true"))], "flag");
        let flag = e.stop_flag();
        let f2 = flag.clone();
        // ScriptedWorker records calls; raise the flag once the first call has landed by
        // scripting the outcome queue: the outcome itself cannot run code, so use a sink.
        let calls = rec.calls.clone();
        let mut e = e.with_sink(Box::new(move |ev| if matches!(ev, Event::Step { .. }) && calls.borrow().len() == 1 { f2.store(true, std::sync::atomic::Ordering::SeqCst) }));
        let ev = e.handle_events("make p").unwrap();
        assert!(matches!(ev.last().unwrap(), Event::Stopped { text, .. } if text == "Stopped the job in p."), "{ev:?}");
        assert_eq!(rec.calls.borrow().len(), 1, "the second write never ran");
        assert!(e.open_job().unwrap().is_none());
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst), "the flag is cleared once honoured");
    }

    #[test]
    fn resume_carries_on_a_job_left_mid_work() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "resume");
        e.handle("make p").unwrap();
        // Rewind the saved job to Working with the plan but no steps, like a crash after planning.
        let mut job = e.store.last_undoable_job().unwrap().unwrap();
        job.state = State::Working; job.steps.clear(); job.outcome_text.clear();
        e.store.save_job(&job).unwrap();
        e.model = crate::model::FakeModel::new(vec![act(1, write("BLUEPRINT.md")), done(run("true"))]);
        let ev = e.resume().unwrap();
        assert!(matches!(ev.last().unwrap(), Event::Done { .. }), "{ev:?}");
        assert!(e.open_job().unwrap().is_none());
        assert!(e.resume().unwrap().is_empty(), "nothing open, nothing to resume");
    }
```

- [ ] **Step 2: Run to verify they fail** — compile error on `stop_flag`/`resume`.

- [ ] **Step 3: Implement**

```rust
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
// field: stop: Arc<AtomicBool>,   in new(): stop: Arc::new(AtomicBool::new(false)),
pub fn stop_flag(&self) -> Arc<AtomicBool> { self.stop.clone() }

/// A job left mid-work (a restart): carry on with it. Nothing open → nothing emitted.
pub fn resume(&mut self) -> Result<Vec<Event>, EngineError> {
    self.out.clear();
    let job = match self.open_job()? { Some(j) if matches!(j.state, State::Working | State::Planning | State::Asking) => j, _ => return Ok(vec![]) };
    let r = self.run_turns(job);
    let out = std::mem::take(&mut self.out);
    r?;
    for l in out.iter().flat_map(crate::event::lines) { self.store.push_message("assistant", &l)?; }
    Ok(out)
}
```
One helper, called at the top of `run_turns`'s `loop` (before the MAX_STEPS check) and at the top of `perform` (so an action the user approved a moment before stopping does not run either — design §2.1):
```rust
    /// True when the user's stop has landed: the job is finished as Cancelled and reported.
    fn stopped(&mut self, job: &Job) -> Result<bool, EngineError> {
        if !self.stop.swap(false, Ordering::SeqCst) { return Ok(false); }
        let message = format!("Stopped the job in {}.", display_name(job));
        self.finish(job.clone(), State::Cancelled, message)?;
        Ok(true)
    }
```
In `run_turns`: `if self.stopped(&job)? { return Ok(()); }`. In `perform`: `if self.stopped(job)? { return Ok(true); }` as the first line.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → all pass.

- [ ] **Step 5: Commit** — `git commit -am "feat(core): a stop flag lands between steps; resume() carries on an interrupted job"`

---

### Task 5: A question at a Needs-your-OK

**Files:**
- Modify: `runtime/core/src/engine.rs`, `runtime/core/src/prompt.rs`, `runtime/core/src/job.rs`

**Interfaces:**
- Produces: `engine::is_refusal(&str) -> bool` (pub); `prompt::approval_question(instructions, job, what: &str, why: &str, question: &str) -> Prompt` with `allowed: vec!["reply"]`; `Job.ok_questions: u32` (`serde(default)`).

- [ ] **Step 1: Failing tests**

`engine.rs`:
```rust
    #[test]
    fn refusal_words() {
        for t in ["no", "No.", "n", "don't", "dont", "no thanks", "refuse", "not that", "cancel that", "no, don't"] { assert!(is_refusal(t), "{t}"); }
        for t in ["what does it send?", "yes", "why", "hmm"] { assert!(!is_refusal(t), "{t}"); }
    }

    /// `tag` must differ per test: `temp_root` wipes the tag's folder, and tests run in parallel.
    fn waiting_ok(tag: &str) -> (crate::engine::Engine<crate::model::FakeModel>, Recorder) {
        let (mut e, rec, _) = engine_with(vec![start("p", true), plan(), act(1, Action::HttpPost { url: "https://x".into(), body: "b".into() })], tag);
        e.handle("post it").unwrap();
        assert_eq!(e.open_job().unwrap().unwrap().state, State::WaitingApproval);
        (e, rec)
    }

    #[test]
    fn a_question_at_needs_ok_is_answered_and_the_ok_asked_again() {
        let (mut e, rec) = waiting_ok("okq-question");
        e.model = crate::model::FakeModel::new(vec![Move::Reply { text: "It sends the body b to x.".into(), remember: None }]);
        let ev = events_of(&mut e, "what does it send?");
        assert!(matches!(&ev[0], Event::Said { text } if text == "It sends the body b to x."), "{ev:?}");
        assert!(matches!(&ev[1], Event::NeedsOk { what, .. } if what == "http post to https://x"));
        let job = e.open_job().unwrap().unwrap();
        assert_eq!(job.state, State::WaitingApproval);
        assert!(job.pending_action.is_some(), "the action is still waiting");
        assert_eq!(job.ok_questions, 1);
        assert!(rec.calls.borrow().is_empty() && rec.admin_calls.borrow().is_empty(), "nothing ran");
        let p = e.model.prompts.borrow();
        assert_eq!(p.last().unwrap().allowed, vec!["reply"]);
        assert!(p.last().unwrap().user.contains("what does it send?"));
        assert!(p.last().unwrap().user.contains("http post to https://x"));
    }

    #[test]
    fn no_still_declines_and_yes_still_approves() {
        let (mut e, rec) = waiting_ok("okq-no");
        e.model = crate::model::FakeModel::new(vec![Move::GiveUp { reason: "declined".into(), missing: "your ok".into() }]);
        let ev = events_of(&mut e, "no thanks");
        assert!(matches!(ev.last().unwrap(), Event::Failed { .. }), "{ev:?}");
        assert!(rec.calls.borrow().is_empty());
        let (mut e, rec) = waiting_ok("okq-yes");
        e.model = crate::model::FakeModel::new(vec![done(run("true"))]);
        e.handle("yes").unwrap();
        assert_eq!(rec.calls.borrow().len(), 2, "the post ran, then the check");
    }

    #[test]
    fn three_questions_without_an_answer_decline_the_action() {
        let (mut e, _) = waiting_ok("okq-three");
        e.model = crate::model::FakeModel::new(vec![
            Move::Reply { text: "a".into(), remember: None }, Move::Reply { text: "b".into(), remember: None }, Move::Reply { text: "c".into(), remember: None },
            Move::GiveUp { reason: "declined".into(), missing: "your ok".into() },
        ]);
        e.handle("why?").unwrap(); e.handle("why though?").unwrap();
        let ev = events_of(&mut e, "and why is that?");
        assert!(matches!(&ev[0], Event::Said { text } if text == "c"));
        assert!(matches!(&ev[1], Event::Said { text } if text.contains("taking that as a no")), "{ev:?}");
        assert!(matches!(ev.last().unwrap(), Event::Failed { .. }), "{ev:?}");
    }
```
`prompt.rs`:
```rust
    #[test]
    fn approval_question_is_reply_only_and_carries_the_action() {
        let job = Job::new("p", "/data/projects/p", "g", false, "u");
        let p = approval_question(&[], &job, "http post to https://x", "network access", "what does it send?");
        assert_eq!(p.allowed, vec!["reply"]);
        assert!(p.user.contains("http post to https://x") && p.user.contains("network access") && p.user.contains("what does it send?"));
        assert!(p.user.contains("Do not") , "tells the model not to act");
    }
```
Also add `approval_question(&[], &job, "w", "y", "q")` to the existing `every_allowed_list_matches_a_real_move` test's `check(...)` calls in `prompt.rs`, so a typo in the new `allowed` list fails a test rather than only a `debug_assert` at runtime.

- [ ] **Step 2: Run to verify they fail** — compile errors.

- [ ] **Step 3: Implement**

`job.rs`: `#[serde(default)] pub ok_questions: u32,` (init 0 in `new`).

`prompt.rs`:
```rust
/// The user asked something instead of yes or no while an action waits for their OK (1d §2.1).
/// Reply only: the answer is words, never a move that changes the job.
pub fn approval_question(instructions: &[String], job: &Job, what: &str, why: &str, question: &str) -> Prompt {
    let user = format!(
        "Standing instructions:\n{}\n\nJob: {} — {}\nAn action is waiting for the user's OK: {what} (reason: {why}).\nThe user asked: {question}\nAnswer the question in one or two plain sentences so they can decide. Do not act, do not decide for them, do not ask them for the OK yourself (the system asks again).",
        join_instructions(instructions), if job.housekeeping { "housekeeping" } else { &job.project }, job.goal,
    );
    Prompt { system: SYSTEM.into(), user, allowed: vec!["reply"] }
}
```
`engine.rs`:
```rust
pub fn is_refusal(text: &str) -> bool {
    const NO_WORDS: [&str; 8] = ["no", "n", "don't", "dont", "no thanks", "refuse", "not that", "cancel that"];
    let t = normalize(text);
    NO_WORDS.contains(&t.as_str()) || matches!(words(&t).next(), Some(first) if NO_WORDS.contains(&first))
}
```
In `handle_inner`, the `State::WaitingApproval` arm:
```rust
                State::WaitingApproval => {
                    if is_yes(text) { return self.resume_after_approval(job, true, text); }
                    if is_refusal(text) { return self.resume_after_approval(job, false, text); }
                    // Neither: a question. Answer it, then ask for the OK again — the action
                    // stays exactly where it is. Bounded like every other loop the model is in.
                    let (what, why) = match &job.pending_action {
                        Some((_, a)) => (crate::event::describe(a), job.pending_reason.clone()),
                        None => { job.state = State::Working; self.store.save_job(&job)?; return self.run_turns(job); }
                    };
                    let p = prompt::approval_question(&self.store.instructions()?, &job, &what, &why, text);
                    let answer = match self.model.next_move(&p)? { Move::Reply { text, .. } => text, other => format!("(I answered out of turn — {other:?})") };
                    self.emit(Event::Said { text: answer });
                    job.ok_questions += 1;
                    if job.ok_questions >= 3 {
                        self.emit(Event::Said { text: "Three questions and no yes: taking that as a no.".into() });
                        return self.resume_after_approval(job, false, "no answer after three questions");
                    }
                    self.store.save_job(&job)?;
                    self.emit(Event::NeedsOk { job_id: job.id.clone(), what, why });
                    Ok(())
                }
```
In `resume_after_approval`, set `job.ok_questions = 0;` right after `job.state = State::Working;`.

- [ ] **Step 4: Run** — `cargo test -p aios-core` → all pass. The old test `names_and_yes_no` and any test that sent a non-yes word as a refusal (grep `handle("no` / `"nope"` / `"not now"` in engine tests) still pass only if those words are on the refusal list or are yes-negations; if one now goes to the model and fails with `Exhausted`, that is a real ambiguity — add the word to `NO_WORDS` only if it is plainly a refusal in English; otherwise report it as a finding.

- [ ] **Step 5: Commit** — `git commit -am "feat(core): a question at Needs-your-OK is answered and the OK asked again"`

---

### Task 6: The `state` snapshot

**Files:**
- Modify: `runtime/core/src/engine.rs`

**Interfaces:**
- Produces: `Engine::state(&self) -> Result<Option<JobState>, EngineError>`.

- [ ] **Step 1: Failing tests**

```rust
    #[test]
    fn state_mirrors_the_open_job_and_what_it_waits_for() {
        use aios_proto::Waiting;
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Which language?".into()] }], "state-ans");
        assert!(e.state().unwrap().is_none());
        e.handle("make p").unwrap();
        let st = e.state().unwrap().unwrap();
        assert_eq!((st.name.as_str(), st.housekeeping, st.understood.as_str()), ("p", false, "Starting a new project p"));
        assert_eq!(st.waiting, Waiting::Answer { questions: vec!["Which language?".into()] });

        let (mut e, _) = waiting_ok("state-ok");
        let st = e.state().unwrap().unwrap();
        assert_eq!(st.plan, vec!["write it".to_string(), "run it".to_string()]);
        assert_eq!(st.waiting, Waiting::Ok { what: "http post to https://x".into(), why: e.open_job().unwrap().unwrap().pending_reason });

        // A job left mid-work: run one to completion, then rewind the saved record to Working
        // with its steps (the same rewind Task 4's resume test uses).
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "state-steps");
        e.handle("make p").unwrap();
        let mut job = e.store.last_undoable_job().unwrap().unwrap();
        job.state = State::Working; job.steps.truncate(1); job.outcome_text.clear();
        e.store.save_job(&job).unwrap();
        let st = e.state().unwrap().unwrap();
        assert_eq!(st.steps.len(), 1);
        assert_eq!((st.steps[0].plan_step, st.steps[0].text.as_str(), st.steps[0].ok), (1, "wrote BLUEPRINT.md", true));
        assert_eq!(st.waiting, Waiting::None);
    }
```
- [ ] **Step 2: Run to fail** — compile error.

- [ ] **Step 3: Implement**

```rust
    /// The open job as a front door needs to draw it (1d §2.2).
    pub fn state(&self) -> Result<Option<JobState>, EngineError> {
        Ok(self.open_job()?.map(|job| JobState {
            id: job.id.clone(),
            name: display_name(&job).to_string(),
            housekeeping: job.housekeeping,
            understood: job.understood.clone(),
            plan: job.plan.clone(),
            steps: job.steps.iter().map(|s| StepView { plan_step: s.plan_step, text: crate::event::describe(&s.action), ok: s.ok }).collect(),
            waiting: match (job.state, &job.pending_action) {
                (State::WaitingAnswer, _) => Waiting::Answer { questions: job.pending_questions.clone() },
                (State::WaitingApproval, Some((_, a))) => Waiting::Ok { what: crate::event::describe(a), why: job.pending_reason.clone() },
                _ => Waiting::None,
            },
        }))
    }
```

- [ ] **Step 4: Run** — pass. **Step 5: Commit** — `git commit -am "feat(core): state() snapshot of the open job"`

---

### Task 7: The service

**Files:**
- Create: `runtime/core/src/service.rs`, `runtime/core/src/bin/ai-os-engine.rs`, `runtime/core/tests/service.rs`
- Modify: `runtime/core/src/lib.rs` (+ `pub mod service;`), `runtime/core/Cargo.toml` (`[[bin]] name = "ai-os-engine" path = "src/bin/ai-os-engine.rs"`)

**Interfaces:**
- Consumes: `Engine::{with_sink, handle_events, resume, state, stop_flag}`, `engine::is_stop`, `aios_proto::{Event, Request, JobState, Waiting, StepView}`.
- Produces: `service::run<M: Model + 'static>(listener: UnixListener, make: Box<dyn FnOnce(Box<dyn FnMut(&Event)>) -> Engine<M> + Send>) -> !` (the engine is built on the engine thread by `make`, which receives the broadcast sink; no `Send` on `M` or the engine is needed, the path check confirmed the bounds); `service::bind(path: &Path) -> io::Result<UnixListener>` refuses a live socket, removes a stale file, sets mode 0600; `service::socket_path() -> PathBuf`.

- [ ] **Step 1: The failing service test** — `runtime/core/tests/service.rs`

```rust
//! Two clients on a temp socket, a scripted model, no display (1d design §7 item 2).
use aios_core::engine::Engine;
use aios_core::model::FakeModel;
use aios_core::moves::Move;
use aios_core::service;
use aios_core::store::Store;
use aios_core::testing_pub::{scripted_workers, Recorder};
use aios_proto::{Client, Event, Waiting};
use executor::action::Action;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ai-os-svc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
}

/// A model whose moves can be refilled from the test and that blocks on a gate so the test can
/// act *while* the engine is inside a job.
struct GatedModel { inner: Mutex<FakeModel>, gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>> }
impl aios_core::model::Model for GatedModel {
    fn next_move(&self, p: &aios_core::model::Prompt) -> Result<Move, aios_core::model::ModelError> {
        if let Some(rx) = self.gate.lock().unwrap().as_ref() { let _ = rx.recv_timeout(Duration::from_secs(5)); }
        self.inner.lock().unwrap().next_move(p)
    }
}

fn start(dir: &PathBuf, moves: Vec<Move>, gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>) -> PathBuf {
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    let root = dir.clone();
    std::thread::spawn(move || {
        service::run(listener, Box::new(move |sink| {
            let rec = Recorder::default();
            std::fs::create_dir_all(root.join("hk")).unwrap();
            Engine::new(Store::open_in_memory().unwrap(), GatedModel { inner: Mutex::new(FakeModel::new(moves)), gate }, root.clone(), None, scripted_workers(&rec), root.join("hk"), root.join("snaps")).with_sink(sink)
        }))
    });
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

fn until(c: &mut Client, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut got = vec![];
    while let Some(e) = c.next_event() { let stop = pred(&e); got.push(e); if stop { return got; } }
    panic!("connection closed before the event: {got:?}");
}

fn write(p: &str) -> Action { Action::WriteFile { path: p.into(), contents: "x".into() } }
fn job() -> Vec<Move> { vec![
    Move::Start { project: "p".into(), new_project: true, description: "d".into(), goal: "g".into(), creative: true, understood: "Starting p".into(), remember: None },
    Move::Plan { steps: vec!["write".into()] },
    Move::Act { step: 1, action: write("BLUEPRINT.md") },
    Move::Done { summary: "finished".into(), check: Action::RunCommand { argv: vec!["true".into()] } },
] }

#[test]
fn both_clients_see_every_event_in_order_with_contiguous_seq() {
    let dir = temp("two");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut a = Client::connect(&sock).unwrap();
    let mut b = Client::connect(&sock).unwrap();
    // `connect` returns before the service has registered the client: a hello/state round trip
    // on each proves both are registered before anything is broadcast.
    a.hello().unwrap(); assert!(matches!(a.next_event(), Some(Event::State { .. })));
    b.hello().unwrap(); assert!(matches!(b.next_event(), Some(Event::State { .. })));
    a.say("make p").unwrap();
    let ea = until(&mut a, |e| matches!(e, Event::Done { .. }));
    let eb = until(&mut b, |e| matches!(e, Event::Done { .. }));
    assert_eq!(ea, eb);
    assert!(matches!(&ea[0], Event::You { text } if text == "make p"));
    assert!(matches!(&ea[1], Event::Understood { .. }));
}

#[test]
fn seq_is_contiguous_on_the_wire() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("seq");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    s.write_all(b"{\"say\":\"make p\"}\n").unwrap();
    let mut seqs = vec![];
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v.as_object().unwrap().keys().next().unwrap(), "kind", "kind is the first key: {line}");
        seqs.push(v["seq"].as_u64().unwrap());
        if v["kind"] == "done" { break; }
    }
    for w in seqs.windows(2) { assert_eq!(w[1], w[0] + 1, "{seqs:?}"); }
}

#[test]
fn hello_answers_state_and_busy_is_answered_during_a_job() {
    let dir = temp("busy");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(grx)));
    let sock = start(&dir, job(), gate.clone());
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap();
    assert_eq!(a.next_event(), Some(Event::State { job: None }));
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); // Start
    gtx.send(()).unwrap(); // Plan
    until(&mut a, |e| matches!(e, Event::Plan { .. }));
    // The engine is now blocked inside the job (waiting for the gate before Act).
    let mut b = Client::connect(&sock).unwrap();
    b.hello().unwrap();
    let Some(Event::State { job: Some(st) }) = b.next_event() else { panic!() };
    assert_eq!((st.name.as_str(), st.plan.len(), st.waiting), ("p", 1, Waiting::None));
    b.say("use python").unwrap();
    assert!(matches!(b.next_event(), Some(Event::You { .. })));
    assert!(matches!(b.next_event(), Some(Event::Busy { text, .. }) if text.contains("working on p")));
    gtx.send(()).unwrap(); gtx.send(()).unwrap(); // Act, Done
    until(&mut a, |e| matches!(e, Event::Done { .. }));
    b.hello().unwrap();
    let got = until(&mut b, |e| matches!(e, Event::State { .. }));
    assert_eq!(got.last(), Some(&Event::State { job: None }));
}

#[test]
fn stop_during_a_job_lands_and_a_dropped_client_changes_nothing() {
    let dir = temp("stop");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(grx)));
    let sock = start(&dir, job(), gate);
    let mut a = Client::connect(&sock).unwrap();
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); gtx.send(()).unwrap();
    until(&mut a, |e| matches!(e, Event::Plan { .. }));
    let mut b = Client::connect(&sock).unwrap();
    b.say("stop").unwrap();
    drop(b);
    gtx.send(()).unwrap(); // lets the model answer the Act; the flag is checked before the next turn
    gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Stopped { .. } | Event::Done { .. }));
    assert!(matches!(got.last().unwrap(), Event::Stopped { .. }), "{got:?}");
}

#[test]
fn a_bad_line_gets_error_and_nothing_else() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("bad");
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.write_all(b"not json\n").unwrap();
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["kind"], "error");
    assert!(v.get("seq").is_none(), "seq is on broadcasts only, so every client's run of seq is contiguous");
}

#[test]
fn stop_typed_right_after_a_request_still_stops_it() {
    // The stop arrives before the engine has even picked the request up: it must be queued AND
    // arm the flag, or it would sit behind the whole job.
    let dir = temp("early-stop");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, job(), Arc::new(Mutex::new(Some(grx))));
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap(); a.next_event();
    a.say("make p").unwrap();
    a.say("stop").unwrap();
    gtx.send(()).unwrap(); gtx.send(()).unwrap(); gtx.send(()).unwrap(); gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Stopped { .. } | Event::Done { .. } | Event::Said { .. }));
    assert!(matches!(got.last().unwrap(), Event::Stopped { .. }), "{got:?}");
}

#[test]
fn a_message_while_only_a_chat_reply_is_in_progress_is_queued_not_busy() {
    let dir = temp("chat-queue");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, vec![Move::Reply { text: "one".into(), remember: None }, Move::Reply { text: "two".into(), remember: None }], Arc::new(Mutex::new(Some(grx))));
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap(); a.next_event();
    a.say("hi").unwrap();
    a.say("hi again").unwrap(); // the engine is inside the first reply (gated) — no job, so no busy
    gtx.send(()).unwrap(); gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Said { text } if text == "two"));
    assert!(!got.iter().any(|e| matches!(e, Event::Busy { .. })), "{got:?}");
}
```
`testing.rs` is `#[cfg(test)]` and invisible to integration tests: change `lib.rs` to `pub mod testing;` unconditionally (`FakeModel` and everything it uses are already public and ungated; the fakes ship inside the binaries, unused, which is harmless). The test's `use` line is then `use aios_core::testing::{scripted_workers, Recorder};` — replace the `testing_pub` line above with that.

- [ ] **Step 2: Run to fail** — `cargo test -p aios-core --test service` → compile error.

- [ ] **Step 3: `service.rs`**

```rust
//! The engine as a service (1d design §3): one engine thread, one thread per client, JSON lines.
use crate::engine::{is_stop, Engine};
use crate::model::Model;
use aios_proto::{Event, JobState, Request, StepView, Waiting};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

enum Command { Say(String) }

/// Everything the client threads need without touching the engine.
struct Shared {
    clients: Mutex<Vec<(u64, Sender<String>)>>,
    seq: AtomicU64,
    /// True while the engine is inside `handle_events`/`resume` — a job is being worked.
    running: AtomicBool,
    /// The open job as the events described it: `hello` is answered from here, at once, even
    /// mid-job (the engine's own `state()` is only reachable between calls).
    mirror: Mutex<Option<JobState>>,
    stop: Mutex<Option<Arc<AtomicBool>>>,
}

impl Shared {
    /// To every client, numbered: `seq` counts broadcasts only, so each client's run is
    /// contiguous and a gap means a dropped line. Answers to one client (`send_to`) carry no seq.
    fn broadcast(&self, ev: &Event) {
        let mut v = serde_json::to_value(ev).expect("event serialises");
        v["seq"] = serde_json::Value::from(self.seq.fetch_add(1, Ordering::SeqCst));
        let line = v.to_string();
        self.clients.lock().unwrap().retain(|(_, tx)| tx.send(line.clone()).is_ok());
    }
    fn send_to(&self, id: u64, ev: &Event) {
        let line = serde_json::to_string(ev).expect("event serialises");
        let mut clients = self.clients.lock().unwrap();
        if let Some(pos) = clients.iter().position(|(cid, _)| *cid == id) {
            if clients[pos].1.send(line).is_err() { clients.remove(pos); }
        }
    }
    fn update_mirror(&self, ev: &Event) {
        let mut m = self.mirror.lock().unwrap();
        match ev {
            Event::Understood { job_id, name, text, housekeeping } => *m = Some(JobState { id: job_id.clone(), name: name.clone(), housekeeping: *housekeeping, understood: text.clone(), plan: vec![], steps: vec![], waiting: Waiting::None }),
            Event::Plan { steps, .. } => if let Some(j) = m.as_mut() { j.plan = steps.clone(); j.waiting = Waiting::None; },
            Event::Step { plan_step, text, ok, .. } => if let Some(j) = m.as_mut() { j.steps.push(StepView { plan_step: *plan_step, text: text.clone(), ok: *ok }); j.waiting = Waiting::None; },
            Event::NeedsAnswer { questions, .. } => if let Some(j) = m.as_mut() { j.waiting = Waiting::Answer { questions: questions.clone() }; },
            Event::NeedsOk { what, why, .. } => if let Some(j) = m.as_mut() { j.waiting = Waiting::Ok { what: what.clone(), why: why.clone() }; },
            Event::Done { .. } | Event::Failed { .. } | Event::Stopped { .. } => *m = None,
            _ => {}
        }
    }
}

/// Listen on the socket, private to this user. A socket something still answers on is a live
/// service: refuse rather than unlink it and run two engines on one database. A stale file
/// (nothing answers) is removed.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("an engine is already listening on {}", path.display())));
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Serve forever. `make` builds the engine on the engine thread and receives the broadcast sink.
pub fn run<M: Model + 'static>(listener: UnixListener, make: Box<dyn FnOnce(Box<dyn FnMut(&Event)>) -> Engine<M> + Send>) -> ! {
    let shared = Arc::new(Shared { clients: Mutex::new(vec![]), seq: AtomicU64::new(1), running: AtomicBool::new(false), mirror: Mutex::new(None), stop: Mutex::new(None) });
    let (tx, rx) = channel::<Command>();
    let sh = shared.clone();
    std::thread::spawn(move || {
        let sink_sh = sh.clone();
        let mut engine = make(Box::new(move |ev| { sink_sh.update_mirror(ev); sink_sh.broadcast(ev); }));
        *sh.stop.lock().unwrap() = Some(engine.stop_flag());
        if let Ok(Some(st)) = engine.state() { *sh.mirror.lock().unwrap() = Some(st); }
        sh.running.store(true, Ordering::SeqCst);
        if let Err(e) = engine.resume() { eprintln!("engine: resume failed: {e}"); }
        sh.running.store(false, Ordering::SeqCst);
        for cmd in rx {
            let Command::Say(text) = cmd;
            sh.running.store(true, Ordering::SeqCst);
            let r = engine.handle_events(&text);
            sh.running.store(false, Ordering::SeqCst);
            // A stop that arrived after the job had already ended on its own: say so, do not
            // leave the flag armed for the next job.
            if engine.stop_flag().swap(false, Ordering::SeqCst) {
                sh.broadcast(&Event::Said { text: "Nothing is running now.".into() });
            }
            if let Err(e) = r { sh.broadcast(&Event::Said { text: format!("(something went wrong: {e})") }); }
        }
    });
    let mut next_id = 1u64;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let id = next_id; next_id += 1;
        let (ctx, crx) = channel::<String>();
        shared.clients.lock().unwrap().push((id, ctx));
        // Writer: everything broadcast to this client, in order.
        let mut w = match stream.try_clone() { Ok(s) => s, Err(_) => continue };
        std::thread::spawn(move || { for line in crx { if w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).is_err() { break; } } });
        // Reader: the client's requests.
        let sh = shared.clone();
        let tx = tx.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Request>(&line) {
                    Ok(Request::Hello(_)) => { let st = sh.mirror.lock().unwrap().clone(); sh.send_to(id, &Event::State { job: st }); }
                    Ok(Request::Say(text)) => {
                        sh.broadcast(&Event::You { text: text.clone() });
                        // Stop: arm the flag (lands between steps if a job is running) AND queue
                        // it (a stop typed before the engine picked the request up must not sit
                        // behind the whole job; an idle engine answers it as today). The
                        // engine thread clears a flag nothing consumed.
                        if is_stop(&text) {
                            if let Some(f) = sh.stop.lock().unwrap().as_ref() { f.store(true, Ordering::SeqCst); }
                            if tx.send(Command::Say(text)).is_err() { break; }
                            continue;
                        }
                        // Busy only while a JOB is being worked (§3.3): a chat reply in progress
                        // just queues the next message behind it.
                        let job = sh.mirror.lock().unwrap().as_ref().map(|j| (j.id.clone(), j.name.clone()));
                        match job {
                            Some((job_id, name)) if sh.running.load(Ordering::SeqCst) => {
                                sh.send_to(id, &Event::Busy { job_id, text: format!("I'm working on {name}. Say stop if you want me to change course.") });
                            }
                            _ => if tx.send(Command::Say(text)).is_err() { break; },
                        }
                    }
                    Err(_) => sh.send_to(id, &Event::Error { text: "could not read that message".into() }),
                }
            }
            sh.clients.lock().unwrap().retain(|(cid, _)| *cid != id);
        });
    }
    unreachable!("listener.incoming() never ends")
}
```
`Event::You` is broadcast before the busy check so the order is You then Busy (the test asserts it). A queued `stop` that reaches an idle engine goes through `handle_events` → the front door → the model; that is today's behaviour for "stop" with no job and is acceptable (the flag was already cleared by the post-check). `Request` derives `Deserialize` with `#[serde(rename = "say")]` on a tuple variant: `{"say":"…"}` is the externally-tagged form serde produces for enums, which is exactly the wire format. Verify with a unit test in `proto` if unsure: `serde_json::from_str::<Request>(r#"{"say":"hi"}"#)`.

`src/bin/ai-os-engine.rs`:
```rust
use aios_core::engine::{Engine, HOUSEKEEPING_DIR};
use aios_core::model::OllamaModel;
use aios_core::service;
use aios_core::store::Store;
use executor::admin::AdminWorker;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

fn main() {
    let db = std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into());
    let model = std::env::var("AI_OS_MODEL").unwrap_or_else(|_| "qwen3.5:9b".into());
    let root = PathBuf::from(std::env::var("AI_OS_PROJECTS").unwrap_or_else(|_| "/data/projects".into()));
    let sock = aios_core::service::socket_path();
    let listener = service::bind(&sock).unwrap_or_else(|e| { eprintln!("cannot listen on {}: {e}", sock.display()); std::process::exit(1) });
    eprintln!("ai-os-engine listening on {}", sock.display());
    service::run(listener, Box::new(move |sink| {
        let store = Store::open(&db).expect("open store");
        Engine::new(store, OllamaModel::local(&model), root, Some(db),
            Box::new(|ws| (
                Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
                Box::new(AdminWorker) as Box<dyn Worker>,
            )),
            PathBuf::from(HOUSEKEEPING_DIR),
            PathBuf::from("/data/snapshots")).with_sink(sink)
    }))
}
```
Add to `service.rs`:
```rust
/// `$AI_OS_SOCKET_DIR/ai-os.sock` when set (tests, a second engine on purpose), else
/// `$XDG_RUNTIME_DIR/ai-os.sock` (a systemd user service and every shell in the distro have
/// it), else `/run/user/<uid>/ai-os.sock`.
pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var("AI_OS_SOCKET_DIR").ok()
        .or_else(|| std::env::var("XDG_RUNTIME_DIR").ok())
        .unwrap_or_else(|| {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(1000);
            format!("/run/user/{uid}")
        });
    Path::new(&dir).join("ai-os.sock")
}
```

- [ ] **Step 4: Run** — `cargo test -p aios-core --test service` → 7 pass; `cargo test -p aios-core` all pass; `cargo build --bin ai-os-engine` builds.

- [ ] **Step 5: Commit** — `git add runtime/core && git commit -m "feat(core): the engine service on a private socket, JSON lines, busy and stop"`

---

### Task 8: The thin terminal client and the workshop setup

**Files:**
- Modify: `runtime/core/src/main.rs`
- Create: `trial/setup-rail.sh`, `trial/ai-os-engine.service`

**Interfaces:**
- Consumes: `aios_proto::Client`, `aios_core::event::lines`, `aios_core::service::socket_path`.

- [ ] **Step 1: `main.rs` becomes the client**

```rust
//! The builder's terminal front door: a thin client of ai-os-engine (1d design §3.5).
use aios_core::event::lines;
use aios_proto::Client;
use std::io::{BufRead, Write};

fn main() {
    let sock = aios_core::service::socket_path();
    let client = match Client::connect(&sock) {
        Ok(c) => c,
        Err(e) => { eprintln!("The AI OS service is not running ({}: {e}). Start it: systemctl --user start ai-os-engine", sock.display()); std::process::exit(1) }
    };
    // One connection, two halves: the reader prints on its own thread so it never blocks the prompt.
    let (mut reader, mut client) = client.split();
    client.hello().ok();
    std::thread::spawn(move || {
        while let Some(ev) = reader.next_event() {
            for l in lines(&ev) { println!("ai> {l}"); }
            print!("you> "); std::io::stdout().flush().ok();
        }
        println!("(the service closed the connection)");
        std::process::exit(0);
    });
    let stdin = std::io::stdin();
    print!("you> "); std::io::stdout().flush().ok();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let text = line.trim();
        if text.is_empty() { print!("you> "); std::io::stdout().flush().ok(); continue; }
        if client.say(text).is_err() { break; }
    }
}
```
(`You` echoes render to nothing in `lines`, so the terminal is not doubled. The `hello` goes out on the same connection the reader half listens on, so the `state` answer is seen.)

- [ ] **Step 2: The unit and the setup script**

`trial/ai-os-engine.service`:
```ini
[Unit]
Description=AI OS engine (workshop)

[Service]
ExecStart=/usr/local/bin/ai-os-engine
Environment=AI_OS_DB=/data/ai-os.db
Environment=AI_OS_MODEL=qwen3.5:9b
Environment=AI_OS_PROJECTS=/data/projects
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```
`trial/setup-rail.sh`:
```bash
#!/usr/bin/env bash
# Phase 1d workshop setup. Run as root inside the ai-os distro after a release build. Idempotent.
# WORKSHOP ONLY (parent spec §11): binaries are copied from the Windows-mounted repo; the product
# ships them from a package.
set -euo pipefail
repo=${AI_OS_REPO:-/mnt/c/Users/gdoum/Desktop/projects/ai-os}
apt-get install -y libgtk-4-dev
for b in ai-os-engine ai-os-chat ai-os-rail; do
  install -m 0755 -o root -g root "$repo/runtime/target/release/$b" /usr/local/bin/$b
done
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
install -m 0644 -o ai -g ai "$repo/trial/ai-os-engine.service" /home/ai/.config/systemd/user/ai-os-engine.service
sed -i 's/\r$//' /home/ai/.config/systemd/user/ai-os-engine.service
loginctl enable-linger ai   # already yes on the workshop; idempotent, needed on a fresh install
# ai's own service manager, addressed from root (systemd 259 supports -M user@).
systemctl --user -M ai@ daemon-reload
systemctl --user -M ai@ enable ai-os-engine.service
systemctl --user -M ai@ restart ai-os-engine.service
echo "rail ready"
```
`ai-os-rail` does not exist until Task 10: the `install` loop must print and skip a missing binary (`src="$repo/runtime/target/release/$b"; [ -f "$src" ] || { echo "skip $b (not built)"; continue; }`), never fail silently and never fail the script.

- [ ] **Step 3: Build and install in the distro, run the chat once**

```bash
wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo build --release -p aios-core"
wsl -d ai-os -u root -- bash -c "sed 's/\r$//' /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-rail.sh | bash"
wsl -d ai-os -u ai -- bash -lc "systemctl --user is-active ai-os-engine && ls -la /run/user/1000/ai-os.sock && printf 'what is a prime?\n' | timeout 60 ai-os-chat"
```
Expected: `active`, a 0600 socket owned by ai, and an `ai>` reply from the real model. If the unit fails, `journalctl --user -u ai-os-engine -n 30` as ai.

- [ ] **Step 4: Confirm `live_1c.rs` still compiles** — `cargo test -p aios-core --no-run`.

- [ ] **Step 5: Commit** — `git add runtime/core/src/main.rs trial/setup-rail.sh trial/ai-os-engine.service && git commit -m "feat: ai-os-chat is a thin client; workshop unit and setup for the engine service"`

---

### Task 9: The rail's card model

**Files:**
- Create: `runtime/rail/Cargo.toml`, `runtime/rail/src/lib.rs`, `runtime/rail/src/cards.rs`, `runtime/rail/tests/cards.rs`
- Modify: `runtime/Cargo.toml` (add `"rail"` to members)

**Interfaces:**
- Consumes: `aios_proto::{Event, JobState, Waiting, ChangedFile, FileKind}`.
- Produces: `aios_rail::cards::{Card, CardKind, Button, Cards}`; `Cards::apply(&mut self, &Event) -> Vec<Change>`; `Change::{Added(usize), Updated(usize), Line(String)}`.

- [ ] **Step 1: Failing test** — `runtime/rail/tests/cards.rs`

```rust
use aios_proto::*;
use aios_rail::cards::{Button, CardKind, Cards, Change};

fn j() -> String { "j1".into() }

#[test]
fn a_whole_job_becomes_the_right_cards() {
    let mut cards = Cards::default();
    assert_eq!(cards.apply(&Event::You { text: "make p".into() }), vec![Change::Added(0)]);
    assert!(matches!(cards.list[0].kind, CardKind::You));
    cards.apply(&Event::Understood { job_id: j(), name: "p".into(), text: "Starting p".into(), housekeeping: false });
    let b = 1;
    assert!(matches!(cards.list[b].kind, CardKind::Building { .. }));
    assert_eq!(cards.list[b].buttons, vec![Button { label: "Stop".into(), say: "stop".into() }]);
    assert_eq!(cards.apply(&Event::Plan { job_id: j(), steps: vec!["write".into(), "run".into()] }), vec![Change::Updated(b)]);
    cards.apply(&Event::Step { job_id: j(), plan_step: 1, text: "wrote a.py".into(), ok: true });
    cards.apply(&Event::Step { job_id: j(), plan_step: 2, text: "ran python3".into(), ok: false });
    cards.apply(&Event::Step { job_id: j(), plan_step: 2, text: "ran python3 a.py".into(), ok: true });
    let CardKind::Building { steps, .. } = &cards.list[b].kind else { panic!() };
    assert_eq!(steps.iter().map(|s| (s.text.as_str(), s.done, s.ok, s.detail.as_deref())).collect::<Vec<_>>(),
        vec![("write", true, true, Some("wrote a.py")), ("run", true, true, Some("ran python3 a.py"))]);

    cards.apply(&Event::NeedsOk { job_id: j(), what: "http post to x".into(), why: "network".into() });
    let ok = 2;
    assert!(matches!(&cards.list[ok].kind, CardKind::NeedsOk { what, why } if what == "http post to x" && why == "network"));
    assert_eq!(cards.list[ok].buttons, vec![Button { label: "Yes".into(), say: "yes".into() }, Button { label: "No".into(), say: "no".into() }]);
    cards.apply(&Event::Said { text: "it sends b".into() });
    cards.apply(&Event::NeedsOk { job_id: j(), what: "http post to x".into(), why: "network".into() });
    assert_eq!(cards.list.len(), 5, "said, then the OK asked again as a new card");

    let files = vec![ChangedFile { path: "/data/p/a.py".into(), kind: FileKind::Text, size: 3 }, ChangedFile { path: "/data/p/pic.png".into(), kind: FileKind::Image, size: 9 }];
    cards.apply(&Event::Done { job_id: j(), text: "finished".into(), check: Some("ran python3 a.py: ok".into()), files: files.clone() });
    let CardKind::Building { collapsed, .. } = &cards.list[b].kind else { panic!() };
    assert!(collapsed);
    let done = cards.list.last().unwrap();
    let CardKind::Done { text, check, files: f } = &done.kind else { panic!() };
    assert_eq!((text.as_str(), check.as_deref()), ("finished", Some("ran python3 a.py: ok")));
    assert_eq!(f, &files);
    assert_eq!(done.buttons, vec![Button { label: "Undo".into(), say: "undo".into() }]);
    assert_eq!(done.opens, vec!["/data/p/a.py".to_string(), "/data/p/pic.png".to_string()]);
    assert_eq!(done.thumbnails, vec!["/data/p/pic.png".to_string()]);

    cards.apply(&Event::Undone { job_id: j(), name: "p".into(), lines: vec![UndoLine { text: "put back a.py".into(), ok: true }, UndoLine { text: "could not".into(), ok: false }], notes: vec!["Not covered: x".into()] });
    let CardKind::Undone { lines, notes } = &cards.list.last().unwrap().kind else { panic!() };
    assert_eq!(lines.len(), 2); assert_eq!(notes, &vec!["Not covered: x".to_string()]);
}

#[test]
fn busy_is_a_line_not_a_card_and_state_rebuilds_a_job() {
    let mut cards = Cards::default();
    assert_eq!(cards.apply(&Event::Busy { job_id: j(), text: "working".into() }), vec![Change::Line("working".into())]);
    assert!(cards.list.is_empty());
    let st = JobState { id: j(), name: "p".into(), housekeeping: false, understood: "Starting p".into(), plan: vec!["a".into(), "b".into()],
        steps: vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }], waiting: Waiting::Answer { questions: vec!["which?".into()] } };
    cards.apply(&Event::State { job: Some(st) });
    assert_eq!(cards.list.len(), 2);
    let CardKind::Building { steps, .. } = &cards.list[0].kind else { panic!() };
    assert!(steps[0].done && !steps[1].done);
    assert!(matches!(&cards.list[1].kind, CardKind::NeedsAnswer { questions } if questions == &vec!["which?".to_string()]));
    assert_eq!(cards.apply(&Event::State { job: None }), vec![], "an empty state changes nothing");
}
```

- [ ] **Step 2: Crate files**

`runtime/rail/Cargo.toml`:
```toml
[package]
name = "aios-rail"
version = "0.0.1"
edition = "2021"

[lib]
name = "aios_rail"
path = "src/lib.rs"

[[bin]]
name = "ai-os-rail"
path = "src/main.rs"

[dependencies]
aios-proto = { path = "../proto" }
serde_json = { version = "1", features = ["preserve_order"] }
gtk4 = { version = "0.11", features = ["v4_18"] }
```
(`gtk4` 0.11 is the series that targets the distro's GTK 4.22 and needs Rust ≥ 1.92; the distro has 1.98. `glib` is re-exported as `gtk4::glib`. If `v4_18` is not a feature name of the resolved version, use the highest `v4_*` feature it offers at or below 4.22.)

`src/lib.rs`: `pub mod cards;`
`src/main.rs` for now: `fn main() { println!("ai-os-rail: window comes in Task 10"); }` (so the bin exists; Task 10 replaces it).

- [ ] **Step 3: Run to fail** — `cargo test -p aios-rail` → compile error. (GTK dev files must be present: run `trial/setup-rail.sh` from Task 8 first, it installs `libgtk-4-dev`.)

- [ ] **Step 4: `cards.rs`**

```rust
//! Cards from events (1d design §4.2). Pure data: the window renders it, the test reads it.
use aios_proto::{ChangedFile, Event, FileKind, JobState, Waiting};

#[derive(Debug, Clone, PartialEq)]
pub struct Button { pub label: String, pub say: String }

#[derive(Debug, Clone, PartialEq)]
pub struct StepLine { pub text: String, pub done: bool, pub ok: bool, pub detail: Option<String> }

#[derive(Debug, Clone, PartialEq)]
pub enum CardKind {
    You, Said,
    Building { name: String, understood: String, steps: Vec<StepLine>, collapsed: bool },
    NeedsAnswer { questions: Vec<String> },
    NeedsOk { what: String, why: String },
    Done { text: String, check: Option<String>, files: Vec<ChangedFile> },
    Failed { text: String, files: Vec<ChangedFile> },
    Stopped { text: String, files: Vec<ChangedFile> },
    Undone { lines: Vec<aios_proto::UndoLine>, notes: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub kind: CardKind,
    pub text: String,
    pub buttons: Vec<Button>,
    /// Paths with an Open button, in order. Always from the service, never from user text.
    pub opens: Vec<String>,
    pub thumbnails: Vec<String>,
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change { Added(usize), Updated(usize), Line(String) }

#[derive(Default)]
pub struct Cards { pub list: Vec<Card>, building: Option<usize> }

fn btn(label: &str, say: &str) -> Button { Button { label: label.into(), say: say.into() } }

fn result_card(kind: CardKind, text: &str, files: &[ChangedFile], job_id: &str) -> Card {
    Card {
        kind, text: text.into(), buttons: vec![btn("Undo", "undo")],
        opens: files.iter().map(|f| f.path.clone()).collect(),
        thumbnails: files.iter().filter(|f| f.kind == FileKind::Image).map(|f| f.path.clone()).collect(),
        job_id: Some(job_id.into()),
    }
}

impl Cards {
    fn push(&mut self, c: Card) -> Vec<Change> { self.list.push(c); vec![Change::Added(self.list.len() - 1)] }

    fn open_building(&mut self, job_id: &str, name: &str, understood: &str) -> usize {
        self.list.push(Card { kind: CardKind::Building { name: name.into(), understood: understood.into(), steps: vec![], collapsed: false }, text: understood.into(), buttons: vec![btn("Stop", "stop")], opens: vec![], thumbnails: vec![], job_id: Some(job_id.into()) });
        let i = self.list.len() - 1; self.building = Some(i); i
    }

    fn close_building(&mut self) -> Vec<Change> {
        let Some(i) = self.building.take() else { return vec![] };
        if let CardKind::Building { collapsed, .. } = &mut self.list[i].kind { *collapsed = true; }
        self.list[i].buttons.clear();
        vec![Change::Updated(i)]
    }

    pub fn apply(&mut self, ev: &Event) -> Vec<Change> {
        match ev {
            Event::You { text } => self.push(Card { kind: CardKind::You, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Said { text } => self.push(Card { kind: CardKind::Said, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Understood { job_id, name, text, .. } => { let i = self.open_building(job_id, name, text); vec![Change::Added(i)] }
            Event::Plan { steps, .. } => match self.building {
                Some(i) => { if let CardKind::Building { steps: s, .. } = &mut self.list[i].kind { *s = steps.iter().map(|t| StepLine { text: t.clone(), done: false, ok: false, detail: None }).collect(); } vec![Change::Updated(i)] }
                None => vec![],
            },
            Event::Step { plan_step, text, ok, .. } => match self.building {
                Some(i) => {
                    if let CardKind::Building { steps, .. } = &mut self.list[i].kind {
                        if let Some(s) = steps.get_mut(plan_step.saturating_sub(1)) { s.done = true; s.ok = *ok; s.detail = Some(text.clone()); }
                    }
                    vec![Change::Updated(i)]
                }
                None => vec![],
            },
            Event::NeedsAnswer { job_id, questions } => self.push(Card { kind: CardKind::NeedsAnswer { questions: questions.clone() }, text: questions.join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::NeedsOk { job_id, what, why } => self.push(Card { kind: CardKind::NeedsOk { what: what.clone(), why: why.clone() }, text: format!("{what}\n{why}"), buttons: vec![btn("Yes", "yes"), btn("No", "no")], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::Done { job_id, text, check, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Done { text: text.clone(), check: check.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Failed { job_id, text, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Failed { text: text.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Stopped { job_id, text, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Stopped { text: text.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Undone { job_id, lines, notes, .. } => self.push(Card { kind: CardKind::Undone { lines: lines.clone(), notes: notes.clone() }, text: lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::Busy { text, .. } | Event::Error { text } => vec![Change::Line(text.clone())],
            Event::State { job: None } => vec![],
            Event::State { job: Some(st) } => self.rebuild(st),
        }
    }

    /// A reopened rail: the open job as one Building card (steps ticked so far) plus its
    /// waiting card, if any. Earlier finished jobs are not replayed (1d §4.2).
    fn rebuild(&mut self, st: &JobState) -> Vec<Change> {
        let i = self.open_building(&st.id, &st.name, &st.understood);
        if let CardKind::Building { steps, .. } = &mut self.list[i].kind {
            *steps = st.plan.iter().map(|t| StepLine { text: t.clone(), done: false, ok: false, detail: None }).collect();
            for s in &st.steps { if let Some(l) = steps.get_mut(s.plan_step.saturating_sub(1)) { l.done = true; l.ok = s.ok; l.detail = Some(s.text.clone()); } }
        }
        let mut ch = vec![Change::Added(i)];
        match &st.waiting {
            Waiting::None => {}
            Waiting::Answer { questions } => ch.extend(self.apply(&Event::NeedsAnswer { job_id: st.id.clone(), questions: questions.clone() })),
            Waiting::Ok { what, why } => ch.extend(self.apply(&Event::NeedsOk { job_id: st.id.clone(), what: what.clone(), why: why.clone() })),
        }
        ch
    }
}
```

- [ ] **Step 5: Run** — `cargo test -p aios-rail` → 2 pass. **Step 6: Commit** — `git add runtime/Cargo.toml runtime/rail && git commit -m "feat(rail): the card model, built from events, no display needed"`

---

### Task 10: The rail window

**Files:**
- Modify: `runtime/rail/src/main.rs`
- Create: `docs/superpowers/findings/phase1d-rail.png` (by the screenshot step)

**Interfaces:**
- Consumes: `aios_rail::cards::*`, `aios_proto::Client`, `aios_core::service::socket_path` — NOT `aios_core` (it would pull sqlite into the rail): compute the socket path the same way inline (`$XDG_RUNTIME_DIR/ai-os.sock`).

- [ ] **Step 1: `main.rs`**

```rust
//! The rail (1d design §4): a tall window of cards and a text box; a client of ai-os-engine.
use aios_proto::{Client, Event};
use aios_rail::cards::{Card, CardKind, Cards, Change};
use gtk4 as gtk;
use gtk::prelude::*;
use gtk::glib;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use std::time::Duration;

fn socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/run/user/1000".into());
    PathBuf::from(dir).join("ai-os.sock")
}

enum FromNet { Event(Event), Down, Up }

/// The socket on its own thread: reconnects every 3 s; every event goes to a plain std channel
/// that the GTK loop drains on a timer (gtk4-rs 0.11 has no glib channel; std::mpsc + a 50 ms
/// `timeout_add_local` needs no extra crate).
fn net_thread(to_ui: Sender<FromNet>) -> Sender<String> {
    let (say_tx, say_rx) = channel::<String>();
    std::thread::spawn(move || loop {
        match Client::connect(&socket_path()) {
            Ok(client) => {
                let (mut reader, mut writer) = client.split();
                let _ = to_ui.send(FromNet::Up);
                let _ = writer.hello();
                let ui = to_ui.clone();
                let pump = std::thread::spawn(move || { while let Some(e) = reader.next_event() { if ui.send(FromNet::Event(e)).is_err() { break } } });
                while !pump.is_finished() {
                    match say_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(t) => { if writer.say(&t).is_err() { break } }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return,
                    }
                }
                let _ = to_ui.send(FromNet::Down);
                std::thread::sleep(Duration::from_secs(3));
            }
            Err(_) => { let _ = to_ui.send(FromNet::Down); std::thread::sleep(Duration::from_secs(3)); }
        }
    });
    say_tx
}

fn open_path(path: &str) {
    // Only paths the service sent reach here (cards.rs builds `opens` from events alone).
    let _ = std::process::Command::new("xdg-open").arg(path).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn();
}

fn render(card: &Card, say: &Sender<String>) -> gtk::Widget {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
    b.add_css_class("card");
    let title = |t: &str| { let l = gtk::Label::new(Some(t)); l.add_css_class("title"); l.set_xalign(0.0); l.set_wrap(true); l };
    let text = |t: &str| { let l = gtk::Label::new(Some(t)); l.set_xalign(0.0); l.set_wrap(true); l.set_selectable(true); l };
    match &card.kind {
        CardKind::You => { b.add_css_class("you"); b.append(&text(&card.text)); }
        CardKind::Said => { b.add_css_class("said"); b.append(&text(&card.text)); }
        CardKind::Building { name, understood, steps, collapsed } => {
            b.add_css_class("building");
            b.append(&title(&format!("Building: {name}")));
            if !collapsed {
                b.append(&text(understood));
                for s in steps {
                    let mark = if !s.done { "☐" } else if s.ok { "✓" } else { "✗" };
                    b.append(&text(&format!("{mark} {}", s.text)));
                    if let Some(d) = &s.detail { let l = text(d); l.add_css_class("dim"); b.append(&l); }
                }
            }
        }
        CardKind::NeedsAnswer { questions } => { b.add_css_class("ask"); b.append(&title("Needs your answer")); for q in questions { b.append(&text(q)); } }
        CardKind::NeedsOk { what, why } => { b.add_css_class("ok"); b.append(&title("Needs your OK")); b.append(&text(what)); let l = text(why); l.add_css_class("dim"); b.append(&l); }
        CardKind::Done { text: t, check, files } => {
            b.add_css_class("done"); b.append(&title("Done")); b.append(&text(t));
            if let Some(c) = check { let l = text(&format!("check: {c}")); l.add_css_class("dim"); b.append(&l); }
            for f in files {
                if card.thumbnails.contains(&f.path) { let p = gtk::Picture::for_filename(&f.path); p.set_size_request(-1, 200); p.set_can_shrink(true); b.append(&p); }
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
                let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string();
                let l = text(&name); l.set_hexpand(true); row.append(&l);
                let open = gtk::Button::with_label("Open"); let path = f.path.clone(); open.connect_clicked(move |_| open_path(&path)); row.append(&open);
                b.append(&row);
            }
        }
        CardKind::Failed { text: t, files } | CardKind::Stopped { text: t, files } => {
            b.add_css_class("failed"); b.append(&title(if matches!(card.kind, CardKind::Failed { .. }) { "Could not finish" } else { "Stopped" })); b.append(&text(t));
            for f in files { let row = gtk::Box::new(gtk::Orientation::Horizontal, 6); let name = f.path.rsplit('/').next().unwrap_or(&f.path).to_string(); let l = text(&name); l.set_hexpand(true); row.append(&l); let open = gtk::Button::with_label("Open"); let path = f.path.clone(); open.connect_clicked(move |_| open_path(&path)); row.append(&open); b.append(&row); }
        }
        CardKind::Undone { lines, notes } => {
            b.add_css_class("undone"); b.append(&title("Undone"));
            for l in lines { b.append(&text(&format!("{} {}", if l.ok { "✓" } else { "✗" }, l.text))); }
            for n in notes { let l = text(n); l.add_css_class("dim"); b.append(&l); }
        }
    }
    if !card.buttons.is_empty() {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        for btn in &card.buttons { let w = gtk::Button::with_label(&btn.label); let s = say.clone(); let t = btn.say.clone(); w.connect_clicked(move |_| { let _ = s.send(t.clone()); }); row.append(&w); }
        b.append(&row);
    }
    b.upcast()
}
```
The match over `CardKind` has no `_` arm: a new card kind is a compile error here, not a blank card.

```rust
const CSS: &str = "
.card { padding: 8px 10px; margin: 4px 8px; border-radius: 8px; background: alpha(@theme_fg_color, 0.06); }
.you { background: alpha(@theme_selected_bg_color, 0.25); margin-left: 40px; }
.said { margin-right: 40px; }
.building { border-left: 3px solid @theme_selected_bg_color; }
.ok { border-left: 3px solid #e0a020; }
.ask { border-left: 3px solid #4090e0; }
.done { border-left: 3px solid #40b060; }
.failed { border-left: 3px solid #d04040; }
.title { font-weight: bold; }
.dim { opacity: 0.7; font-size: 90%; }
.status { opacity: 0.6; font-style: italic; margin: 2px 12px; }
";

fn main() {
    let app = gtk::Application::builder().application_id("org.aios.Rail").build();
    app.connect_activate(|app| {
        let css = gtk::CssProvider::new(); css.load_from_data(CSS);
        gtk::style_context_add_provider_for_display(&gtk::gdk::Display::default().unwrap(), &css, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        let win = gtk::ApplicationWindow::builder().application(app).title("AI OS").default_width(420).default_height(900).build();
        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let scroll = gtk::ScrolledWindow::builder().vexpand(true).child(&column).build();
        let status = gtk::Label::new(Some("Connecting to the AI OS service…")); status.add_css_class("status"); status.set_xalign(0.0);
        let entry = gtk::Entry::builder().placeholder_text("Tell the AI what you want…").margin_start(8).margin_end(8).margin_bottom(8).build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 4);
        root.append(&scroll); root.append(&status); root.append(&entry);
        win.set_child(Some(&root));

        let (to_ui, from_net) = channel::<FromNet>();
        let say = net_thread(to_ui);
        let cards = Rc::new(RefCell::new(Cards::default()));
        let widgets: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();

        let s = say.clone();
        entry.connect_activate(move |e| { let t = e.text().trim().to_string(); if !t.is_empty() { let _ = s.send(t); e.set_text(""); } });

        let (cards2, widgets2, column2, status2, scroll2, say2) = (cards.clone(), widgets.clone(), column.clone(), status.clone(), scroll.clone(), say.clone());
        glib::timeout_add_local(Duration::from_millis(50), move || {
            while let Ok(msg) = from_net.try_recv() {
                match msg {
                    FromNet::Up => status2.set_text(""),
                    FromNet::Down => status2.set_text("The AI OS service is not running — retrying…"),
                    FromNet::Event(ev) => {
                        // `apply` on its own line: the RefMut must end before `render` borrows.
                        let changes = cards2.borrow_mut().apply(&ev);
                        for ch in changes {
                            match ch {
                                Change::Added(i) => { let w = render(&cards2.borrow().list[i], &say2); column2.append(&w); widgets2.borrow_mut().push(w); }
                                Change::Updated(i) => { let old = widgets2.borrow()[i].clone(); let w = render(&cards2.borrow().list[i], &say2); column2.insert_child_after(&w, Some(&old)); column2.remove(&old); widgets2.borrow_mut()[i] = w; }
                                Change::Line(t) => status2.set_text(&t),
                            }
                        }
                        let adj = scroll2.vadjustment(); adj.set_value(adj.upper());
                    }
                }
            }
            glib::ControlFlow::Continue
        });
        entry.grab_focus();
        win.present();
    });
    app.run();
}
```
Keep the shape: one net thread, events delivered on the main loop by the 50 ms drain, one widget per card, re-rendered on `Updated`. Never block the main loop on the socket. `use std::sync::mpsc::{channel, Sender};` covers both channels; drop unused imports if the compiler says so, never the borrow-scoping line.

- [ ] **Step 2: Build in the distro and install**

```bash
wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo build --release -p aios-rail 2>&1 | tail -5"
wsl -d ai-os -u root -- bash -c "sed 's/\r$//' /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-rail.sh | bash"
```
The first GTK build compiles the `gtk4`/`gio`/`glib` bindings (several minutes). Build under `runtime/target` as everything else.

- [ ] **Step 3: Launch from Windows and give it a real job**

From the Windows side (PowerShell tool, background): `wsl -d ai-os ai-os-rail`. Then, from a second terminal, `wsl -d ai-os -u ai -- bash -lc "printf 'make a project called rail-demo with a python script that prints the first five primes, decide everything yourself\n' | timeout 300 ai-os-chat"` so the job runs while the rail shows it (the rail is a second client and sees the same events). Watch the Building card tick. Then press Open on `primes.py` (a GNOME text editor window appears through WSLg) and Undo.

- [ ] **Step 4: The screenshot (done by the lead in this session, not by the task's builder)**

The rail window is on the Windows desktop; capturing it needs the Windows-side screenshot tool (`mcp__computer-use__screenshot`), which asks him to grant access to that window at the moment of capture — he must be present. The builder of this task finishes at Step 3 and reports "window up, job ran, Open opened <app>, Undo reported"; the lead then takes the screenshot with him, showing a Building card mid-tick or a Done card with Open and Undo, plus a Needs-your-OK if a second job "write the word hello into /etc/ai-os-rail-test" is given (the `/etc` write asks). Saved as `docs/superpowers/findings/phase1d-rail.png` (binary in `.gitattributes` already). Until that image exists the rail is not reported as verified (his rule).

- [ ] **Step 5: Commit** — `git add runtime/rail docs/superpowers/findings/phase1d-rail.png && git commit -m "feat(rail): the GTK4 rail window on the WSLg display, with the first screenshot"`

---

### Task 11: Live acceptance through the service, and the results

**Files:**
- Create: `runtime/core/tests/live_1d.rs`
- Modify: `docs/superpowers/specs/2026-09-16-phase1d-rail-design.md` (§7 Results block), `docs/superpowers/specs/2026-09-15-ai-os-design.md` (a "Phase 1d status" block like 1c's; §5.1 display note per 1d §5)

**Interfaces:**
- Consumes: `service::{bind, run}`, `aios_proto::Client`, the real workers, `qwen3.5:9b`.

- [ ] **Step 1: The test**

```rust
// Phase 1d acceptance (1d design §7 item 4): the real model, both hands, through the service on
// a temp socket, no human. Inside the distro with Ollama and the wrapper installed:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_1d -- --nocapture
use aios_core::engine::Engine;
use aios_core::model::OllamaModel;
use aios_core::service;
use aios_core::store::Store;
use aios_proto::{Client, Event};
use executor::admin::AdminWorker;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DB: &str = "/data/ai-os-live-1d.db";
const PROJECT_DIR: &str = "/data/projects/rail-live";
const ETC_FILE: &str = "/etc/ai-os-live-1d.txt";

fn start_service() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ai-os-live-1d-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    std::thread::spawn(move || service::run(listener, Box::new(|sink| {
        Engine::new(Store::open(DB).unwrap(), OllamaModel::local("qwen3.5:9b"), PathBuf::from("/data/projects"), Some(DB.into()),
            Box::new(|ws| (Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>, Box::new(AdminWorker) as Box<dyn Worker>)),
            PathBuf::from("/data/housekeeping"), PathBuf::from("/data/snapshots")).with_sink(sink)
    })));
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

/// Say it, print every event, return once `stop` matches. Panics after `limit` seconds.
fn say_until(c: &mut Client, text: &str, limit: u64, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    println!("you> {text}");
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
    panic!("connection closed: {got:?}");
}

fn must_clear(path: &str, dir: bool) {
    let r = if dir { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = r { assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "could not clear {path}: {e}"); }
}

#[test]
fn live_1d() {
    if std::env::var("AI_OS_LIVE").is_err() { eprintln!("skipped: set AI_OS_LIVE=1"); return; }
    must_clear(DB, false);
    // The project folder is a btrfs subvolume: only the wrapper can remove it. A leftover fails
    // the run rather than silently reusing an old project.
    assert!(!std::path::Path::new(PROJECT_DIR).exists(), "clear {PROJECT_DIR} first (sudo /usr/local/libexec/ai-os-admin remove-dir {PROJECT_DIR})");
    let sock = start_service();
    let mut c = Client::connect(&sock).unwrap();
    c.hello().unwrap();
    assert_eq!(c.next_event(), Some(Event::State { job: None }), "a fresh db has no open job");

    // 1. A project job, decide-yourself, ends Done with files.
    let ev = say_until(&mut c, "make a new project called rail-live with a python script that prints the first five primes; decide everything yourself and do not ask", 400,
        |e| matches!(e, Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(&ev[0], Event::You { .. }));
    assert!(ev.iter().any(|e| matches!(e, Event::Understood { name, .. } if name == "rail-live")), "{ev:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Plan { .. })));
    assert!(ev.iter().filter(|e| matches!(e, Event::Step { .. })).count() >= 2);
    let Event::Done { files, .. } = ev.last().unwrap() else { panic!("the job did not finish: {:?}", ev.last()) };
    assert!(files.iter().any(|f| f.path.ends_with(".py")), "a python file among the changed files: {files:?}");
    assert!(files.iter().all(|f| f.path.starts_with(PROJECT_DIR)));

    // 2. A write under /etc needs the OK; a question there is answered and the OK asked again; yes runs it.
    must_clear(ETC_FILE, false);
    let ev = say_until(&mut c, &format!("in project rail-live, write the single word hello into the file {ETC_FILE}; decide everything yourself"), 400,
        |e| matches!(e, Event::NeedsOk { .. } | Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(ev.last().unwrap(), Event::NeedsOk { .. }), "expected an OK request for the /etc write: {:?}", ev.last());
    let ev = say_until(&mut c, "what exactly will you write there?", 120, |e| matches!(e, Event::NeedsOk { .. } | Event::Done { .. } | Event::Failed { .. }));
    assert!(ev.iter().any(|e| matches!(e, Event::Said { .. })), "the question was answered: {ev:?}");
    assert!(matches!(ev.last().unwrap(), Event::NeedsOk { .. }), "and the OK asked again: {ev:?}");
    let ev = say_until(&mut c, "yes", 400, |e| matches!(e, Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(ev.last().unwrap(), Event::Done { .. }), "{:?}", ev.last());
    assert_eq!(std::fs::read_to_string(ETC_FILE).unwrap().trim(), "hello");

    // 3. Undo puts the /etc file back (it did not exist) and reports it.
    let ev = say_until(&mut c, "undo", 120, |e| matches!(e, Event::Undone { .. } | Event::Said { .. }));
    let Event::Undone { lines, .. } = ev.last().unwrap() else { panic!("{ev:?}") };
    assert!(lines.iter().any(|l| l.ok), "{lines:?}");
    assert!(!std::path::Path::new(ETC_FILE).exists(), "the /etc file is gone again");

    // 4. Busy during a job, then stop lands.
    let mut b = Client::connect(&sock).unwrap();
    c.say("in project rail-live, add a script that prints the first hundred primes and a second one that prints the first hundred squares; decide everything yourself").unwrap();
    let t = Instant::now();
    loop { match c.next_event() { Some(Event::Plan { .. }) => break, Some(_) if t.elapsed() < Duration::from_secs(200) => {}, other => panic!("{other:?}") } }
    b.say("use bash instead").unwrap();
    let mut saw_busy = false;
    for _ in 0..50 { match b.next_event() { Some(Event::Busy { .. }) => { saw_busy = true; break } Some(Event::Done { .. }) => break, _ => {} } }
    assert!(saw_busy, "a message mid-job is answered with busy");
    b.say("stop").unwrap();
    let mut ended = None;
    let t = Instant::now();
    while let Some(e) = c.next_event() { if matches!(e, Event::Stopped { .. } | Event::Done { .. } | Event::Failed { .. }) { ended = Some(e); break; } assert!(t.elapsed().as_secs() < 300); }
    assert!(matches!(ended, Some(Event::Stopped { .. })), "stop landed within a step: {ended:?}");
}
```
Freshness: the db is cleared at the start and the project folder must not pre-exist, as in 1c. Before each run, clear the project subvolume as root: `wsl -d ai-os -u root -- /usr/local/libexec/ai-os-admin remove-dir /data/projects/rail-live` (a refusal there means the path is not a project folder — check).

- [ ] **Step 2: Run it**

```bash
wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && AI_OS_LIVE=1 cargo test -p aios-core --test live_1d -- --nocapture 2>&1 | tail -80"
```
Expected: pass. A failure is analysed like 1c's: a product gap is fixed in the code (the prompt, the hands, the service) and the run repeated; the assertion is never loosened to pass. Record every run and its cause in the design's Results block.

- [ ] **Step 3: The full suite at the commit gate**

```bash
wsl -d ai-os -u ai -- bash -lc "cd /mnt/c/Users/gdoum/Desktop/projects/ai-os/runtime && cargo test --workspace 2>&1 | grep -E 'test result|FAILED|panicked'"
```
Expected: every crate green (core, executor, proto, rail; the gated tests skipped unless their env is set).

- [ ] **Step 4: Write the results** — in the 1d design add `### Results (2026-09-16)` under §7 with: runs, seconds per script, what failed and what was changed, the screenshot path. In the parent spec add a `## Phase 1d status` block (what shipped, the display note for §5.1, carry-forwards from §8 of the 1d design).

- [ ] **Step 5: Commit** — `git add runtime/core/tests/live_1d.rs docs && git commit -m "test: Phase 1d live acceptance through the service; results recorded"`

Then STOP: report to him in plain words; merging to master waits for his word.

---

## Self-review against the spec

- §2 events: Task 1 (types), Task 2 (emission and `lines`), Task 3 (`files`), Task 5 (`needs_ok` re-ask), Task 6 (`state`). `busy` is emitted by the service (Task 7), as §3.3 says.
- §2.1 stop flag: Task 4. Changed files: Task 3. Approval question and its bound: Task 5.
- §3 service, socket, protocol, busy, start-up resume, shutdown-by-signal (nothing to do: the process exits, the db holds the job), unit and linger: Tasks 7–8.
- §4 rail: Tasks 9–10; reconnect and status line: Task 10's `net_thread`; Open: Task 10 `open_path`.
- §5 workshop wiring: Task 8 (`setup-rail.sh`, unit, linger, packages), Task 10 (build/install).
- §6 ripples: `testing.rs` goes `pub mod` (Task 7); `live_1c.rs` untouched and re-compiled (Task 8).
- §7 proofs: 1 Tasks 2–6; 2 Task 7; 3 Task 9; 4–5 Tasks 11 and 10; 6 is his.
- Path check (2026-09-16, read-only, against the live distro) folded in: gtk4 0.11 + std channel drain (no glib channel exists), the RefCell borrow scope in the rail, stop always armed AND queued, busy only for jobs, seq on broadcasts only, `bind` refuses a live socket, one split connection per client, `Service { name, action }`, changed-files test on a second job and before `LAST_RUN.md`, per-test temp tags, `systemctl --user -M ai@`, no `After=default.target`, `AI_OS_SOCKET_DIR` first.
- Names used across tasks: `Event`, `ChangedFile`, `FileKind::of`, `JobState`, `StepView`, `Waiting`, `UndoLine`, `Request`, `Client::{connect,say,hello,next_event}`, `Engine::{with_sink,handle_events,stop_flag,resume,state}`, `event::{describe,lines}`, `service::{bind,run,socket_path}`, `cards::{Cards,Card,CardKind,Button,Change,StepLine}` — consistent throughout.
