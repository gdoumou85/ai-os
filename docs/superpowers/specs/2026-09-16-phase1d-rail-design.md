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

### Results (2026-09-16)

All of it inside the `ai-os` distro as user `ai`, `runtime/` as the working directory, Ollama up with `qwen3.5:9b`, the engine service on a temp socket of the test's own and its own database `/data/ai-os-live-1d.db` — the real `ai-os-engine` on `/run/user/1000/ai-os.sock` was left alone throughout.

**The live acceptance has passed.** `AI_OS_LIVE=1 cargo test -p aios-core --test live_1d -- --nocapture`, run 9, **60.2 s** for the four scripts end to end (72 s including the build): a fresh database and no `rail-live` folder, then the real 9B through the socket with nobody typing but the test.

- **Script 1, the project job** — "make a new project called rail-live with a python script that prints the first five primes; decide everything yourself and do not ask": `understood` naming the project, a five-step `plan`, `step` lines for `wrote BLUEPRINT.md`, `ran mkdir -p src`, `wrote src/primes.py`, `ran python3 src/primes.py`, and a `done` carrying the check and two changed files, both under `/data/projects/rail-live`, one of them the `.py`.
- **Script 2, the Needs-your-OK** — "write the single word hello into `/etc/ai-os-live-1d.txt`": the write was blocked and came back as `needs_ok`; the question "what exactly will you write there?" was answered with a `said` ("I will write the single word 'hello' into /etc/ai-os-live-1d.txt.") and the same OK asked again, the action untouched; `yes` ran it, the check read the file back, and `done` arrived. `/etc/ai-os-live-1d.txt` held `hello`.
- **Script 3, undo** — two `undone` lines, both ok: "Undo removed /etc/ai-os-live-1d.txt (it did not exist before)" and "Restored the files of /data/projects/rail-live from before the job". The `/etc` file was gone from disk.
- **Script 4, busy and stop** — a second client's message mid-job was answered `busy` without reaching the model, and `stop` landed as `stopped` within a step.

**Nine runs went into getting there, and six gaps the failures exposed were fixed in the code — never in the test, and no assertion was ever loosened.** In order:

1. **`make_dir` for a folder inside the project (run 1).** The 9B planned `make_dir src` for a folder in its own workspace. `make_dir` is the privileged hand and the wrapper takes absolute paths outside the workspace only, so the approval gate would have spent the user's yes on an action that then fails. The executor now refuses a `make_dir` that resolves inside the working directory outright and names the hand that does it (`run_command mkdir -p …`), the way `wrong_hand` has named the right hand since 1c; the system prompt says the same in one line.
2. **A file outside the project written with `echo` (run 2).** Told to write `/etc/ai-os-live-1d.txt`, the model ran `echo hello /etc/ai-os-live-1d.txt`. In the sealed sandbox that command succeeds — echo printed two words — and the model then read back a file that was never written and gave up. `wrong_hand` now catches the writing programs (`echo`, `printf`, `tee`, `cp`, `mv`, `touch`, `dd`, `truncate`, `chmod`, `chown`) aimed at an absolute path outside `/data` and names `write_file`. Reads of the machine and anything under `/data` stay free.
3. **A question at a Needs-your-OK answered with `busy` (run 3).** The service's `running` flag only falls when `handle_events` returns, which is *after* the `needs_ok` has reached the client — so a question typed the instant the card appears raced it and came back "busy", the one thing §2.1 promises it is not. The busy decision now reads the job mirror's `waiting`, which the sink updates before it broadcasts and so cannot race. A new service test holds it: `a_question_at_a_needs_ok_is_answered_never_answered_with_busy` (it was confirmed to fail against the old rule before the fix went in).
4. **Giving up for want of a yes it cannot ask for (run 4).** The model planned the `write_file` it needed and then gave up on the job, "because the user did not provide confirmation". Nothing in the prompt said who does the asking. It now does: where a move needs the user's yes, the machine stops it and asks them — take the step, never wait for permission, never give up for the want of one.
5. **The same yes asked twice (run 5).** After the approved `/etc` write had succeeded, the 9B re-issued it verbatim, the gate blocked it again, and the job sat on a second Needs-your-OK nobody was there to answer. A yes now holds for the rest of the job (`Job::approved_actions`, the mirror of `declined_actions`, which is still checked first so a later no wins).
6. **The same successful action over and over (run 6).** Freed of the second OK, the model wrote the same `/etc` file nine times in a row and had no steps left for the rest of the job. An action identical to the one that just succeeded is now rejected with "that exact action just succeeded — its result is in the steps above; move on". Only an *immediate* repeat: re-running a command after something else changed stays legitimate, and a `done` check is exempt entirely.
7. **One retry for the blueprint gate (runs 7 and 8, the same failure twice).** With the write done, the model said `done`, the gate held it back for the BLUEPRINT update, it said `done` again, and the grammar budget of two ended the job. A `done` held back by the gate is a legal move refused by policy, not an answer the system could not read, and what it asks for is one concrete extra step — so it has its own bound now (`Job::done_gated`, `MAX_DONE_GATED = 4`) and the note tells the model to do it with an act and then say done again. A model that only ever says done still ends.

Run 5's first attempt was thrown away for a fault of the operator's, not the product: two acceptance runs were started against the same database and the second cleared the file the first had open ("attempt to write a readonly database"). It is counted here for honesty and produced nothing.

Every fix carries its own test, all of them new: `a_make_dir_inside_the_working_directory_is_refused_with_the_right_hand_named` and `a_free_command_writing_outside_the_ai_areas_is_sent_to_write_file` in `executor`, `a_question_at_a_needs_ok_is_answered_never_answered_with_busy` in `tests/service.rs`, and `a_yes_holds_for_the_rest_of_the_job_and_the_same_action_is_not_asked_twice`, `the_same_action_twice_in_a_row_after_it_worked_is_rejected_not_run_again`, `a_done_held_back_by_the_blueprint_gate_has_room_for_more_than_one_retry` and `system_rules_say_who_asks_the_user_and_which_hand_reaches_outside` in `aios-core`. No existing test was edited.

`cargo test --workspace` at the commit gate: every crate green — 116 in `aios-core`'s library, 9 in `tests/service.rs`, 3 in the rail's cards, 67 in `executor`, 7 in `admin_integration`, 9 in `sandbox_integration`, and the gated tests (`live_1c`, `live_1d`, `live_primes`, `snapshot_it`) skipping without their env.

**The screenshot** (§7 item 5) is `docs/superpowers/findings/phase1d-rail.png`: the real `ai-os-rail` window on the Windows desktop through WSLg, a real session on the `rail-demo` project — Building cards, a Done card carrying its summary, its check line and its two changed files (`BLUEPRINT.md` and `sum5.py`, an Open button on each) with Undo underneath, a few chat replies, and at the foot the Undone card with "Restored the files of /data/projects/rail-demo from before the job" and the not-covered note. It also caught, before the fix, the failure numbered 7 above: a "Could not finish" card reading "I kept answering in a way the system could not accept (update BLUEPRINT.md for what you changed before saying done)". Item 6 — him at the window — is still his.

## 8. Out of scope, carried forward

- Steering a running job with new text (call 2, option 2): after the skills phase.
- Pause, take-over, the Watch card, replaying earlier jobs, pinning the rail to an edge.
- Changed-files by content (not mtime); files the admin hand wrote outside the folder listed on the Done card (the undo report covers them today).
- A D-Bus front for the service (revisit when Phase 2 brings zbus in).
- Multiple engines/users; the socket is per-user by construction.
- The `busy` answer is service-side: a stop that arrives in the same instant the engine finishes a job is queued and then means "no job to stop" — the engine's existing answer covers that.
