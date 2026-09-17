# Phase 2a — the desktop hand

Design agreed 2026-09-17 (three parts, each approved in conversation). Parent spec: `2026-09-15-ai-os-design.md` §2 decisions 8, 9 and 12, §4.4 (hands and senses, tier 2), §4.7 (the desktop worker), §5.1 (the workshop's two displays). Predecessors: `2026-09-16-phase1c-hands-and-undo-design.md` (the hands and the risk rule as code), `2026-09-16-phase1d-rail-design.md` (events, service, rail). Findings that shaped it: `../findings/phase0-findings.md` Task 4, and the probe in §9 below.

**His four calls that shaped this (2026-09-17):**
1. Phase 2 is split: 2a is the desktop hand (this design), 2b is the screen fallback. Browsers through their own automation wait.
2. One job kind covers both the window the user hands over and a window the AI opened itself: a window is a window, wherever it was opened.
3. **Nothing is per-app.** The hand is one protocol for every program; the model does the understanding, and a bigger model understands more with no code change. LibreOffice's own scripting API (UNO), which the parent spec listed as a text route, is out: it is per-app code, so if Writer ever needs it that is a learned skill (Phase 3), not hand code.
4. What the model sees of a window is capped by a setting that follows the context budget, so the deploy PC's larger model sees more without a code change.

## 1. What 2a delivers

- **The desktop worker** (parent §4.7): the model's third hand, operating program controls through the accessibility bus. Five actions: `look`, `press`, `type`, `read`, `open_app`. One protocol for every toolkit, no per-app code anywhere.
- **The window job**: a third front-door move, `window`, for "take my Writer window and …". Any job may use the hand; a project job may open an app to check its own output.
- **Decision 9 for the hand, as code**: a press on a control whose name means leaving the machine or destroying unsaved work stops at Needs-your-OK.
- **The invisible session as a unit** (`ai-os-desktop.service`), installed by a setup script like the engine's, in place of the Phase 0 hand-run script.
- **The honest undo line** on any Done card of a job that used the hand.

Not in 2a: the screen fallback (2b: Mutter screencast and a pixel click); browsers through CDP or WebDriver; the clipboard-paste text route; typing on the user's real seat where insertion does not commit; pause and take-over; "Watch it work" on the Building card; the Watch card.

## 2. The window job

A third front-door move beside `start` and `housekeep`:

```json
{"move":"window","window":"Text Editor","goal":"add a line saying reviewed and save","understood":"Taking your Text Editor window to add a line and save"}
```

`window` is the user's words for it, matched against app names and window titles from `look`. A window job has no project folder and no blueprint, like housekeeping, and its working directory is the scratch folder. The user's words are carried verbatim on the job as since 1c. If no window matches or more than one does, rule 8 applies and the job asks before acting. The move joins `allowed_moves` and the front door's schema in the same way `housekeep` did; the discriminator key stays first (decision 13).

## 3. The five actions

All five go through the accessibility bus (AT-SPI). None knows what any app is.

| Action | Fields | What it does |
|---|---|---|
| `look` | `window?`, `find?` | Without `window`: the open windows, app name and title. With one: its controls — short `id`, role, name, state flags (checked, disabled, focused, showing), and a preview of any text or value. `find` keeps only controls whose name or role contains the word. Menus appear as names; press one and look again to see its items. |
| `press` | `control` | The control's default accessibility action: a button clicks, a toggle flips, a menu opens, a menu item activates. |
| `type` | `control`, `text`, `replace?` | Inserts `text` at the end of the control's text through its editable-text interface. `replace: true` clears it first. |
| `read` | `control`, `from_line?`, `lines?` | The control's full text, windowed by line like `read_file`, for documents too long for a preview. |
| `open_app` | `name`, `visible?` | Launches a desktop application by its desktop-entry name (`org.gnome.TextEditor`, `libreoffice-writer`) in the invisible session, or on the visible display when `visible` is true. Name validated by its own rule, `^[A-Za-z0-9][A-Za-z0-9._-]*$` (desktop-entry ids carry dots and capitals), and resolved to an existing `.desktop` file under the standard application folders before anything runs. Never a shell string. |

**Control ids.** Per job, the worker keeps a table from short integer ids to (application bus name, object path). Ids are handed out by `look` and stay valid for the job; a fresh `look` re-validates the table, and an id whose object is gone reports "that control is gone; look again" rather than acting on anything else. The model never sees a bus name or an object path.

**Which windows.** The probe (§9) confirmed that windows on the WSLg display and windows in the invisible session register on the same accessibility bus, so one worker sees both and `look` lists both. `open_app` decides where a new window appears: invisible unless the user asked to see it; the front door sets `visible` from the user's words ("show me", "open … for me"). On bare metal both display names are the same and the distinction disappears (parent §5.1).

## 4. The budget

`look` returns interactive and readable controls only: buttons, toggles, check boxes, radio buttons, menus and showing menu items, entries, text areas, combo boxes, page tabs, and short labels (a status bar's "1 word, 17 characters" is a label the model needs). It skips panels, fillers, separators, and anything not showing. Controls come in tree order with the focused one first.

It stops at `look_cap` controls and ends with "N more, narrow with find". `look_cap` is a setting (`set_setting look_cap`) defaulting to the model's context budget divided by 200, never below 20: 40 on the 8k workshop, more on the deploy PC's full context. It is the one knob that lets a bigger model see more.

`read` is windowed like `read_file` and capped the same way; `type` text is summarised in the job record like a big `write_file` (1b's I4) so it cannot blow the prompt.

## 5. The worker and the dispatcher

- `DesktopWorker` in the `executor` crate, beside `SandboxWorker` and `AdminWorker`. A fourth `Lane::Desktop` in `executor::lane` for the five actions, listed and not `_`, so a sixth desktop action fails to compile until it is placed.
- It runs **inside the engine process**. The engine already runs as user `ai` on the user bus; the accessibility bus address comes from `org.a11y.Bus.GetAddress` on that bus. No new process, no new privilege, no root.
- **Dependencies:** `zbus` (blocking API) and the `atspi` crate (the Odilia screen-reader project's AT-SPI proxies, built on zbus). Named fallback if the crate's API proves unstable: hand-rolled zbus proxies for the five interfaces the hand uses — Accessible, Action, Text, EditableText, Component. Both are in-process; the choice changes nothing above the worker.
- Nothing is cached between looks except the id table. A `look` walks the window's tree fresh, bounded by node count as the Phase 0 probe was.
- **Environment:** the worker reads `AI_OS_DISPLAY_INVISIBLE` (workshop: the headless shell's `wayland-N`) and `AI_OS_DISPLAY_VISIBLE` (workshop: `wayland-0`, WSLg) and sets `WAYLAND_DISPLAY` for what `open_app` launches. The setup script writes both into the engine unit's environment; on bare metal they are the same value.
- `Executor::new` takes the third worker; `FakeWorker` serves it in tests as it does the other two.

## 6. Decision 9 for the hand, as code

`rules::classify` for the five actions:

- `look`, `read`, `type`: `Auto`. `open_app`: `Auto` when the name passes its rule (§3), `NeedsConfirm` naming the bad name otherwise, as the package rules do.
- `press`: `Auto`, unless the control's name (as `look` reported it, kept in the id table) contains a word from a fixed list, case-insensitive:
  - **leaves the machine:** send, post, publish, upload, share, pay, buy, submit, order;
  - **destroys unsaved work:** close, quit, discard, don't save, delete, revert, replace.
  Then `NeedsConfirm("press Close in Text Editor: it may throw away unsaved work")`, with the control and window named. A yes holds for that exact action for the rest of the job, a no wins over it, and neither outlives the job (1d).
- Ceiling, stated: a button called "Go" or "OK" that sends is not caught. The list is the rule the executor can apply without asking the model whether its own action is risky (decision 9's last sentence). The list lives in `rules.rs` next to the path rules and is tested there.

`open_app` does not close anything and no `close` action exists: the AI closes a window only through the app's own control, which the list catches.

## 7. Undo

Nothing done inside a window is put back by the AI. Unsaved work has no snapshot (parent §4.8, limit), and desktop actions record no reverse. Files an app saves inside a project during a project job are covered by that job's snapshot as before; a window job has no project, so a file its app saves is not covered. Every Done card of a job that used the hand carries one line: "what I did inside Text Editor can't be undone by me". The `done` event gains an optional `windows: Vec<String>` (the windows the job worked) from which the rail and the terminal lines derive that sentence; no new event kind.

## 8. The invisible session as a unit

`ai-os-desktop.service`, a user unit for `ai`, installed by `trial/setup-desktop.sh` the way `setup-rail.sh` installs the engine's:

- `gnome-shell --headless --virtual-monitor 1600x900 --wayland --no-x11`, `WantedBy=default.target`, `Restart=on-failure`.
- The environment work the Phase 0 script does by hand — `XDG_CURRENT_DESKTOP=GNOME`, `XDG_SESSION_TYPE=wayland`, the toolkit-accessibility setting, the activation environment for the portals, `graphical-session.target` raised — moves into the unit and an `ExecStartPost` script.
- The shell's display name is read from its log after start and written to the engine unit's environment as `AI_OS_DISPLAY_INVISIBLE` (a drop-in), then the engine is restarted. `ai-os-engine.service` gets `After=ai-os-desktop.service`.
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

- `executor::action::Action`: five new variants; `lane` and `classify` list them.
- `executor::Executor`: a third worker field; `execute` routes `Lane::Desktop`.
- `aios_core::moves`: `Move::Window { window, goal, understood, remember? }`; `allowed_moves`, the front-door schema and its key-order test.
- `aios_core::job`: a job carries `window: Option<String>` and the list of windows it worked; `JobKind` gains the window kind with the scratch workspace.
- `aios_core::prompt`: standing rules for the hand (§11); the housekeeping-style user prompt for a window job.
- `aios_core::engine`: a `done` with `windows`; `describe`/`lines` wording for desktop steps ("pressed Bold in Writer", "typed two lines into Text Editor", "opened Calculator").
- `aios_proto::Event::Done { windows }` (optional, default empty; old clients ignore it).
- `aios_rail::cards`: the Done card names the window instead of files when files are empty and windows are not; the undo line.
- `set_setting look_cap`: the second setting v1 knows.
- `trial/setup-desktop.sh`; `setup-rail.sh` gains the display environment.

## 11. The prompt

Standing rules in the style of the other hands', short: look before you act and again after; ids come from the latest look; text goes in with `type`, never with `run_command`; buttons and menus with `press`; `open_app` opens programs, never `run_command`; a window job's Done check is a `read` or a `look` that shows the result, because the sandbox cannot see a window. No application is named anywhere in the prompt.

## 12. How 2a is proven

1. **Executor unit tests**: `lane` for the five actions; `classify` for the word list, both halves, case-insensitive, and the free ones; the id table (hand-out, re-validation, "gone"); `look`'s filter, order and cap on a fake tree; `open_app` name validation (dots and capitals pass, slashes and spaces do not).
2. **Engine unit tests**: the `window` move through the front door; a window job's workspace and no-blueprint rule; `done` carrying `windows`; the step wording.
3. **Proto and rail tests**: `Done` with and without `windows` round-trips; the Done card for a window job.
4. **Live acceptance** (`runtime/core/tests/live_2a.rs`, `AI_OS_LIVE=1`): real qwen3.5:9b, the service on a temp socket, no human, three scripts:
   1. **The user's window.** The test opens a text file in Text Editor on the visible display. "Take my Text Editor window, add a line saying reviewed and save it." Done, and the file on disk has the line.
   2. **The AI's own window.** "Open the calculator and tell me what 12 times 34 is." The app opens in the invisible session, buttons are pressed, the result is read from the display and said. Nothing appears on screen.
   3. **The risky press.** "Take my Text Editor window and close it without saving." A Needs-your-OK naming the control, `yes`, and the window is gone from the bus.
   A failed run is fixed in the code, never in the test; every run and its cause is recorded in a Results block here.
5. **Him**: hands the rail a window of his own on the Windows desktop and watches it worked.

Test commands as in 1d (inside the distro). `cargo test -p executor` and `-p aios-core` stay headless; the live test needs the desktop unit up.

## 13. Out of scope, carried forward

- The screen fallback: Mutter screencast plus a pixel click, shown to the user while it happens (2b).
- Browsers through their own automation (CDP/WebDriver); Electron apps with no tree go to the screen fallback.
- The clipboard-paste text route, and typing on the user's real seat, for apps whose document does not take insertion (Writer's body).
- Telling a phantom insertion from a real one.
- "Watch it work" on the Building card; pause and take-over.
- A button with an innocent name that sends or destroys.
