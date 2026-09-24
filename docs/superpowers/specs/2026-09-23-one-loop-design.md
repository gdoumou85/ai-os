# One loop: the AI steers itself (v0.10.0)

The owner, 2026-09-23, after a day of fixes: *"It looks for a blueprint when a task is not
blueprint related, it assumes every prompt is about a project. This is not true AI that
understands the user, just some failed predefined behaviours. This calls for a big restructure."*

The owner is right about the cause. Every message is first sorted into chat, project or housekeeping,
and each kind runs a fixed script: ask, then plan, then act; a project must write BLUEPRINT.md;
done must carry a check; two rejected answers kill the job. Each live failure added a rule, and
the model spends its effort obeying rules instead of understanding the user. This design replaces
that engine with one loop in which the model decides, and keeps only the parts that work.

## Decisions (owner, 2026-09-23)

- **Models:** the design serves the local model (about 9B, 8k context) and the free cloud models
  equally. The small model will stumble more on hard tasks; that is accepted.
- **Memory:** the model sees the chat since the last Clear, plus the notes about what is being
  talked about. Clear is a truly fresh chat; notes, journal and projects stay.
- **Approach A:** replace the brain (engine, prompts, moves, job types); keep the hands, the
  desktop control, the store, the rail, the model transport, the installer.
- **The goal, in the owner's words:** "a local LLM running full agentic use for a variety of
  tasks, that would basically make the LLM a full user of the PC, same as any human." A review
  of this design against it added, all approved: the web as text, the full keyboard and mouse,
  programs in the background, talking to it while it works, fitting the local model (vision and
  context), and a thought before each move (§1–§3, §7).

## 1. The loop

A message from the user starts a **turn**. The model is given one conversation:

1. **The brief** (system message): who it is, full access on this machine as its user with sudo,
   its moves and actions and how each behaves (the desktop/screen instructions of today's SYSTEM
   stay — they are knowledge, not rules), and a few habits: look before you change, prove what you
   did before you say it is done, keep a project's notes current, ask only what the user's words
   leave open.
2. **The context block** (first user-side message, rebuilt each turn): *This machine* (today's
   machine map), *Where things are* (home, projects root, projects with folders, the scratch
   folder), standing instructions, the notes about this message (§2).
3. **The chat since the last Clear**: user messages, the model's replies and questions, and one
   compact line per action it took with its result.

Then, until the turn ends:

```
loop:
  stopped?            -> end: Stopped
  actions >= 100?     -> end: Couldn't finish ("100 actions without finishing")
  new user messages?  -> they join the conversation now ("the user adds, while you work: …")
  move = model(conversation)
    not a move        -> once more with the reason; twice -> say so plainly, end
  match move:
    reply {text, outcome}  -> end. No actions this turn: a chat line.
                              Actions: the work card closes (Finished / Couldn't finish).
    ask {question, options}-> end, the card shows the question and buttons.
                              The answer is simply the user's next message.
    todo {items}           -> the card's list is replaced; continue.
    act {action}           -> the hand runs it; its result joins the conversation; continue.
    remember {text}        -> a standing instruction, kept only if the user's own words ask
                              to keep something (asks_to_keep, as today); continue.
```

`ask` carries one question and up to five suggested answers.

Every move starts with `thought`: one short sentence of reasoning before the choice (small
models choose better after it). It is the card's live line while the move runs, and is kept in
the conversation only for the newest move.

No state machine: no Asking/Planning/Working, no front door, no project/housekeeping split, no
plan-step numbers, no blueprint gate, no done check, no rejection budget. `outcome` is `done`
(default) or `could_not`, so the card never says Finished over a failure.

**Answer format.** Moves stay grammar-constrained JSON (Ollama `format`, OpenAI
`response_format`), one schema with the five moves (plus `learn`, used only by the learning turn); `move` stays the first key of every move and `thought` the second — the runner's grammar commits to a move by its first key; `act.action` is today's `Action` enum
with §1b's new actions. This is what keeps the 9B reliable, and every provider in the Model and Cloud cards
already supports it. What changes is the transport: `Prompt` carries a message list
(`system`, then `user`/`assistant` turns) instead of one system and one user string, with the
screen image, as today, on the newest user-side message.

**Working folder.** Commands run in the user's home, as a terminal would open; the brief says to
use absolute paths. `SetSetting projects_root` stays, as an action.

A turn lives in memory only: a restart mid-turn leaves the chat as it was, and the user says it again. A job left open by v0.9.x is closed at start with a line in the chat.

### 1b. The hands: everything a person at this PC does

Today's actions stay (`run_command`, `read_file`, `write_file`, `edit_file`, `set_setting`,
`look`, `press`, `type`, `read`, `open_app`, `screen_look`, `screen_click`, `screen_type`). New:

| Action | What it does | Where |
|---|---|---|
| `web_read {url, from_line?}` | The page as plain text (scripts, styles and tags dropped, links kept as `text <url>`), paged like `read_file`, with its title. | executor, HTTP GET (ureq, already a dependency of core) and a small tag stripper |
| `web_search {query}` | Up to 8 results: title, address, snippet. | executor, DuckDuckGo's HTML page; `ponytail:` one engine, scraped — a changed page breaks it, and the failure says so |
| `key {keys}` | A key or combination on the visible screen: `ctrl+s`, `alt+tab`, `escape`, `down`, `ctrl+shift+t`. | screen session, `NotifyKeyboardKeysym` (modifiers down, key, all up) |
| `scroll {cell, spot, direction, amount}` | Wheel at a point. | screen session, `NotifyPointerAxisDiscrete` |
| `drag {from: cell/spot, to: cell/spot}` | Press, move, release. | screen session, button down → motion → up |
| `start {name, argv}` | A program left running in the background (a server, a download, a long build); its output goes to a log. | executor; process group, log in the scratch folder |
| `output {name, lines?}` | The newest lines of that program's output, and whether it still runs (exit code if not). | executor |
| `stop {name}` | Ends it (TERM, then KILL after 5 s). | executor |
| `wait {seconds}` | Waits (at most 300), then says what finished meanwhile. | engine; Stop cuts it short |

Background programs outlive the turn — a server keeps serving — and the context block lists
them ("Running in the background: devserver (since 14:02), …"). `run_command` keeps its 30-minute
limit for things that finish; the brief says to `start` anything that keeps running.

## 2. Memory and notes

- **Project notes.** A project is a folder: every folder under the projects root, plus folders
  registered elsewhere (today's `projects` rows, and any folder where the model writes a
  BLUEPRINT.md, registered on that write). Its notes file stays BLUEPRINT.md. The notes are shown
  (capped at 1500 chars in the context block, 2000 on first touch) when the user's message names the project (today's `named_in`), and the
  first time in a turn an action touches that folder, appended to that action's result.
- **Notes reminder.** A `reply` ending a turn that wrote files inside a project folder without
  writing its BLUEPRINT.md gets one note back ("you changed <project> but not its notes") and the
  model moves again; the second reply ends the turn whatever it says. Never a block.
- **Journal.** Unchanged from v0.9.5: one engine-written line per turn that took an action; the
  context block carries the lines that share a telling word with the message (at most five).
- **Standing instructions.** Unchanged (`instructions` table, `remember`, Clear wipes them).
- **Skill tips.** `notes::for_job` picks tips by the message's words (skills = the notebooks
  whose name the message mentions); the learning turn after a turn with actions stays as it is,
  fed the turn's actions instead of a job's steps.
- **Fitting 8k.** The brief is under 1800 tokens and the fullest context block under 3500 (tests hold both; tips and the request are cut to 1500 chars inside it); at 8k the worst case leaves about 1900 tokens of chat, which is why the Model card recommends 16k. The engine counts (chars/4) and fits the rest into the model's context less 1500
  tokens for the answer. Never dropped: the brief, the context block, the message that started
  this turn with anything the user added while it worked, and the newest to-do list. Then the
  newest messages, newest first; action results older than the newest six are cut to 300 chars;
  the oldest go first. No summarising call. The chat itself is stored whole (`messages`), so a
  bigger context setting shows more of it. The Model card says, under the context bar, that 8k
  is the floor and 16k or more is what a full agent wants when the PC can hold it.
- **A model that cannot see.** The screen actions (`screen_look`, `screen_click`, `screen_type`,
  `scroll`, `drag`) are offered only to a model that takes images: Ollama's `/api/show`
  `capabilities` has `vision`; with the Cloud switch on, the screen is closed for now (the pool
  picks its account per call, after the grammar is built); a cloud model that can see is a
  follow-up. Without them the schema drops those actions and the brief says why: the desktop is
  worked through `look`, `press`, `type`, `key` and the command line. The Model card's list stops filtering out models named `vision`/`-vl`/`vlm`, so one that can see can be chosen.

## 3. What the user sees

- A turn with no actions: its reply, as a chat line (as today).
- A turn with actions: one work card under the message — the live line and elapsed time (today's
  `Busy`), each action in plain words with ✓/✗ (`Step`), the to-do list with ticks when the model
  keeps one (`Plan`, re-sent on each `todo`), and the end: Finished / Stopped / Couldn't finish
  with the model's words and the files changed (`Done`/`Stopped`/`Failed`).
- A question: the card shows it with the answer buttons (`NeedsAnswer`); typing works too.
- Talking to it while it works: a message sent mid-work shows in the chat as always and joins
  the work at its next step — no more "I'm busy". "stop" still stops.
- Gone: "Building: housekeeping", "Step 3 of 5".

Protocol: the `Event` set stays. `job_id` becomes the turn's id. `Understood` is no longer sent
(the card opens on the turn's first `Step`, `Plan`, `Busy` or `NeedsAnswer`); `Step.plan_step`
is 0; `Done.check` is `None`. `State` reports a running turn as today's busy job does.
`Busy` is no longer sent for a `say` during work: the service hands the text to the running
turn's inbox (a channel the engine drains between actions) and echoes it with `You`. Files
changed are the paths of the turn's successful writes and edits.

## 4. What the engine still enforces

1. Stop (flag checked between actions, as today).
2. The action cap: 100 per turn.
3. Loop catch: the same action with the same failure three times in a row → the result says so
   ("this has failed the same way three times: do something different"); a fifth → end,
   Couldn't finish, with the failure.
4. Honest hands, unchanged in the executor: no-shell lines sent back, cut output marked, sizes
   on writes, `sudo tee` fallback.

## 5. Code

| Stays | Replaced |
|---|---|
| executor (all hands, plus §1b's new ones), proto, rail (small card changes; Model card: who can see, the 16k note), model transport (message list), cloud, machine, notes/learn, store, service (turn in place of job; an inbox for mid-work messages), find | engine.rs (2600 lines) → a loop of a few hundred; prompt.rs → brief + context block; moves.rs/schema.rs → five moves; job.rs stays as the turn's record (`Job::turn`), since learn.rs reads it; its job-machine fields go unused and are removed in a follow-up |

Kept helpers: `places`, `journal_line`/`journal_for`, `asks_to_keep`, `named_in`, `is_stop`,
`sanitize_project_name`, the machine block, `describe`/`doing`. The old `core_jobs` rows are
ignored; a job open at upgrade is dropped with a line in the chat.

## 6. Tests

Scripted-model tests (FakeModel), one per real chat from 2026-09-23, plus the limits:

1. "show me my projects" → "delete it" → "where is it": the second message's conversation has
   the first; the delete runs `rm -rf` on the project's folder; "where is it" is answered from
   *Where things are*.
2. "create a new project Flappy", then the game description: no housekeeping, no blueprint gate;
   the second message sees the first; a BLUEPRINT.md written in a new folder registers it.
3. Clear, then "delete any flappy plans": the journal line with Flappy's paths is in context.
4. "hi": a chat line, no card, no journal line, no old work in context.
5. Stop mid-turn: Stopped, chat kept.
6. A chat too long for 8k: the brief and the context block survive, old results are cut, the
   oldest messages drop, the newest stay.
7. Loop catch at three and five; the cap at 100.
8. The notes reminder: once, then the reply ends the turn.
9. The budget: the brief under 2000 tokens, the fullest context block under 1500.
10. Trimming never drops the turn's first message, what the user added mid-work, or the newest
    to-do list.
11. A message sent mid-work reaches the next prompt, and "stop" mid-work stops.
12. A model without vision is offered no screen actions and the brief says so.
13. The hands: `web_read` strips a saved page to text; `web_search` parses a saved results page;
    `start` → `output` → `stop` on `sh -c 'echo hi; sleep 60'`; `key` parses `ctrl+shift+t`
    into keysyms; `wait` returns early on Stop.

Then the owner runs the same chats on the VM with the local model and with a cloud model.

## 7. What a local model will still find hard

Said plainly so the owner's tests are read fairly: a 9B at 8k handles errands, files, commands,
installs and short desktop tasks; long multi-file projects and long screen work are where it
loses the thread. More context (16k+) and a model that can see move that line most; a cloud model
moves it furthest. The design does not change with the model — only what it can finish.

## 8. Rollout

Branch `feat/one-loop`. The old engine stays on master until every test above passes in CI;
then one release, v0.10.0, with the Help guide rewritten where it names jobs, projects and
housekeeping, and a line in the main design doc.
