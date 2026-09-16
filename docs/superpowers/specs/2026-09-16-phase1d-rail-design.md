# Phase 1d — the engine service and the chat rail

Design agreed 2026-09-16 (five sections, each approved in conversation). Parent spec: `2026-09-15-ai-os-design.md` §4.9 (the front door), §4.10 (the stack), §11 (the 1b carry-forwards for 1d). Predecessors: `2026-09-16-phase1b-core-loop-design.md`, `2026-09-16-phase1c-hands-and-undo-design.md`.

**His four calls that shaped this (2026-09-16):**
1. A job keeps running when the rail window closes: the engine becomes a background service, the rail is a window onto it.
2. While a job runs, only "stop" acts at once; anything else gets a "busy" line. At a Needs-your-OK, a question is answered and the OK asked again.
3. The Done card lists the files the job changed, each with an Open button; images show a thumbnail.
4. 1d is proven both automatically and by a screenshot of the real window with a real job's cards, then by him.

## 1. What 1d delivers

- **Typed events from the engine** in place of prose lines. Every card the rail draws comes from an event; the terminal output becomes one function that turns events into lines.
- **The engine service** (`ai-os-engine`): a user service holding the one engine, listening on a private socket, speaking JSON lines. All front doors (rail, terminal workbench, tests) are clients of it.
- **The rail** (`ai-os-rail`): a native GTK4 window in Rust that draws the conversation as cards and sends what the user types. On the workshop it appears on the Windows desktop through WSLg.
- **Stop that lands mid-job**, a **busy** answer during a job, and **a question at a Needs-your-OK** answered without counting as yes or no (closes the two 1b carry-forwards).
- **Changed-files on Done**, the first "see the result" moment.

Not in 1d: pause and take-over controls, the Watch card (Phase 4), replaying earlier finished jobs in the rail, pinning the rail to a desktop edge (packaging, bare metal), steering a running job with new instructions (his call 2, option 2; revisit after skills), the desktop hand (Phase 2), voice.

## 2. The engine's typed events

`aios_core::event::Event`, serialised with serde, `kind` first (the `preserve_order` rule from 1b applies to everything the model or a client parses).

| kind | fields | when |
|---|---|---|
| `said` | `text` | a chat reply; also the "(Noted for the future: …)" line |
| `understood` | `job_id`, `name`, `text`, `housekeeping: bool` | a job starts (`Start` or `Housekeep` accepted) |
| `plan` | `job_id`, `steps: [String]` | the model's plan is accepted |
| `step` | `job_id`, `plan_step`, `text`, `ok: bool` | one action ran (or was applied by the engine, e.g. a setting) |
| `needs_answer` | `job_id`, `questions: [String]` | the model asked; waiting |
| `needs_ok` | `job_id`, `what`, `why` | an action was blocked for approval; waiting |
| `done` | `job_id`, `text`, `check: Option<String>`, `files: [ChangedFile]` | done-with-check accepted |
| `failed` | `job_id`, `text`, `files` | gave up, or the limits fired |
| `stopped` | `job_id`, `text` | the user said stop |
| `undone` | `job_id`, `lines: [{text, ok}]`, `notes: [String]` | after "undo"; `notes` are the not-covered lines |
| `busy` | `job_id`, `text` | text arrived while a job was running and was not stop |
| `state` | see §2.2 | answer to `hello` |

`ChangedFile { path, kind: "text" \| "image" \| "other", size }`. `step.text` is the one-liner the engine already builds for the model's own record (the action's plain description, e.g. "wrote find_primes.py"), not the raw detail; the detail stays in the action log.

### 2.1 How the engine emits

- `Engine` gains an event sink: `Box<dyn FnMut(Event) + Send>`, set at construction (`Engine::new(...)` grows one argument; `testing.rs` gets a collecting sink). `handle(&mut self, text)` returns `Result<(), EngineError>` and emits as it goes. Every emitted event is also appended to `messages` (role `assistant`, the rendered line) so `recent_messages` and the front-door prompt keep working unchanged.
- `event::lines(&Event) -> Vec<String>` produces exactly the sentences the engine emits today, one function, used by the terminal client. `handle` keeps its signature and returns those lines, so the existing tests are not changed at all; `handle_events` is the typed twin. A test that fails is a finding.
- **Stop flag**: `Arc<AtomicBool>`, checked at the top of every `run_turns` iteration and before each `perform`. When set, the engine finishes the job as `Cancelled` with the existing "Stopped the job in …" text, emits `stopped`, clears the flag. Cost: one step of latency (one model call plus one action), never more. Stop typed while the engine is *waiting* (answer or OK) goes through `handle` as today.
- **Changed files**: computed at `finish` for `done`/`failed`/`stopped` by walking the job's folder for entries whose mtime is at or after the job's start time (a new `Job.started_at` field, `serde(default)`), skipping directories named `.git`, `.venv`, `node_modules`, `target`, and any hidden entry, capped at 20 and sorted by path. A housekeeping job walks the housekeeping folder. Ceiling (ponytail): mtime, not content; a file touched but unchanged is listed. Files written outside the folder by the admin hand are not listed; the undo report covers them.
- **The question at a Needs-your-OK**: `handle_inner` in `WaitingApproval` becomes three-way: `is_yes` → approve; `is_refusal` (new list: `no`, `n`, `don't`, `dont`, `no thanks`, `refuse`, `not that`, `cancel that`, plus every `is_stop` phrase stays a stop) → decline as today; anything else → the model answers it with a `Reply`-only grammar over a new `prompt::approval_question(job, pending_reason, action, text)` prompt, the engine emits `said` with the answer and then `needs_ok` again with the same what/why; the job stays `WaitingApproval` with `pending_action` untouched. The answer is not a move that changes the job. Bounded: after 3 questions without a yes or no on the same action the engine says so and declines the action (the 1b rejection bound pattern), so a model that never satisfies the user cannot loop forever.

### 2.2 The `state` snapshot

```
state { job: Option<{ id, name, housekeeping, understood, plan: [String], steps: [{plan_step, text, ok}],
                     waiting: "none" | {"answer": [questions]} | {"ok": {what, why}} }> }
```
Built from `store.open_job()`; `waiting` mirrors `Job.state`. A job left mid-work by a crash is reported with `waiting: "none"` and the service resumes it (§3.3) — the rail draws it as Building.

## 3. The service

### 3.1 Process and socket

`ai-os-engine`, a binary in the `aios-core` crate (next to the existing `main.rs`, which becomes the thin client `ai-os-chat`). Runs as user `ai`. Listens on `$XDG_RUNTIME_DIR/ai-os.sock` (workshop: `/run/user/1000/ai-os.sock`, the directory is 0700 `ai`); the socket file is created 0600 and any stale file is removed at start. One `std::os::unix::net::UnixListener`, one thread per client, one engine thread. No async runtime, no new dependency beyond what is already in the workspace (`serde_json`). If something already answers on the socket the service refuses to start (two engines on one database is the failure that must not happen); only a stale, unanswered socket file is removed.

### 3.2 Protocol

One JSON object per line, UTF-8, `\n` terminated, both directions.

Client → service:
- `{"say": "<text>"}` — exactly what the user typed or a button sent (`yes`, `no`, `stop`, `undo`).
- `{"hello": {}}` — request a `state` event (answered to that client only).

Service → clients: the events of §2, every event to every connected client, in emission order, each broadcast carrying a monotonically increasing `seq` added by the service so a client can detect a gap (answers to one client alone — `state`, `busy`, `error` — carry no `seq`, so every client's run of numbers is contiguous). A `say` is also echoed to every client as `{"kind":"you","text":…}` so a second rail shows what the first typed. A line the service cannot parse gets `{"kind":"error","text":"could not read that message"}` on that connection and nothing else happens.

### 3.3 Inside

- The engine lives on one thread with an `mpsc` receiver of commands `Say(text)` / `Hello(client_id)`. Client threads push commands and hold a broadcast sender (a `Vec<Sender<String>>` behind a mutex; a send error drops that client).
- **Busy**: the service keeps `running: AtomicBool`, true while the engine thread is inside `handle`, and a mirror of the open job built from the events. A `say` that arrives while `running` is true AND the mirror holds a job is answered by the service itself with `busy` (text: "I'm working on <name>. Say stop if you want me to change course.") and is *not* queued; while only a chat reply is in progress (no job) the message is queued behind it. `stop` always sets the engine's stop flag AND is queued, so a stop typed before the engine has picked the request up cannot sit behind the whole job, and an idle engine answers it as today. While the engine is *waiting* (answer/OK) `running` is false and text is queued normally.
- **Start-up**: if `store.open_job()` returns a job in `Working`/`Planning`/`Asking`, the service emits `state` to new clients as-is and resumes the job by calling `handle("")`-equivalent `Engine::resume()` (a new public method wrapping the existing "carry on with it" arm, which today needs a user line to trigger). A job in a waiting state is left waiting.
- **Shutdown**: SIGTERM closes the listener; a running job is left in the database exactly as the 1b crash path expects; the service does not try to finish it.

### 3.4 The service unit

`~/.config/systemd/user/ai-os-engine.service` for `ai`, `WantedBy=default.target`, `Restart=on-failure`, environment `AI_OS_DB`, `AI_OS_MODEL`, `AI_OS_PROJECTS` as `main.rs` reads today. `loginctl enable-linger ai` so the user manager, and with it the service, is up whenever the distro is, not only while a terminal is open. Installed by `trial/setup-rail.sh` (§5).

### 3.5 The terminal client

`ai-os-chat` keeps its prompt and its `ai>` lines but does nothing itself: it connects, sends `hello`, prints `render::lines` of every event it receives, and sends each line typed as `say`. Scripted runs (the live acceptance) use the same client code as a library.

## 4. The rail

### 4.1 Program

`ai-os-rail`, a new crate `runtime/rail` (package `aios-rail`), Rust + `gtk4` (gtk4-rs, the stack's choice). A `gtk::ApplicationWindow`, default 420×900, a `gtk::ScrolledWindow` holding a vertical `gtk::Box` of cards, a `gtk::Entry` at the bottom that keeps focus. Enter sends `{"say": …}`. The socket is read on a background thread; events cross to the GTK main loop through `glib::MainContext::channel`.

### 4.2 Cards

`rail::cards` builds a `Card` model (a plain Rust struct tree: title, lines, buttons with their `say` payloads, thumbnail path) from the event stream; the window renders `Card`s into widgets. The model is what the rendering test exercises (§7) — no display needed.

| event | card |
|---|---|
| `you` / `said` | speech bubble, left/right |
| `understood` | opens a **Building** card: name + understood text, Stop button (`say: stop`) |
| `plan` | the Building card gains the step lines, each with an empty box |
| `step` | ticks the box of `plan_step` (✓ or ✗) and shows `text` under it; a second step on the same plan step replaces the line |
| `needs_answer` | **Needs your answer** card, questions in bold; the entry stays the only input |
| `needs_ok` | **Needs your OK** card: what, why, buttons Yes (`say: yes`) and No (`say: no`); typing something else asks a question (§2.1) |
| `done` | Building collapses to its heading; **Done** card: text, check in small print, files list — image kind shows a `gtk::Picture` thumbnail (max 200 px), every file has an Open button; Undo button (`say: undo`) |
| `failed` / `stopped` | the sentence, the files list, Undo |
| `undone` | **Undone** card: one line per entry with ✓/✗, then the notes in small print |
| `busy` | a small grey line, not a card |
| `state` | on connect: rebuilds the Building card (and the waiting card, if any) for the open job; nothing for earlier jobs |

**Open** runs `xdg-open <path>` from the rail process (`std::process::Command`, detached). The rail runs on the WSLg display, so the app opens there. Files under `/data` are readable by `ai` (the `ai:ai-sandbox` 2770 rule from 1c). Paths come from the service and are opened as given; the rail never builds a path from user text.

### 4.3 Connection

On start: connect, send `hello`, draw. If the socket is missing or refused: one line "The AI OS service is not running" and a retry every 3 s; on success the line is replaced by the rebuilt state. If the connection drops mid-session the same line appears and the retry loop runs; on reconnect `hello` rebuilds the open job and the earlier cards of this window session are kept above it.

## 5. Workshop wiring (all in setup scripts, never by hand)

`trial/setup-rail.sh` (root, workshop only, same pattern and same "does not ship" marking as `setup-admin.sh`):
1. `apt-get install libgtk-4-dev` (build) — the GTK4 runtime and the GNOME apps `xdg-open` reaches are already present from Phase 0.
2. Installs `ai-os-engine`, `ai-os-rail`, `ai-os-chat` from `runtime/target/release` into `/usr/local/bin` (workshop shortcut: copied from the Windows-mounted repo, as the admin wrapper is; the product ships them from a package).
3. Writes the user unit (§3.4) into `/home/ai/.config/systemd/user/`, `loginctl enable-linger ai`, `systemctl --user enable --now ai-os-engine` as `ai`.

Launching the rail from Windows: `wsl -d ai-os ai-os-rail` (`ai` is the distro's default user; `WAYLAND_DISPLAY=wayland-0` and `XDG_RUNTIME_DIR` are set by WSL for the login shell — verified 2026-09-16). Nothing is written on the Windows side; removal stays "unregister the distro". The headless GNOME session is not used by 1d.

Parent-spec §5.1 addition at close-out: the rail and apps opened from it use the WSLg display; the AI's own desktop work (Phase 2) uses the invisible session. On bare metal there is one display and the distinction disappears.

## 6. Changes to existing code (the ripples)

- `engine.rs`: every `Ok(vec![…])` becomes an emit; `handle` signature; stop flag; `is_refusal`; the approval-question arm; `resume()`; `started_at` on `Job::new`/`new_housekeeping`; changed-files at `finish` and at the stop path.
- `job.rs`: `started_at: u64` (unix seconds, `serde(default)`).
- `prompt.rs`: `approval_question`. `schema.rs`: a `Reply`-only grammar (the per-state narrowing rule from 1b).
- `store.rs`: unchanged tables; `push_message` now called from the emit path.
- `testing.rs`: collecting sink, `handle_lines`, `FakeModel` scripted reply for the approval question.
- `main.rs` → `bin/ai-os-chat.rs` (thin client) and `bin/ai-os-engine.rs` (service); shared client code in `client.rs`.
- New crate `runtime/rail`; workspace `members` grows by one. The rail crate is the only place GTK is linked; `aios-core` and `executor` stay display-free so the service and tests build on a headless box.
- `live_1c.rs` keeps driving the engine as a library (its assertions are on lines via `handle_lines`); the new `live_1d.rs` drives the service over the socket.

## 7. How 1d is proven

1. **Engine unit tests** (`aios-core`): the 93 existing pass unchanged through `handle_lines`; new tests — one per event kind; stop flag set mid-job finishes the job as Cancelled within one step and emits `stopped`; the approval-question path (a question → `said` + `needs_ok` again, `pending_action` unchanged; `no` declines; `yes` approves; the 3-question bound); changed-files (mtime, skips, cap, housekeeping folder); `state` for each waiting state and for none.
2. **Service test** (`runtime/core/tests/service.rs`): a temp socket, a scripted model, two clients — both receive every event with contiguous `seq`; `say` during a job returns `busy` without reaching the model; `stop` during a job lands; `hello` mid-job returns the right `state`; a client dropping mid-job changes nothing; a bad line gets `error` and nothing else.
3. **Rail rendering test** (`runtime/rail/tests/cards.rs`): a fixed event sequence (understood → plan → 3 steps → needs_ok → said → needs_ok → done → undone) produces the expected `Card` tree and button payloads. No display.
4. **Live acceptance** (`runtime/core/tests/live_1d.rs`, `AI_OS_LIVE=1`): real qwen3.5:9b, real hands, the service on a temp socket, no human: a project job that hits a Needs-your-OK (a `/etc` read-then-write pattern from 1c's script is the known trigger), a question at the OK answered and re-asked, `yes`, Done with a non-empty files list, then `undo` with `undone` lines. Asserts the event sequence and kinds, never wording; freshness asserted as in 1c.
5. **The screenshot**: the real `ai-os-rail` on the Windows desktop showing a real job's Building card mid-tick, a Needs your OK, and a Done card with Open and Undo, captured through the Windows-side computer-use screenshot and saved to `docs/superpowers/findings/phase1d-rail.png`. Without it the rail is not reported as verified (his rule).
6. **Him**: opens the rail, gives it a job in his own words, watches it, presses Open on a file, presses Undo.

Test commands stay as in 1c (inside the distro; no Rust on Windows). The rail crate is built only in the distro; its tests run there too.

## 8. Out of scope, carried forward

- Steering a running job with new text (call 2, option 2): after the skills phase.
- Pause, take-over, the Watch card, replaying earlier jobs, pinning the rail to an edge.
- Changed-files by content (not mtime); files the admin hand wrote outside the folder listed on the Done card (the undo report covers them today).
- A D-Bus front for the service (revisit when Phase 2 brings zbus in).
- Multiple engines/users; the socket is per-user by construction.
- The `busy` answer is service-side: a stop that arrives in the same instant the engine finishes a job is queued and then means "no job to stop" — the engine's existing answer covers that.
