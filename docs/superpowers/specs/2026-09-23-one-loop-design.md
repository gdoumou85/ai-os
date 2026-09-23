# One loop: the AI steers itself (v0.10.0)

The owner, 2026-09-23, after a day of fixes: *"It looks for a blueprint when a task is not
blueprint related, it assumes every prompt is about a project. This is not true AI that
understands the user, just some failed predefined behaviours. This calls for a big restructure."*

He is right about the cause. Every message is first sorted into chat, project or housekeeping,
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

No state machine: no Asking/Planning/Working, no front door, no project/housekeeping split, no
plan-step numbers, no blueprint gate, no done check, no rejection budget. `outcome` is `done`
(default) or `could_not`, so the card never says Finished over a failure.

**Answer format.** Moves stay grammar-constrained JSON (Ollama `format`, OpenAI
`response_format`), one schema with the five moves; `act.action` is today's `Action` enum
unchanged. This is what keeps the 9B reliable, and every provider in the Model and Cloud cards
already supports it. What changes is the transport: `Prompt` carries a message list
(`system`, then `user`/`assistant` turns) instead of one system and one user string, with the
screen image, as today, on the newest user-side message.

**Working folder.** Commands run in the user's home, as a terminal would open; the brief says to
use absolute paths. `SetSetting projects_root` stays, as an action.

## 2. Memory and notes

- **Project notes.** A project is a folder: every folder under the projects root, plus folders
  registered elsewhere (today's `projects` rows, and any folder where the model writes a
  BLUEPRINT.md, registered on that write). Its notes file stays BLUEPRINT.md. The notes are shown
  (capped at 3000 chars) when the user's message names the project (today's `named_in`), and the
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
- **Fitting 8k.** The engine counts (chars/4) and keeps the brief, the context block and the
  newest messages that fit the model's context less 1500 tokens for the answer. Action results
  older than the newest six are cut to 300 chars; beyond that the oldest messages drop. No
  summarising call. The chat itself is stored whole (`messages`), so a bigger context setting
  shows more of it.

## 3. What the user sees

- A turn with no actions: its reply, as a chat line (as today).
- A turn with actions: one work card under the message — the live line and elapsed time (today's
  `Busy`), each action in plain words with ✓/✗ (`Step`), the to-do list with ticks when the model
  keeps one (`Plan`, re-sent on each `todo`), and the end: Finished / Stopped / Couldn't finish
  with the model's words and the files changed (`Done`/`Stopped`/`Failed`).
- A question: the card shows it with the answer buttons (`NeedsAnswer`); typing works too.
- Gone: "Building: housekeeping", "Step 3 of 5".

Protocol: the `Event` set stays. `job_id` becomes the turn's id. `Understood` is no longer sent
(the card opens on the turn's first `Step`, `Plan`, `Busy` or `NeedsAnswer`); `Step.plan_step`
is 0; `Done.check` is `None`. `State` reports a running turn as today's busy job does. Files
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
| executor (all hands), proto, rail (small card changes), model transport (message list), cloud, machine, notes/learn, store, service (turn in place of job), find | engine.rs (2600 lines) → a loop of a few hundred; prompt.rs → brief + context block; moves.rs/schema.rs → five moves; job.rs → a turn record (id, request, actions, todo, shown notes) |

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
9. The budget: brief + fullest context block under 3500 tokens, so 8k leaves room for chat.

Then the owner runs the same chats on the VM with the local model and with a cloud model.

## 7. Rollout

Branch `feat/one-loop`. The old engine stays on master until every test above passes in CI;
then one release, v0.10.0, with the Help guide rewritten where it names jobs, projects and
housekeeping, and a line in the main design doc.
