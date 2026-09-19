# Phase 3 — skills — design

2026-09-19. Approved in conversation with the owner. The parent is `2026-09-15-ai-os-design.md`
(rule 3, §4.5, §4.6, phase table row 3). Acceptance cases come from the owner's own runs
(memory: skills-findings).

## Why

The AI re-learns the same things every job. The owner watched it spend a long time working out how
to open the browser, and expects the second time to be quick. Beyond the computer's basics he
wants the AI to get *better at crafts* — coding, web design, Blender — so that "make a round object
in Blender" is known, not rediscovered.

## Decisions (the owner's)

1. **A skill is a craft, not a trick.** Each skill is a *notebook* ("Blender", "Web design",
   "Python coding") that grows with every job in that area. Entries are short: a **technique** (how
   to do X, from steps that worked), a **pitfall** (what failed and what fixed it), or **taste**
   (what the owner liked or rejected).
2. **The computer's basics are one special notebook, "This computer"** — how to open the browser,
   where things are. Same machinery; read on every job.
3. **Reuse is a tip sheet, not a replay.** The AI is shown what worked and still takes each step
   itself, so a different website or a moved button does not break it.
4. **What it reads stays small however big the notebooks grow.** One line per topic, the code
   replaces by topic (no duplicates), only the most-used and the matching entries go into a turn,
   a notebook holds at most 200 entries and the least-used goes first.
5. **Wrong entries do not survive.** A tip that led to a failure is marked, shown as such, and the
   AI must fix or remove it.
6. **A skill entry may point to "This computer" entries** ("make a round object" → "start
   Blender"). Opening the skill brings the pointed-to entries along; a changed entry updates every
   skill pointing to it by topic; a deleted one marks the pointing entries "needs checking".
7. **He sees it and can delete it:** "Learned: …" on the Done card and a **Skills** button in the
   chat window.
8. **No hand-coded shortcuts** (his call, 2026-09-19): the AI learns the basics itself.

Carried from the parent design: only proven knowledge is kept (after a passed check), techniques
are built from the executor's record of the steps that ran, not the model's account, and an entry
whose steps change the machine is shown to the owner before it is kept.

## 1. Storage (`core::notes`, one new table in the engine's database)

```sql
CREATE TABLE IF NOT EXISTS notes(
  notebook TEXT NOT NULL,          -- "this computer" or a skill name, lower case, trimmed
  topic TEXT NOT NULL,             -- lower case, trimmed; the key inside a notebook
  kind TEXT NOT NULL,              -- technique | pitfall | taste
  text TEXT NOT NULL,              -- one line, ≤ 200 chars
  steps TEXT NOT NULL DEFAULT '',  -- the compact actions that proved it, from the job record
  links TEXT NOT NULL DEFAULT '[]',-- JSON list of "this computer" topics it points to
  uses INTEGER NOT NULL DEFAULT 0,
  failed INTEGER NOT NULL DEFAULT 0,       -- 1: led to a failure last time it was used
  needs_check INTEGER NOT NULL DEFAULT 0,  -- 1: a topic it points to was removed
  updated_at INTEGER NOT NULL,
  PRIMARY KEY(notebook, topic));
```

- `put` replaces by (notebook, topic), keeping `uses`, clearing `failed`/`needs_check`.
- After a `put`, a notebook over **200** entries drops the one with the fewest uses, oldest first.
- Removing a "this computer" topic sets `needs_check` on every entry whose `links` names it.
- Names are cut to 60 chars. The front door lists at most 20 notebooks ("this computer" first,
  then by total uses); the Skills screen lists all.
- An entry waiting for the owner's Keep (§5) lives in a separate table, `notes_pending` (same
  columns plus the job id): it never hides, replaces or evicts a kept entry, and only Keep moves it
  into `notes` through the normal `put`. *(As built: the first draft marked pending rows in `notes`
  itself, and a discarded proposal could delete the kept entry it replaced.)*

Limits live as constants beside the code (`MAX_ENTRIES = 200`, text ≤ 200 chars, steps ≤ 400
chars). The Settings screen (memory: settings-screen) may expose them later; not now.

## 2. Picking notebooks at job start

`start` gains `skills: [string]` (0–3 names, optional, `serde(default)`). `housekeep` does not: a
housekeeping job reads only "This computer". The front-door prompt lists the existing notebook
names (names only) and says: "name the skill areas this job belongs to, from this list, or a new
short name for a craft not listed; none for a plain errand". Names are lowered and trimmed; the job
stores them (`Job::skills`, `serde(default)`).

## 3. What a job turn reads

`prompt::job_turn` gets a block **"What you learned before (tips, not orders — check them):"**
built by `notes::for_job(goal + request, skills)`:

- **This computer:** the 15 most-used entries, plus up to 5 more whose topic or text shares a
  word (≥ 3 letters, stop words dropped) with the goal and the user's request.
- **Each named skill:** its 10 most-used, plus up to 5 matching, plus every "this computer" entry
  its shown entries link to (not already shown).
- Each line: `[notebook] topic: text` then, for a technique, `(did: <steps>)`. A failed entry ends
  with `(FAILED last time — fix or remove it)`, a needs-check one with `(needs checking)`.
- The whole block is capped at 3000 chars; lines beyond it are dropped, least-used first.
- The entries shown are remembered on the job (`Job::shown_notes`, `serde(default)`) so the
  learning turn can ask about them.

ponytail: word overlap is the search; embeddings if it misses too often.

## 4. The learning turn

A new move, **`learn`**, legal only in a learning turn:

```json
{"move":"learn",
 "entries":[{"notebook":"this computer","topic":"open a website","kind":"technique",
             "text":"open_app firefox with the address as the name","steps":[7],"links":[]}],
 "used":["this computer/open a website"],
 "wrong":["blender/add a sphere"],
 "remove":[]}
```

**After `done` passes** (`finish` with `State::Done`, jobs and housekeeping alike): the engine
emits `Done` as today, then runs one extra model call with `prompt::learning_turn` — the goal,
the numbered steps of this job (compact, all of them, ok/failed), the entries shown, and the
question: *what did you learn that you would want to know straight away next time, which tips
did you use, which were wrong? Nothing new → empty entries.* Allowed moves: `learn` only.

The engine then applies it, trusting the record over the model:

- **technique:** must cite ≥ 1 step that exists and is `ok`; `steps` is filled from those steps'
  `compact_action`, never from the model's words. No valid step → dropped.
- **pitfall:** must cite a failed step and a later ok step; stored as text + the ok steps.
- **taste:** no steps; allowed only when the job had the user's words in it (request or answers).
- `links` keep only topics that exist in "this computer" (or are added in the same move).
- `used` → `uses += 1` for entries in `shown_notes`; `wrong` → `failed = 1`; `remove` → deleted,
  only for entries that were shown (the AI can't remove what it didn't see).
- Anything invalid is dropped silently; a learning turn never fails the job and never retries.

**After a failed or given-up job:** the same turn with a shorter question; only `wrong` and
`used` apply (no new entries — nothing was proven). A cancelled job gets no learning turn.

The learning turn is bounded to one model call. If the model is unreachable, nothing is learned.
After a failed job that was shown no tips there is nothing it could change, so no call is made.

## 5. Entries that change the machine

A technique whose cited steps include `install`, `remove`, `service`, `set_setting`, or a
`write_file`/`edit_file`/`make_dir` outside the job's folder is stored in `notes_pending`
and shown on the Done card — topic, text and the steps it did — with **Keep** / **Discard** (the
fixed words `keep what you learned` / `discard what you learned`). Proposals left from an earlier
job are discarded when the next learning turn starts, and only the newest card keeps the buttons.

## 6. Protocol and the chat window

- `Event::Learned { job_id, lines: Vec<String>, pending: bool }` after every learning turn — empty
  when nothing changed, so the chat window's spinner keeps turning until the learning turn is over
  (a slow model can take minutes for it). The rail appends "Learned: …" lines to that job's Done
  card, with Keep/Discard when `pending`.
- `Request::Skills` → `Event::Skills { notebooks: Vec<Notebook> }` (name, entries with kind,
  topic, text, uses, failed, needs_check).
- `Request::Forget { notebook, topic }` → a fresh `Event::Skills`. Both are answered on the
  client's own thread from its own database connection, so the screen opens while a job runs.
  Keep/Discard are the fixed words above, not requests.
- **Skills button** in the title bar beside Model and Help: a card listing notebooks, "This
  computer" first; clicking one lists its entries (topic, text, "used N times", a red mark when
  failed or needs checking) each with ✕.
- The Help guide (`rail/src/guide.txt`) gets a "Skills" section in the same commit (memory:
  guide-stays-current).

## 7. Tests (CI, no model)

- `notes`: put replaces by topic and keeps uses; cap drops the least-used oldest; removing a
  linked computer topic flags the linking entries; pending entries hidden from `for_job`.
- `for_job`: most-used + matching selection, linked computer entries pulled in, failed and
  needs-check markers, the 3000-char cap.
- Learning apply: technique steps come from the record (a model-written `steps` text is ignored),
  a technique citing a failed or missing step is dropped, pitfall rules, taste rules, `remove`
  limited to shown entries, failed jobs apply only `wrong`/`used`, admin steps make it pending.
- Engine with the scripted model: a job that passes then learns emits `Done` then `Learned`; a
  second job's first turn contains the entry; a `learn` outside a learning turn is rejected.
- Schema: `learn` in `MOVE_SCHEMA` with `move` first; the move-names test updated.

## 8. Live acceptance (the owner's Ubuntu VM)

1. "Open the browser and go to <a website>" — may be slow. The Done card says "Learned: …" and
   the Skills screen shows it under "This computer".
2. Same request with another website — goes straight there (few steps), and the entry's use count
   goes up.
3. Later, in Blender: "make a round object" twice — a "Blender" notebook appears, pointing to
   "start Blender", and the second run is shorter.

## Out of scope

Replaying steps without the model; sharing notebooks between machines; retraining the model from
notebooks (Phase 6 may use them); editing entries by hand (delete only); steering a running job.
