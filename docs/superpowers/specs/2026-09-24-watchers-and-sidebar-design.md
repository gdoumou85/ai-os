# Watchers, alerts and the sidebar (v0.11.0, v0.12.0)

2026-09-24, with the owner. Until now the AI works only while it answers a message. The owner
wants it to watch things (prices, a website, a folder, emails, the clock), be woken when something
happens, and act on it, for any kind of work, not only trading. Every alert carries the reason it
exists, written by whoever made it (the owner or the AI itself), so the AI knows what it was about
when it fires. The owner can see every watcher and switch any of them off. The rail gets a left
sidebar to hold this and what already lives in pop-up windows.

Two releases: **v0.11.0**, the sidebar with what exists today; **v0.12.0**, watchers and alerts.

## 1. Watchers

A watcher is a row in the store, table `watchers`:

| column | meaning |
|---|---|
| `name` | unique, letters, digits, `-`, `_`, spaces (shown as is) |
| `reason` | a note from its maker to whoever handles the alert: what it is for, what the plan was. Required, never empty |
| `urgent` | 0/1: an urgent alert pauses the work in hand (§2) |
| `made_by` | `you` or `AI`, with the words of the request it was made in (cut to 120 chars) |
| `kind` | `timer`, `check` or `push` |
| `every_s` | timer and check: seconds between runs (timer ≥ 60, check ≥ 60) |
| `daily_at` | timer only, instead of `every_s`: `HH:MM`, local time |
| `argv` | check and push: the command, JSON array of strings |
| `paused` | 0/1 |
| `next_at` | ms: when a timer or check is next due |
| `last_fired_at`, `last_text` | the last alert |
| `fails` | a check's failures in a row |
| `created_at` | ms |

The kinds:

- **timer**: when due, it fires with the text `it is <HH:MM>`.
- **check**: when due, AI OS runs `argv` under `timeout 60`, in the user's home, as the machine
  hand runs any command. Exit 0 with output: it fires, the output (cut to 1500 chars) is the
  alert's text. Exit 0 with no output: nothing happened. Any other exit: `fails += 1`; at 3 the
  watcher is paused and the chat says why (the last error, cut short). A success sets `fails = 0`.
- **push**: `argv` is a program that runs on its own and calls `ai-os-alert "<name>" "<text>"` when
  something happens. AI OS starts it through `executor::programs` under the name `watch-<name>`,
  and each scheduler tick restarts it if it is not running, at most once a minute; after 5 restarts
  in 10 minutes it is paused and the chat says why, with the end of its output.

**The scheduler** is a thread in the service, started with it. Every 15 seconds it opens its own
store connection, finds the timers and checks that are due and not paused, advances their
`next_at`, runs the checks (one at a time, never on the engine thread), keeps push programs alive,
and hands each alert to the engine thread (§2). A restart loses nothing: rows persist, `next_at`
in the past means due now, and a timer missed while the machine was off fires once, not once per
missed period.

**Throttle**: an alert from a watcher less than 2 minutes after its last one is not handled; it is
counted, and the next handled alert says `(and N more since the last)`. `ai-os-alert` for an
unknown or paused watcher is refused with a line saying so.

**`ai-os-alert <name> <text>`**: a new small binary next to `ai-os-chat`. It sends
`Request::Alert { watcher, text }` and exits with 0 on accepted, 1 with the reason otherwise.

**The AI's hand**: two new engine-lane actions, like `set_setting`:

- `watch name reason urgent when command`: creates the watcher or replaces the one of that name.
  `when` is words a small model writes easily: `every 5 minutes`, `every 2 hours`, `daily 08:00`,
  or `live`. No `command`: a timer (not with `live`). A `command` with `every`: a check. A
  `command` with `live`: a push. Anything else is refused with what is allowed. `made_by` is `AI`
  and the pinned request.
- `unwatch name`: deletes it (and stops its push program).

The owner asking ("warn me every morning about the weather") is still the AI using `watch`; the
engine sets `made_by` to `you` when the turn's request is the owner's own words and the owner
asked to be warned, reminded or watched for (the words `watch`, `warn`, `remind`, `alert`,
`notify`, `tell me when`, `every`); otherwise `AI`.

## 2. When an alert fires

The alert becomes a turn of its own:

- **Its own chat.** `messages` gains `chat TEXT NOT NULL DEFAULT 'main'`. The owner's conversation
  is `main`; an alert turn's is `alert-<id>`. Every read and write of messages takes the chat,
  and an alert chat is deleted when its turn ends (its journal line stays). Clear forgets `main`
  and any alert chat; a restart forgets all.
- **The pinned request**: `An alert woke you. Watcher: <name> (made by <made_by>; urgent|not
  urgent). What happened: <text>. Its reason: <reason>. After acting on it, bring its reason up to
  date with watch, or unwatch it if its job is done.` A project named in the reason or the name
  brings its notes, as a message naming it does.
- **Not urgent**: the engine thread takes alerts from a queue between turns, after the owner's
  queued words: the work in hand finishes first.
- **Urgent**: a second flag beside Stop, `yield`, lands at the same points Stop does. The turn is
  parked (its Job with request, to-do list and steps, kept in memory), the alert turn runs, and
  the parked turn continues in `main` with a result line `(an urgent alert was handled in between:
  <watcher>; carry on)`. A Stop while parked ends the parked turn too.
- **The owner's words during an alert turn** do not join it: they wait for `main` (the inbox is
  closed for alert turns; they queue as commands).
- **One at a time.** Alerts queue in order; an urgent one goes ahead of non-urgent ones.

**Telling the owner**:

- `Event::Alert { job_id, watcher, text, reason, urgent }` opens an orange **Alert card** in the
  chat: the watcher, what happened, the reason. The turn's steps and closing line fill that card
  as the Working card does today.
- A **desktop pop-up**: the service runs `notify-send -a "AI OS" -h
  string:desktop-entry:org.aios.Rail "<watcher>" "<text>"`, so it shows even with the rail closed,
  and a click opens the rail.
- A **count** on Watchers in the sidebar of alerts since the owner last opened that page.

## 3. The sidebar (rail, layout A)

A left menu, about 150 px: 💬 Chat, 📁 Projects, 🔔 Watchers (v0.12.0), 🧠 Skills; at the bottom ⚙
Model, ☁ Cloud (a switch, as now), ? Help. The header keeps Clear and Stop. The main area shows the
page picked; Chat is the page at start and on any new card.

- **Chat**: as today; Alert cards in orange.
- **Projects**: every project the engine knows (`Request::Projects {}` → `Event::Projects`), each
  with its folder, the first line of its BLUEPRINT.md and when its folder last changed. **Open
  folder** runs `gio open <folder>`. Clicking the name puts `let's continue <name>` in the entry.
  The project list moves from the engine into `core::projects` so the service's client thread can
  answer it, as it answers Skills.
- **Watchers** (v0.12.0): `Request::Watchers {}` → `Event::Watchers`; a row each: dot (red urgent,
  green on, grey paused), name, when (`every 5 min`, `daily 08:00`, `live`), made by, last fired.
  A click opens its reason and last alert, with **Pause/Resume** (`Request::WatcherPause { name,
  paused }`) and **Delete** (`Request::WatcherDelete { name }`). The service broadcasts
  `Event::Watchers` after any change, so every client stays current. Opening the page clears the
  count.
- **Skills**: the Skills window's content, as a page.
- **Model**, **Help**: the model dialog and the guide, as pages.

main.rs is 682 lines; each page goes in its own file under `rail/src/pages/`.

## 4. The AI's brief

Under the actions: `watch` and `unwatch` as above, and one habit: `An alert is your earlier self
or the user asking to be woken for something: read its reason and act on it, then keep the reason
current.` The brief stays under its 1800-token test.

## 5. Tests

- Engine, with a fake clock and a fake runner: a timer fires when due and once after a long gap; a
  check's output fires, no output does not, 3 failures pause it; the throttle counts; `watch`
  parses every `when` form and refuses the rest; `made_by`.
- Engine: an alert turn uses its own chat and leaves `main` as it was; a non-urgent alert waits for
  the turn in hand; an urgent one parks it and the parked turn resumes with its to-do list and
  steps; Stop while parked ends both.
- Service: `ai-os-alert` accepted, refused for unknown and paused; the owner's words during an
  alert turn wait for `main`; Pause and Delete from a client stop a watcher and every client gets
  the new list.
- Rail (`cards.rs`): Alert cards; the page lists render from their events.
- The owner's acceptance on the VM: "every 2 minutes tell me the time" (a timer), "warn me when a
  file appears in Downloads" (a check), then Pause from the Watchers page and see it stop.

## 6. Out of scope

A phone notification (email, Telegram): later, as its own feature. A credential vault and an
approval step for money: not in this design; the trading use waits for them.
