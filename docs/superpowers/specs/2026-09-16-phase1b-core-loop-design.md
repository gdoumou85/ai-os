# Phase 1b — Core loop, model connection, conversation and memory

**Date:** 2026-09-16
**Status:** Design agreed in brainstorming; not built.
**Parent:** `2026-09-15-ai-os-design.md` (v3). This document is the sub-design for Phase 1b of Phase 1 (Foundation). It builds on the Phase 1a executor spine (`runtime/executor/`) and changes nothing in it except the carry-forward in §7.

---

## 1. What Phase 1b delivers

The executor from 1a is a pair of safe hands with nobody attached. Phase 1b attaches the brain and the mouth:

- The user **talks in plain language**; the local model works out what the message is (chat, new work, existing project, an answer, an approval, a cancel).
- Work runs as a **job**: the model asks its questions first, writes a plan, then proposes one action at a time; every action goes through the executor's door; the result goes back to the model; it repeats until the job is **proven** done by a check the model itself proposes.
- Everything the model needs to remember lives **outside the conversation**: standing instructions, a per-project blueprint, and the job record.
- The **model connection** is a swappable part behind a small interface (Ollama now; remote-local and cloud pool later; a fake for tests).

**Out of scope for 1b** (next sub-phases): installing software and the admin worker (1c), undo snapshots (1c), the docked rail (1d), program controls / screen (Phase 2), the ability map and search (Phase 3).

**Visible result in 1b:** a plain chat in the workshop (a builder's front; the rail in 1d draws the same conversation as cards). The acceptance test is the real local model completing this job end to end with no human help: *"make a script that prints the first ten prime numbers, and prove it runs."*

---

## 2. A conversation is the only way in

Every message the user types is sent to the model with:

1. the **standing instructions** (§6.1),
2. the **project list**: name, folder, one-line description, last touched,
3. **what is going on now**: a running job's state, or a question / approval the model is waiting on,
4. the **last exchange or two** of the conversation (for "what were we just talking about"), nothing older.

From that, the model decides what the message is. This is the "understand" stage of the spec's loop (parent §4.1), done at the front door:

| The user says… | The model understands | What happens |
|---|---|---|
| "what's a prime number?" / "thanks" | chat | replies in plain language; no job, nothing runs |
| "make me a script that prints the first ten primes" | new work | creates a project and a job; asks its questions if it has any; then works |
| "add a menu to the primes program" / "continue yesterday's thing" | existing project | finds the project from the list and starts a job **inside that project's folder** |
| "ten is fine" (while a question is pending) | an answer | saves it on the job; the job resumes |
| "yes, send it" (while an approval is pending) | an approval | releases that one action, nothing else |
| "stop" / "leave it" | cancel | stops the current job |

**The model always says what it understood before acting** ("Starting a new project *primes* — I'll write the script and run it"), so a misreading is corrected by the user in one line. A 9B model will misread sometimes; the evaluation (parent §6) counts how often, and a bigger model does it less.

---

## 3. A job's life

A job is a row in the database: goal, project, mode, the user's saved answers, the plan, the steps done with each result, and its state.

```
new → asking → planning → working → checking → done
                     any state ──► waiting_for_user (question or approval) ──► back where it was
                     any state ──► failed (gave up, with the reason)
                     any state ──► cancelled (the user said stop)
```

**Modes**, chosen per job from the user's words:
- **ask** (default): the model asks what it needs to know before planning, and may ask again mid-job when a new question arises. Answers are saved on the job and re-fed in every model call.
- **creative** ("just do what you think best"): the model is told not to ask and to make its own reasonable choices. Risky-action approvals still apply — that is a safety rule (decision 9), not a scope question.

**One turn of the loop:** build the prompt from the saved job → ask the model for exactly one move (§4) → carry it out → save → repeat. The prompt is trimmed to the 8k budget (parent §4.3): goal, answers, plan, blueprint, and the **last few steps** with their results (results already cut to 500 characters by the executor); older steps are summarised as "step N done / failed".

**A failure is told in full, once, and never retried as-is.** A step that fails once will fail again, so the first failure hands the model the whole reason — for commands, the exit code and the *end* of the error output, where the reason lives (the executor's 500-character cut is taken from the tail for failures, not the head). The model must then do something **different**; the loop refuses an action identical to one that already failed on this job ("that exact action already failed with: … — work around it"). **Giving up is a fixed count, not the model's mood:** 3 *different* failed attempts at the same plan step, or 25 steps in total, and the job stops as *failed* with a plain-language reason.

**Crash and restart:** because every turn is saved before the next begins, a job in any state resumes from the database — including one waiting for the user.

---

## 4. What the model is allowed to say

Every model answer is forced into one JSON shape (grammar-forced, decision 13), so it always parses. Exactly one move per turn:

| Move | Payload | The loop does |
|---|---|---|
| `reply` | text | says it to the user; no job change |
| `start` | project (new name, or an existing one) + goal + mode + what it understood | creates the job; if mode is ask, the next turn must be `ask` or `plan` |
| `ask` | 1–3 questions | job → *waiting_for_user*; the user's next message is the answer |
| `plan` | 2–8 numbered steps in plain words | saves the plan; job → *working* |
| `act` | one executor `Action` (run_command / read_file / write_file / http_post), plus which plan step it serves | runs it through `Executor::execute`; saves the outcome; a blocked action → *waiting_for_user* with the reason |
| `done` | summary for the user **plus a check action** | runs the check; ok → *done*; not ok → the model is told the check's output and the job stays *working* |
| `replan` | the revised remaining steps + why | replaces the rest of the plan; job stays *working*. This is how "I can't do it this way" turns into "then this way". |
| `give_up` | reason **and what was missing** (a tool, a package, a permission, information) | job → *failed*; the missing thing is recorded so the next phase can act on it (1c installs it; Phase 3 learns the workaround) |

**Rules enforced by the loop, never left to the model:**
- `done` without a check is rejected and the model is told to include one.
- `act` before `plan` is rejected.
- `ask` in creative mode is rejected and the model is told to decide itself.
- An `act` identical to one that already failed on this job is rejected with the earlier failure's reason (§3).
- The blueprint (§6.2) is the map: the model reads it to find **what** to change and **where**, makes the change, then **updates the blueprint in the same loop** to record that change as done. Enforced: `done` is refused while the blueprint is older than the last code change ("update BLUEPRINT.md for what you just changed"), and the model is reminded right after each code-changing action.

**Action kinds in 1b:** the four the executor already has (run_command / read_file / write_file / http_post) plus one new hand, because a local model on everyday hardware must **edit code in place, never read a whole file, hold it in its head and write it all back**:

- `edit_file { path, find, replace }` — replaces one exact passage of a file. The passage must occur **exactly once**, or the edit is refused and the model is told (zero matches: "not found, re-read the file"; several: "ambiguous, include more surrounding lines"). Same workspace jail and same risk class as `write_file`.
- `read_file` gains an optional line window (`from_line`, `lines`) so the model reads the part it needs, not the first 500 characters of everything. Whole-file reads stay capped.

`write_file` is for new files; the model's instructions say so. The install and package-fetch hands are 1c (§8).

---

## 5. The model connection

One small interface: *given these messages and this answer schema, return one move.* Implementations:

- **Ollama** (workshop and first product engine): HTTP on this machine, `/api/chat`, the move schema sent as `format`, thinking off, temperature 0 — the settings Phase 0 measured at 35–40 tok/s and 100% parse. Model tag from configuration (`qwen3.5:9b` in the workshop).
- **Fake** (tests): returns scripted moves; no GPU; deterministic.
- Later, behind the same interface: a remote-local model on pc-worker, and the free-cloud pool with quota failover (parent §4.3). The loop never knows which one it is talking to. Keys, when they exist, are held by the executor, never in the prompt.

---

## 6. Memory — nothing lives in the chat

The model cannot afford a long history on an 8k budget, and the design does not pretend it has one. The model's instructions say so: *"If it is worth remembering, write it down; you will not see this conversation again."* Three places remember:

### 6.1 Standing instructions
Things the user tells it to keep: "always use Python 3", "never ask about colours, pick them", "my projects go under Work". Saved as short rows when the user says so, or when the model recognises one and confirms ("noted — from now on I'll…"). Re-read on **every** message. In 1b they are few, so all are sent; when they grow, Phase 3's search picks the relevant ones (the same mechanism as the ability map).

### 6.2 The project blueprint
One plain-text file in each project's folder, `BLUEPRINT.md`: what the project is, the decisions made, how to run it, how to check it, what is left. **The code and the blueprint are the truth**, not what the model remembers saying.

Rules:
- The model **reads it first** every time it works in that project (the loop puts it in the prompt) and uses it as the **map to find what to change and where** — instead of reasoning the whole project out again.
- **Each change is recorded in the blueprint as soon as it lands, in the same loop** — the line for that item is replaced with what is now true — so the blueprint never lags the code and the next job finds it right.
- It is kept **as small as it can be**.
- It is edited by **replacing lines, never adding where a line can be replaced**: a decision that changes overwrites the old one; nothing accumulates.

### 6.3 The job record
The running job's plan, answers and steps (§3). Working memory for *this* job only; it stays in the database as history and is not re-fed to later jobs (what mattered went into the blueprint).

### 6.4 Projects
A project is a row: name, folder (`/data/projects/<name>` in the workshop), description, last touched. Each has its own folder, so the sandbox jail (§7) holds per project and "the project I mean" is the model reading a short list, not guessing at paths. A job's workspace is its project's folder.

---

### 6.5 Self-improvement — what 1b lays down for Phase 3
"I can't do that" is where the AI must get better, not stop (parent rule 3). The loop stays fixed code — the model never rewrites the executor, its rules, or the loop; what it improves is its **knowledge**. In 1b:
- A failure forces a **different** attempt or a `replan` (§3, §4); the model is told to find another way (another tool, another approach) before it may give up.
- Every failure and the workaround that followed it are already in the action log and the job record, **as the steps that actually ran** (parent §4.5: skills are built from the log, never from the model's own account).
- `give_up` must name what was missing.
Phase 3 turns these into saved skills — a workaround that passed its check is kept and re-found the next time the same failure appears — and Phase 1c gives the model the install hand so "missing tool" becomes "install it" instead of giving up.

## 7. The safety carry-forward — first task of the phase

From the 1a review (parent §11, item 1): `run_command` runs unprivileged and network-off but can still **read** outside its workspace. Before the model runs a single command, it is jailed: the sandbox process sees the system's programs read-only, its own project folder read-write, and **nothing else** — not `/etc`'s secrets, not the user's home, not other projects. Proven by tests from inside a job: another project's files and the executor's own database do not exist as far as the command can see; the user's home is empty; the workspace is still writable. (`/etc/passwd` stays readable — it holds no secrets and every tool needs it to look up users; `/etc/shadow` was never readable to the sandbox user.)

This does not limit what the AI can *use*: every program installed on the OS is visible and runnable inside a job. It limits only what data it can reach.

Also from §11: the executor and the sandbox worker get **one** workspace reference (the job's project folder), set by the loop that creates the job — closing item 3. Item 2 (how the sandbox user is granted each project folder) is decided here: the loop creates each project folder writable by both the sandbox user (commands run as `ai-sandbox`) and the executor's own user (file actions are done in-process) — one shared group on the folder, set once at creation.

---

## 8. Firm line, for the parent spec and for 1c

**The AI may install and use anything on this machine, freely and without asking.** Installing stays on the machine, so decision 9 makes it automatic. It is not a sandbox command: it is its own hand with its own rule, check and snapshot (the admin worker, decision 12), built in **Phase 1c** together with undo because every install gets a snapshot first. Language-level packages (`pip`, `npm`, crates) need the network from inside the job, so 1c also adds a **fetch-packages-into-this-workspace** action where the executor opens the network **only to the known package registries** for that one step. Using a tool *correctly* is enforced by the loop (run it, prove it) and taught by Phase 3's ability map.

---

## 9. How 1b is proven

**Tests with the fake model, no GPU:** chat reply; new project + questions → answer → plan → act → done-with-check; a failing check sends the model back to work; `done` without a check is refused; `act` before `plan` is refused; `ask` in creative mode is refused; a failed command's result carries the tail of its error output; an identical retry of a failed action is refused with the earlier reason; three different failures on one step gives up; `done` is refused while the blueprint is older than the last code change; a job waiting for an answer resumes from the database after a restart; a blocked action pauses as waiting-for-approval and runs after "yes"; an existing project is chosen and its blueprint is in the prompt; the blueprint-first rule is enforced; `edit_file` replaces exactly one match and refuses zero or many; a windowed `read_file` returns only the asked lines.

**One real run, the acceptance test:** the actual local model, the primes job, end to end in the workshop. The proof is the job's record (plan, every step, the check's real output), the project's `BLUEPRINT.md`, and the script it made — reported in plain words, with the record shown.

**What the user sees in 1b:** a plain chat in the workshop (`ai-os-chat`): type, and it answers or works and tells you what it did and what it understood. It is the engine the 1d rail draws.

---

## 10. Changes to the parent spec (applied with this document — parent is now v4)

- §4.4 tier 1: code is **edited in place** (`edit_file`), never read-memorise-rewrite — a hard rule for local models on everyday hardware.
- §4.9: a fifth card, **Needs your answer** — the model's clarifying question, shown in the rail like *Needs your OK*.
- §4.6 Memory: the three places (§6 here) and the blueprint rules; "the chat is not memory".
- §4.1/§4.2: creative mode per job; the model asks before planning.
- §3 design rules: the firm line of §8.
- §11: Phase 1b design agreed; carry-forward items 1–3 assigned to 1b (§7); 1c scope = admin worker (install), package fetch, undo.
