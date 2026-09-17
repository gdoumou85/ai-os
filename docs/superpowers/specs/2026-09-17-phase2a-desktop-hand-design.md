# Phase 2a — the desktop hand

Design agreed 2026-09-17 (three parts, each approved in conversation; then a complexity pass and a correctness audit against the code, folded in — §14). Parent spec: `2026-09-15-ai-os-design.md` §2 decisions 8, 9 and 12, §4.4 (hands and senses, tier 2), §4.7 (the desktop worker), §5.1 (the workshop's two displays). Predecessors: `2026-09-16-phase1c-hands-and-undo-design.md` (the hands and the risk rule as code), `2026-09-16-phase1d-rail-design.md` (events, service, rail). Findings that shaped it: `../findings/phase0-findings.md` Task 4, and the probe in §9 below.

**His four calls that shaped this (2026-09-17):**
1. Phase 2 is split: 2a is the desktop hand (this design), 2b is the screen fallback. Browsers through their own automation wait.
2. One job kind covers both the window the user hands over and a window the AI opened itself: a window is a window, wherever it was opened.
3. **Nothing is per-app.** The hand is one protocol for every program; the model does the understanding, and a bigger model understands more with no code change. LibreOffice's own scripting API (UNO), which the parent spec listed as a text route, is out: it is per-app code, so if Writer ever needs it that is a learned skill (Phase 3), not hand code.
4. What the model sees of a window is capped by a setting that follows the context budget, so the deploy PC's larger model sees more without a code change.

## 1. What 2a delivers

- **The desktop worker** (parent §4.7): the model's third hand, operating program controls through the accessibility bus. Five actions: `look`, `press`, `type`, `read`, `open_app`. One protocol for every toolkit, no per-app code anywhere.
- **The window job**: "take my Writer window and …" is a `housekeep` job — a job with no project — whose goal names a window. No new move. Any job may use the hand; a project job may open an app to check its own output.
- **Decision 9 for the hand, as code**: a press on a control whose name means leaving the machine or destroying unsaved work stops at Needs-your-OK.
- **The invisible session as a unit** (`ai-os-desktop.service`), installed by a setup script like the engine's, in place of the Phase 0 hand-run script.
- **The honest undo line** on any Done card of a job that used the hand.

Not in 2a: the screen fallback (2b: Mutter screencast and a pixel click); browsers through CDP or WebDriver; the clipboard-paste text route; typing on the user's real seat where insertion does not commit; pause and take-over; "Watch it work" on the Building card; the Watch card.

## 2. The window job

A window job is the existing `housekeep` move: a job with no project, no blueprint, working in the housekeeping folder, carrying the user's words verbatim (1c). The front door's description of `housekeep` widens from "the machine's own layout" to "the machine's own layout, or a window the user named, or a program the AI opens itself to do what was asked" (§12's script 2, "open the calculator and tell me what 12 times 34 is", is the second kind: the window does not exist yet, and it is still the machine, not a project), and the housekeeping job prompt gains one sentence: a window named by the user is found with `look` first. No new move, no new job kind, no new field on the job: what a window job differs in is what its goal mentions.

```json
{"move":"housekeep","goal":"take the Text Editor window, add a line saying reviewed and save","understood":"Taking your Text Editor window to add a line and save"}
```

The window is matched by the model against the app names and titles `look` reports. If none matches or more than one does, rule 8 applies: the job asks before acting.

## 3. The five actions

All five go through the accessibility bus (AT-SPI). None knows what any app is.

| Action | Fields | What it does |
|---|---|---|
| `look` | `window?`, `find?` | Without `window`: the open windows, app name and title. With one: its controls — short `id`, role, name, state flags (checked, disabled, focused, showing), and a preview of any text or value. `find` keeps only controls whose name or role contains the word. Menus appear as names; press one and look again to see its items. |
| `press` | `control`, `name` | The control's default accessibility action: a button clicks, a toggle flips, a menu opens, a menu item activates. `name` echoes the name `look` reported for that id; the worker refuses a press whose echoed name differs from its table ("look again"), so the risk rule in §6 reads a name the worker has verified. |
| `type` | `control`, `text`, `replace?` | Inserts `text` at the end of the control's text through its editable-text interface. `replace: true` clears it first. |
| `read` | `control`, `from_line?`, `lines?` | The control's full text, windowed by line like `read_file`, for documents too long for a preview. |
| `open_app` | `name`, `visible?` | Launches a desktop application by its desktop-entry name (`org.gnome.TextEditor`, `libreoffice-writer`) in the invisible session, or on the visible display when `visible` is true. Name validated by its own rule, `^[A-Za-z0-9][A-Za-z0-9._-]*$` (desktop-entry ids carry dots and capitals), and resolved to an existing `.desktop` file before anything runs. Never a shell string. The `.desktop` file is looked for in the system folders only — `/usr/share/applications`, `/usr/local/share/applications` — and never in `/home/ai/.local/share/applications`: that is the AI's own home, which an approved `write_file` reaches, and `open_app` is the one action that starts a program, so a launcher written there would run as user `ai` with nothing of the sandbox around it. |

**Control ids.** The worker keeps one table from short integer ids to (application bus name, object path, name as last reported). It lives in the engine process for as long as it runs, ids are never reused, and `look` hands out new ones only for objects not already in the table. Every use of an id asks the object first; one that no longer answers reports "that control is gone; look again" rather than acting on anything else. Nothing is cleared per job: an id is only ever the object it was handed out for. The model never sees a bus name or an object path.

**Which windows.** The probe (§9) confirmed that windows on the WSLg display and windows in the invisible session register on the same accessibility bus, so one worker sees both and `look` lists both. `open_app` decides where a new window appears: invisible unless the user asked to see it, which is one prompt rule for the model's `visible` flag ("show me", "open … for me"). On bare metal both display names are the same and the distinction disappears (parent §5.1).

## 4. The budget

`look` returns interactive and readable controls only: buttons, toggles, check boxes, radio buttons, menus and showing menu items, entries, text areas, combo boxes, page tabs, and short labels (a status bar's "1 word, 17 characters" is a label the model needs). It skips panels, fillers, separators, anything not showing, and a label whose text is another control's name — a toolkit that exposes each button's caption as a label beside it would otherwise spend half the cap saying everything twice (found in the acceptance: GNOME Calculator's `=` fell off the end because of it). Controls come in tree order with the focused one first.

It stops at a cap and ends with "N more, narrow with find". The cap is the model's context budget divided by 200, never below 20: 40 on the 8k workshop, more on the deploy PC's full context. `Model` gains `context_tokens()` (the Ollama connection's `num_ctx` literal moves behind it), and the worker is built with the cap. No setting: nobody would set it, and the number already follows the model.

`read` is windowed like `read_file` and capped the same way; `type` text is summarised in the job record like a big `write_file` (1b's I4) so it cannot blow the prompt.

## 5. The worker and the dispatcher

- `DesktopWorker` in the `executor` crate, beside `SandboxWorker` and `AdminWorker`. A fourth `Lane::Desktop` in `executor::lane` for the five actions, listed and not `_`, so a sixth desktop action fails to compile until it is placed.
- It runs **inside the engine process**. The engine already runs as user `ai` on the user bus; the accessibility bus address comes from `org.a11y.Bus.GetAddress` on that bus. No new process, no new privilege, no root.
- **Dependency: `zbus` alone, blocking API**, with hand-written proxies for the five AT-SPI interfaces the hand uses — Accessible, Action, Text, EditableText, Component — plus the a11y bus's `GetAddress`. The audit found the `atspi` crate is async-only (built on zbus's async-io feature, no blocking proxies), so what the design earlier named the fallback is the one blocking option and becomes the choice. Roles come as the strings AT-SPI's `GetRoleName` gives; states as the bit numbers of the five the hand reads (showing, sensitive, editable, checked, focused). The proxies are the size of the Phase 0 probe.
- **How it is wired.** The engine builds its workers through a factory closure per action (`WorkerFactory`, engine.rs) and constructs a fresh `Executor` every step. The factory's pair becomes a triple. The desktop worker's bus connection and id table are one shared state the closure captures (`Rc<RefCell<DesktopState>>`), so they outlive any single `Executor`; the worker handed to each `Executor` is a thin handle onto it. The engine binary, `testing::scripted_workers` and `FakeWorker` follow.
- Nothing is cached between looks except the id table. A `look` walks the window's tree fresh, bounded by node count as the Phase 0 probe was.
- **Environment:** the worker reads `AI_OS_DISPLAY_INVISIBLE` (workshop: `wayland-ai`, pinned in §8) and `AI_OS_DISPLAY_VISIBLE` (workshop: `wayland-0`, WSLg) and sets `WAYLAND_DISPLAY` for what `open_app` launches, through `gtk-launch <entry-id>` (present in the distro). Both are `Environment=` lines in the engine's unit file; on bare metal they are the same value.
- **A second press of the same button is legal.** 1d's "that exact action just succeeded" refusal (engine.rs) exempts `press` and `type`: a repeated digit or a second Next is normal desktop work. *A second* is all this claims: a third is refused, and for a window action the refusal says to look and see what the press did.
- **A window action that failed may be tried again after a fresh look.** The same reason on the failing side: a window moves on between steps, so a `read` refused for want of a look works the moment one happens. 1d's "that exact action already failed" rejection does not apply to the five window actions. A repeat is an ordinary failed step, bounded by `MAX_FAILS_PER_STEP` and `MAX_STEPS`, not by the rejection budget (which is two and fatal).

## 6. Decision 9 for the hand, as code

`rules::classify` for the five actions:

- `look`, `read`, `type`: `Auto`. `open_app`: `Auto`; a bad name is refused by the worker as a failed step, never asked about (an unattended job must not stall on a yes that cannot help — found in the acceptance).
- `press`: `Auto`, unless its `name` (echoed by the model, verified against the table by the worker before anything runs, §3) contains a word from a fixed list, case-insensitive — `classify` stays a pure function of the action:
  - **leaves the machine:** send, post, publish, upload, share, pay, buy, submit, order;
  - **destroys unsaved work:** close, quit, discard, don't save, delete, revert, replace.
  Then `NeedsConfirm("press Close: it may throw away unsaved work")`, with the control named (the window is in the step wording the rail shows). A yes holds for that exact action for the rest of the job, a no wins over it, and neither outlives the job (1d).
- Ceiling, stated: a button called "Go" or "OK" that sends is not caught. The list is the rule the executor can apply without asking the model whether its own action is risky (decision 9's last sentence). The list lives in `rules.rs` next to the path rules and is tested there.

`open_app` does not close anything and no `close` action exists: the AI closes a window only through the app's own control, which the list catches.

## 7. Undo

Nothing done inside a window is put back by the AI. Unsaved work has no snapshot (parent §4.8, limit), and desktop actions record no reverse. Files an app saves inside a project during a project job are covered by that job's snapshot as before; a window job has no project, so a file its app saves is not covered. Every Done card of a job that used the hand carries one line: "what I did inside Text Editor can't be undone by me". The `done` event gains `windows: Vec<String>`, `#[serde(default)]` so an old service's `done` still parses in a new rail; the engine computes it at Done from the job's executed desktop actions (no new field on the job). The rail and the terminal lines derive the sentence from it; no new event kind.

## 8. The invisible session as a unit

`ai-os-desktop.service`, a user unit for `ai`, installed by `trial/setup-desktop.sh` the way `setup-rail.sh` installs the engine's:

- `gnome-shell --headless --wayland-display wayland-ai --virtual-monitor 1600x900 --wayland --no-x11` (GNOME Shell 50 accepts `--wayland-display`, so the name is pinned), `WantedBy=default.target`, `Restart=on-failure`.
- The environment work the Phase 0 script does by hand — `XDG_CURRENT_DESKTOP=GNOME`, `XDG_SESSION_TYPE=wayland`, the toolkit-accessibility setting, the activation environment for the portals, `graphical-session.target` raised — moves into the unit and an `ExecStartPost` script.
- `ai-os-engine.service` gets `After=ai-os-desktop.service` and the two display `Environment=` lines. No log-scrape, no drop-in, no restart.
- **Check in the plan:** both units must survive the launching shell's exit. Today's probe (§9) saw the user manager stop a minute after boot when the shell that started it closed, despite linger, taking the headless shell with it; the engine came back when the manager restarted. The setup script proves the units are still up sixty seconds after it exits, or the plan finds out why.

## 9. The probe that corrected Phase 0 (2026-09-17)

Throwaway, `trial/probes/text_probe.py` and `text_probe2.py`, run by `trial/run-text-probe.sh` and `run-text-probe2.sh`. Four cells: GNOME Text Editor and LibreOffice Writer, each in the invisible session and on the WSLg display.

| Route | Text Editor (GTK4) | Writer |
|---|---|---|
| `EditableText.insertText` | **commits** — the window title, which mirrors the buffer's first line, changed to the inserted words; both displays | reads back but the status bar stays "0 words, 0 characters"; both displays |
| `EditableText.pasteText` after `wl-copy` | not reached headless (`wl-copy` needs a focused surface: "This seat has no keyboard") | false |
| Edit > Paste menu item action after `wl-copy` | no Edit menu | committed once ("1 word, 17 characters"), failed on three retries |
| menu and button actions | as Phase 0 | as Phase 0 |

**Corrections to Phase 0 Task 4 and parent §4.4:** free text through the accessibility interface does **not** need a seat; it commits in GTK widgets on any display. Writer's document body is the exception — its accessibility object accepts and reads back an insertion that never reaches the document — and that is a property of Writer's implementation, not of the session. The clipboard-plus-paste route is feasible but not dependable as probed, and is carried forward.

**Consequence for 2a:** `type` is the one generic text route. The worker cannot tell a committed insertion from Writer's phantom one, and 2a does not paper over it: Writer is worked through its controls, menus, dialogs and Save; free text into its body waits for a learned skill or a dependable clipboard route. The acceptance uses Text Editor for text and would use Writer for controls.

## 10. Changes to existing code (the ripples)

- `executor::action::Action`: five new variants; `lane` and `classify` list them; `Press` carries `name`.
- `executor::Executor`: a third worker field; `execute` routes `Lane::Desktop`; the worker refuses a press whose echoed name differs from its table, the way `wrong_hand` refuses today.
- `executor::desktop`: the zbus blocking proxies, `DesktopState` (connection, id table, cap), `DesktopWorker`.
- `aios_core::engine`: `WorkerFactory` returns a triple; the "just succeeded" refusal exempts `press` and `type`; `done` carries `windows` computed from the steps; `describe`/`lines` wording for desktop steps ("pressed Bold in Writer", "typed two lines into Text Editor", "opened Calculator").
- `aios_core::model`: `context_tokens()` on `Model`; the engine binary computes the cap.
- `aios_core::prompt`: standing rules for the hand (§11); the front door's `housekeep` description and the housekeeping job prompt widened by a sentence each.
- `aios_proto::Event::Done { windows }` with `#[serde(default)]`.
- `aios_rail::cards`: the Done card names the window instead of files when files are empty and windows are not; the undo line.
- `trial/ai-os-desktop.service`, `trial/setup-desktop.sh`; `trial/ai-os-engine.service` gains `After=` and the two display lines.

## 11. The prompt

Standing rules in the style of the other hands', short: look before you act and again after; ids come from the latest look; text goes in with `type`, never with `run_command`; buttons and menus with `press`; `open_app` opens programs, never `run_command`; a window job's Done check is a `read` or a `look` that shows the result, because the sandbox cannot see a window. No application is named anywhere in the prompt.

## 12. How 2a is proven

1. **Executor unit tests**: `lane` for the five actions; `classify` for the word list, both halves, case-insensitive, and the free ones; the id table (hand-out, no reuse, "gone"); a press with a wrong echoed name refused before anything runs; `look`'s filter, order and cap on a fake tree; `open_app` name validation (dots and capitals pass, slashes and spaces do not).
2. **Engine unit tests**: a window goal through the front door as `housekeep`; `done` carrying `windows` computed from the steps; the second identical press allowed; the step wording.
3. **Proto and rail tests**: `Done` with and without `windows` round-trips; the Done card for a window job.
4. **Live acceptance** (`runtime/core/tests/live_2a.rs`, `AI_OS_LIVE=1`): real qwen3.5:9b, the service on a temp socket, no human, three scripts:
   1. **The user's window.** The test opens a text file in Text Editor on the visible display. "Take my Text Editor window, add a line saying reviewed and save it." Done, and the file on disk has the line.
   2. **The AI's own window.** "Open the calculator and tell me what 12 times 34 is." The app opens in the invisible session, buttons are pressed, the result is read from the display and said. Nothing appears on screen.
   3. **The risky press.** "Take my Text Editor window and close it without saving." A Needs-your-OK naming the control, `yes`, and the window is gone from the bus.
   A failed run is fixed in the code, never in the test; every run and its cause is recorded in a Results block here.
5. **Him**: hands the rail a window of his own on the Windows desktop and watches it worked.

Test commands as in 1d (inside the distro). `cargo test -p executor` and `-p aios-core` stay headless; the live test needs the desktop unit up.

### Results (2026-09-17)

**The live acceptance passes.** `runtime/core/tests/live_2a.rs` through `trial/run-live-2a.sh`: real qwen3.5:9b, the engine service on a temp socket, three hands, no human, **81 s** for the three scripts — the user's Text Editor window taken, a line added and the file on disk saved (**24 s**); the calculator opened in the invisible session, `12 × 34` worked out on its own controls and **408** said, with nothing appearing on screen (**40 s**); "close it without saving" held at a Needs-your-OK naming the press, `yes`, and the window gone from the bus (**11 s**). The whole workspace suite is green beside it (**263 tests**). It has passed five times over the three rounds — 81 s, 86 s, 83 s, 89 s and 83 s, the last on the final code.

**Twenty-two runs went into it**, across three rounds: nine to the first pass, six more after the review changed the prompt and the risk rules, and seven in the fix round that followed the whole-branch review. Sixteen of them exposed a gap — the first before the model ever ran, and one of them a gap in the fix that was asked for rather than in the product — and five passed. Every gap was fixed in the code with a test, never in the acceptance. Numbered below as they ran.

1. **The desktop unit never came up on a cold boot.** Its `ExecStartPost` ran `systemctl --user restart xdg-desktop-portal*` and waited: the portal's own start job waits on its bus name, so the unit sat in `activating` until the start timeout killed it, and restarted, forever. The hand needs the accessibility bus, not the portals — both that restart and the `graphical-session.target` start are `--no-block`.
2. **A window would not answer to the name the window list gave it.** The model read `gnome-text-editor — live-2a-notes.txt (…) - Text Editor` off the listing and said it straight back; the matcher only ever compared the app and the title separately. And **nothing could save**: GTK 4's popover menu items carry no accessible name, no description and no label child, so the Main Menu came out as ten identical `[menu item]` lines. Their `keyshortcuts` attribute is the only thing that tells Save from Print, so a nameless control is now named by it — `Ctrl+S` — and one rule says a program's own commands live behind its menu button.
3. **The wrapper refusal was a dead end.** GTK 4 lists a menu button twice, a push-button wrapper with no action and the toggle button that has it; "Main Menu has no action to press" left the 9B to invent keyboard shortcuts and press the document by its first line. The refusal now names the id to press instead.
4. **`look` with `window: ""`** — the 9B's way of writing "list the windows" — was refused, so it tried to open the app by its window title and the job stalled on an OK for a name that could never work. A blank name is no name.
5. **A failure outlived the world that caused it.** Typing before looking is refused ("control 1 was never handed out"); after the look, the same action was still held against it. A `look`, `press`, `type` or `open_app` that succeeds now clears the earlier failures, as a write already did.
6. **The ids are not stable.** The toolkits recycle their objects' paths, so an id from one look belongs to another control after the next press — "control 58 is named 0 now, not 4" — and GNOME Calculator did it four times in one job, each costing a look and a replan, until the 25-step budget ran out. The refusal now asks the bus which id answers to that name right now and says so.
7. **The calculator became a software project.** "Open the calculator and tell me what 12 times 34 is" came back as `start`, blueprint gate and all, because housekeeping offered only "a window the user named" and this window did not exist yet. §2 widens in practice: the front door offers a program on the desktop whether the user has it open or the AI opens it. (The Done card also listed `""` among the windows worked; a blank look is no window.)
8. **A refused press answered with a replan whose one step was the action** — `{"kind":"look","window":"gnome-calculator"}` — five times, until the replan budget killed the job. A replan carrying an action, or the plan already in hand, is answered with a note naming the move it wanted; the plan in hand is left standing, and it costs a replan, not a rejection (run 12 below is the same finding read the rest of the way).
9. **Passed**, 81 s — the first round's pass, on the code the eight gaps above had made.

**The review round (runs 10–15).** The review changed the prompt's menu rule and made a bad `open_app` name a failed step instead of a question (§6 above), so the acceptance was run again. Five more gaps came out of it, almost all in how a refused step is answered — the part of the loop the first round never reached, because the model had always failed somewhere earlier.

10. **"look first" was true and useless.** The model listed the windows, read a control, and was told "control 1 was never handed out; look first" — which it had just done, at the window *list*, which hands out no ids. It sent the same read again and the job died. The refusal now says which look: at a window, and that looking with no window only lists the windows.
11. **The `=` key was out of reach.** GNOME Calculator exposes each of its keys twice, as a button and as a label saying the same word, so a window of 31 controls came out as 61 lines, the 40-control cap cut the look off at `×`, and `=` (52) was never listed; the model guessed four ids for it. A label whose text is a control's name is filler like a panel, and `select` drops it — the calculator now fits the cap whole. The refusal for a name nothing carries says so, and names the one move that reaches past a cap: look again with `find=<name>`.
12. **A replan in the wrong move died faster than before the guard.** Run 8's fix refused it through the rejection path, whose budget is two and fatal. It is a note on a counted replan now: the plan in hand is left standing, the model is told which move it wanted, and the bound is the replan budget of five.
13. **A window action that failed could not be tried again.** Three runs died here: the model read a control before looking, was refused, sent it again, and the "already failed" rejection ended the job one failed step in — while the read it was refused works the moment a look happens. §5 already exempts a repeated press from the *succeeded* guard because a window moves on between steps; the same holds on the failing side, for all five window actions. A repeat is a failed step, counted by `MAX_FAILS_PER_STEP`; a command repeated with nothing changed is still the 1d lesson it was.
14. **"A second press" meant eight.** With the retry allowed, the model pressed Main Menu eight times in a row without looking once, toggling the one popover open and shut, every press answering "pressed Main Menu" as though it had got somewhere. §5 claims *a second* press and that is now all the code exempts; the third is refused, and for a window action the refusal says to look at the window and see what it did.
15. **Passed**, 83 s.

**The fix round (runs 16–22).** The whole-branch review of 2a ruled on nine items; two of them touch the live path — the id check against the bus (§3's control ids) and one clause on the prompt's failed-step rule — so the acceptance was run again. Seven runs: five failures, all of them script 1, and two passes. One gap was a real dead end in the hand. The other finding was the prompt clause itself, and it is recorded as a reversal, not a fix.

16. **A `find` that kept nothing was a dead end.** The Save the model wants is a nameless GTK 4 popover item listed under its shortcut, so `find=Save` matches nothing and the look came back as a bare `controls of …:` with no lines under it. The model read that as "the menu did not open", asked for `find=Save` twice more, and spent its whole replan budget describing the blank answer to itself. An empty answer now says the window has no control of that name, says to look again without `find`, and says why the word missed: a control with no name of its own is listed under its keyboard shortcut.
17–21. **The prompt clause the review asked for costs the run, every time.** The review found that "a step that failed once will fail again" contradicts the engine's exemption for window actions (run 13), and asked for one clause on it: ", except a window action after a fresh look: what a window shows can change." Four runs carried it — three with that wording, one with "Only a window action may be sent again, and only after a fresh look", which leads with the duty instead of the permission — and all four failed script 1 on a near-identical trajectory: told that a window action may be tried again, the 9B re-sends the press of the push-button wrapper whose refusal already says it has no action and names the id that does, which is a refusal no look can change, and burns the replan budget on it. The fifth run, with the clause taken out and nothing else changed, passed in 89 s. The clause is reverted. **The contradiction is real and still open**: the engine's exemption is unconditional while §5 and the clause both say *after a fresh look*. Narrowing the exemption to match — a window action that failed may be sent again only once a look has succeeded since — would make the clause true and stop the wrapper loop, but it revives run 13 unless the second send is answered with a note rather than a rejection (that budget is two and fatal). That is its own ruling. Until it is taken, the prompt keeps the stricter rule: stricter than the floor the engine enforces, which costs the model nothing it can catch the machine in.
22. **Passed**, 83 s, on the final code — the user's window taken, `reviewed` added and saved with `Ctrl+S` off the menu (the run-16 fix carrying its weight); the calculator opened invisibly and **408** read off its display; "close it without saving" held at a Needs-your-OK, `yes`, and the window gone from the bus. The workspace suite is green beside it, **263 tests**.

**The §8 survival check (Task 8):** the units are fine; WSL is not. `ai-os-desktop.service` and `ai-os-engine.service` stay up under linger for as long as the distro runs, but WSL shuts the whole distro down about sixty seconds after the last `wsl` command exits, taking the user manager with it. A cold start brings both units back on its own. On bare metal the question does not arise; on WSL an unattended session needs the Windows-side keep-alive the parent spec's §4.2 already asks for.

## 13. Out of scope, carried forward

- The screen fallback: Mutter screencast plus a pixel click, shown to the user while it happens (2b).
- Browsers through their own automation (CDP/WebDriver); Electron apps with no tree go to the screen fallback.
- The clipboard-paste text route, and typing on the user's real seat, for apps whose document does not take insertion (Writer's body).
- Telling a phantom insertion from a real one.
- "Watch it work" on the Building card; pause and take-over.
- A button with an innocent name that sends or destroys.

## 14. The audit (2026-09-17)

Two passes over the agreed design before the plan: a complexity pass and a correctness audit of its claims against the code and the platform. What changed:

- **No `window` move, no job kind, no field on the job.** `housekeep` already is the job with no project; the audit also found no `JobKind` exists (jobs carry a `housekeeping` flag that five places key on) and that the front door's move list is its own. Reusing `housekeep` removes all of it.
- **Workers are built per action.** The engine's factory constructs fresh workers and a fresh `Executor` every step, so the id table and the bus connection live in shared state the factory captures (§5). The table is per process, ids never reused, every use re-checked; nothing needs clearing per job.
- **`classify` is pure on the action** and runs before any worker sees it, so a press carries the control's `name`, echoed by the model and verified by the worker (§3, §6).
- **`atspi` is async-only.** zbus alone, blocking, five hand-written proxies (§5).
- **A repeated press is normal**, so 1d's "just succeeded" refusal exempts `press` and `type` (§5).
- **`Done { windows }` needs `#[serde(default)]`**; nothing in `Event` has it today (§7).
- **The cap is not a setting**: `set_setting` is gated in three places for one key, and the engine does not know the context size, so `Model::context_tokens()` and a computed cap (§4).
- **`--wayland-display` exists** on GNOME Shell 50: the display name is pinned in the unit, and the log-scrape, drop-in and engine restart go (§8). The engine's environment lines belong in its unit file, which has static `Environment=` lines, not in a setup script.
- Confirmed as claimed: one accessibility bus for both displays; `gtk-launch` and `gio launch` present; the housekeeping folder and the no-blueprint rule as described.
