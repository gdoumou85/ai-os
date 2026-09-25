# AI OS — Design

**Date:** 2026-09-15
**Status:** Version 4 (2026-09-16) — rules 7–9, memory outside the chat, the conversation front door and Phase 1b design added. Version 3 — Phase 0 trial complete, findings folded in. The design held up; the changes below are corrections to measured numbers and one new requirement, not a redesign. Findings: `../findings/phase0-findings.md`. Nothing beyond the throwaway trial distro is built.

---

## 1. Purpose

> The AI does whatever a skilled person sitting at this computer could do. It works out from the request what is needed, and a better model gives better results without rebuilding anything.

Examples of what that means:

- "Create an app." It builds the app.
- "The app needs graphics." It finds a way to make them.
- "Watch this and tell me when it changes." It keeps watching and reaches you.

It is never reduced to a chatbot, a command launcher, or a fixed list of workflows. "Anything a user could do" sets how broad it should be. It is not a promise that every job succeeds.

---

## 2. Decisions already made

| # | Decision | What it means |
|---|---|---|
| 1 | **Linux base** | The kernel and drivers stay as they are. The AI replaces what a user normally does on top of them. |
| 2 | **Ships two ways** | First as a file that installs inside WSL on Windows PCs, later as a USB installer for a normal Linux install. Both are built from the same setup script. |
| 3 | **The laptop is a temporary workshop** | Development happens in WSL2 on ALIEN. When the product is ready, the laptop goes back to Windows only. Everything must uninstall cleanly. |
| 4 | **The AI is ours** | No existing agent (Hermes, OpenClaw and the like) is used as the brain. We build the core ourselves. |
| 5 | **The model** | Start with an existing open model (Qwen or Gemma class). Once the system works, fine-tune it on OS jobs. Never train one from scratch. |
| 6 | **Local by default** | Cloud models are off by default. The user switches cloud on for a specific job, and the AI says why it wants it. |
| 7 | **Shared desktop** | The user and the AI use the same normal desktop at the same time. |
| 8 | **Window handover (B2)** | The user hands the AI a real open window, unsaved work included. The AI works that window through the program's own controls (accessibility), not with a second mouse. The user keeps their own mouse and keyboard. |
| 9 | **Risky actions** | The AI acts on its own when the result stays on this machine: installing, removing, configuring, files in its workspace. It asks first before anything that **leaves the machine** (sending, posting, uploading, paying, pushing code, calling a paid service) and anything that **destroys unsaved work** (deleting personal files, closing a window with unsaved work). A snapshot cannot bring back something that already left. The executor applies this rule as code; the model is never asked whether its own action is risky. |
| 10 | **"Deterministic" = dependable** | The job gets finished and a check proves it. Exact repeatability applies only to saved procedures, which replay the same steps. |
| 11 | **Notifications** | Routine messages appear on the desktop. Urgent ones also go to the user's phone through a private Telegram bot. |
| 12 | **The model has no hands except the executor** | The model only proposes structured actions. The executor runs them through three separate workers (see 4.7): a sandbox for free commands, a desktop worker that can only operate program controls, and an admin worker with a fixed menu of operations. A raw admin shell never exists. |
| 13 | **One model that reads text and images** | Swapping a text model and a vision model on 8 GB costs seconds per swap and makes a screen-driven loop unusable. One multimodal model is loaded, with a fixed context budget per step. **Grammar-forced answers, two findings (2026-09-16):** the schema sent with each call is narrowed to the moves legal in that state, because a small model does not reliably obey prose; and the discriminator key must be *first* in every object, because the grammar makes the model commit on the first key — key order is load-bearing and is preserved and tested. |

---

## 3. Design rules that follow from the purpose

1. **The system is the product; the model is a replaceable part.** Abilities, memory, checks and undo do not depend on any one model. Swapping in a better model improves results with no redesign.
2. **It starts from a person's basic abilities, not from a task list.** It can look at the screen, use programs, run commands, read and write files, browse the web, and install software. Every job is a combination of these.
3. **It knows what it can do, and learns what it cannot do yet.** It keeps a live map of its abilities and looks up what fits each request. When nothing fits, it works the job out from basic abilities. If the result passes its check, the method is saved as a new skill.
4. **Saying "done" is never proof.** Only a check counts: the app starts, the file opens, the tests pass, the page responds. When quality is a matter of taste (design, graphics), the user judges.
5. **Content is data, never orders.** Text found on websites, in documents or inside apps can never give the AI new permissions or new instructions. Two things make this hold in practice: the model never sees a secret (rule 6), and nothing leaves the machine without asking (decision 9), so a trick found on a website can neither steal a key nor post your files anywhere.
6. **The model never sees a secret.** The Telegram token, cloud keys and website logins live in the executor's keyring. The executor fills them in; the model only says "log in to X".
7. **It may install and use anything on this machine, freely and without asking** (his rule, 2026-09-16). Installing stays on the machine, so decision 9 makes it automatic; it is its own hand with a snapshot first (admin worker, Phase 1c), never a sandbox command. What the sandbox jail limits is the *data* a job can reach, never the *programs* it can use.
8. **It asks before it assumes.** The AI can never know the full scope of what the user imagines, and a small model least of all. So a job starts with the model's clarifying questions; the answers are saved on the job and re-fed every step; a new question mid-job pauses the job until answered. Per job the user can switch this off — **creative mode**: "do what you think best" — and the model decides for itself (risky-action approvals still apply).
9. **Code is edited in place.** A local model on everyday hardware never reads a whole file, holds it in its head and writes it all back; it reads the part it needs and replaces one exact passage (`edit_file`).

---

## 4. How it is built

```
┌──────────────────────────────────────────────────────────────┐
│  USER   docked chat rail · handed-over windows · phone        │
└──────────────────────────────┬───────────────────────────────┘
                               │
┌──────────────────────────────▼───────────────────────────────┐
│  CORE   request → understand → inspect → plan → act →        │
│         verify → adapt          (job manager: build/create/  │
│                                  watch)                      │
├──────────────┬──────────────┬──────────────┬─────────────────┤
│ MODEL LAYER  │ ABILITY MAP  │ MEMORY       │ HANDS & SENSES  │
│ runner +     │ + SKILLS     │ user, jobs,  │ commands, files,│
│ local models │ LIBRARY      │ what worked  │ web, app        │
│ (cloud off)  │              │              │ controls,       │
│              │              │              │ screenshots     │
└──────────────┴──────────────┴──────┬───────┴─────────────────┘
                                     │ every action goes through
┌────────────────────────────────────▼─────────────────────────┐
│  EXECUTOR   checks rules · holds secrets · logs every action │
│             · snapshot before system changes                 │
│   ┌─────────────┬────────────────────┬────────────────────┐  │
│   │ SANDBOX     │ DESKTOP WORKER     │ ADMIN WORKER       │  │
│   │ free cmds,  │ in the user's      │ fixed menu: install│  │
│   │ own work-   │ session, program   │ config, service,   │  │
│   │ space only  │ controls only      │ file outside ws    │  │
│   └─────────────┴────────────────────┴────────────────────┘  │
├──────────────────────────────────────────────────────────────┤
│  UNDO       snapshot disk holding everything the AI changes  │
├──────────────────────────────────────────────────────────────┤
│  UBUNTU     kernel, drivers, desktop (base, read-mostly)     │
└──────────────────────────────────────────────────────────────┘
```

### 4.1 Core
- Runs the loop **request → understand → inspect → plan → act → verify → adapt** until the job is done or it needs the user.
- Keeps each job's state written down, not only in the model's head: the goal, the limits, what has been done, the evidence, and what is left. A crash or a model swap loses nothing.
- A step that fails once will fail again: the first failure hands the model the full reason, and the same action is never retried as-is — it must work around it. After a set number of *different* failed attempts it stops and asks the user.

### 4.2 Job manager: three kinds of job
| Kind | Example | What is special about it |
|---|---|---|
| **Build** | "Create an app" | Long plans, many tools, real checks (starts, tests pass) |
| **Create** | "Make the graphics" | Produces things; shows them to the user, because quality is the user's call |
| **Watch** | "Monitor this and tell me" | Never "finishes". Survives reboots, wakes on a timer or an event. What counts as urgent is fixed when the job is created ("phone me if X"), not judged per event. On WSL it only runs while Windows is logged in and WSL is kept alive, so the WSL product needs a small Windows-side keep-alive; bare metal has no such limit |

### 4.3 Model layer
- A model runner (llama.cpp or Ollama class) loads model files and runs them on the GPU. It makes no decisions; it is an engine part. The product uses the runner's Vulkan build as the fallback so AMD and Intel GPUs work; NVIDIA is not assumed.
- One multimodal model is loaded (decision 13). **Measured in Phase 0:** on 8 GB, a 9B model keeps **8k tokens** fully on the GPU (with a q8_0 KV cache + flash attention), not the 16k first assumed — 12k+ spills to the CPU and slows the loop. So the per-step budget on the 8 GB workshop tier is 8k: job state, the searched ability entries, and the latest evidence must fit. This is why the ability map is searched, never read whole. The 20 GB deploy target runs the same model at full context and fits a 27B-class model too, so the tight budget is a minimum-hardware floor, not a design limit.
- The model's tool calls are forced through a grammar (JSON schema) so they always parse. Small models otherwise emit a broken call every few percent of steps, which is enough to wreck a long job.
- A bigger model on another machine (pc-worker, 20 GB) is a "remote local" option: private, same switch as cloud, and the cheapest way to prove that a better model gives better results.
- Cloud is a switch per job (decision 6). It is never a silent fallback.
- **Future (his idea, 2026-09-15) — free-cloud model pool with failover:** when the user has cloud switched on, the AI can draw from a maintained list of free-tier cloud models and **rotate to the next when one runs out of quota**, so a job on cloud does not stall on a single provider's limit. This is a routing layer over the runner, the same shape as existing LLM-gateway/fallback-chain projects; it changes nothing about the per-job cloud switch or "the model never sees a secret" (the executor holds the keys and picks the provider). Not in the near-term phases; noted so the model layer is built to allow a swappable provider list.

### 4.4 Hands and senses (the basic abilities)
Tried in this order, most dependable first:
1. **Direct:** commands, files, system services, program command lines and APIs.
2. **Program controls:** the accessibility layer, which exposes buttons, fields, menus and text. This is how window handover works (decision 8). **Phase 0 split this into two mechanisms that behave differently:**
   - **Reading and operating controls** (clicking buttons, opening menus, toggling settings) works through accessibility *actions*, which run in-process and need no input device. Proven headless: the AI read a full LibreOffice tree and toggled its Bold button. This is the dependable core of handover.
   - **Typing free text** into a field is separate: it needs a real input *seat* (a live display with keyboard). Headless WSL has none, so free-typing did not commit there. It works on any real display — the 20 GB PC or bare metal. Two seat-free text routes are built in Phase 2 as the workshop path and as belt-and-braces everywhere: the program's own scripting API (e.g. LibreOffice UNO) and clipboard-paste via an action.
3. **Screen:** screenshot plus a pixel click. Only a fallback, and it is clearly shown to the user while it happens. **Phase 0 finding:** on Wayland this must use the **compositor's own screencast/remote-desktop API** (GNOME's Mutter, KDE's KWin), not the freedesktop portal — the portal cannot remember input consent and needs a human to click its dialog, while the compositor API serves the session owner with no dialog. Screen capture through it is proven (a real screenshot was produced and the model read it). The AI's primary path (accessibility) needs no permission at all.

Other abilities: web browsing, installing and removing software, desktop notifications, Telegram messages to the user.

### 4.5 Ability map and skills library
- Each entry records: what it does, what it needs, what it produces, what must be true first, its side effects, how to check it worked, and how to recover.
- Updates itself when software is installed or removed.
- For each request, the core **searches** the map for relevant entries rather than reading the whole map, because small models have limited room.
- New skills are saved only after their check passed, and they are built from the executor's action log (the structured steps that actually ran), never from the model's own description of what it did. A skill containing admin steps is shown to the user before it is kept.

### 4.6 Memory
- The user (preferences, projects), past jobs, and which methods worked or failed.
- Stored outside the model, so it survives a model swap.
- **The chat is not memory** (his rule, 2026-09-16; detail in the Phase 1b design). A local model on an 8k budget cannot carry a long history and the design does not pretend it does. Three places remember instead: **standing instructions** (things the user told it to keep, re-read on every message), the **project blueprint** (one small `BLUEPRINT.md` per project — what it is, the decisions, how to run and check it, what is left; the code and the blueprint are the truth; read first every time it works there as the **map to what to change and where**; **updated in the same loop as soon as each change lands**; kept as small as possible; edited by replacing lines, never piling on), the **job record** (working memory for the running job only), and a bounded **last-run note** per project (`LAST_RUN.md`: what went wrong, written by the loop when a job fails, read first by the next job — fix it first, prove it — and wiped by the loop the moment a job in that project is proven done, so old fixed problems are never re-read and memory never grows). The model is told: if it is worth remembering, write it down; you will not see this conversation again.

### 4.7 Executor
- The model's only hands. The core turns model output into structured actions; nothing the model writes is ever run as a shell string with admin rights.
- Checks every action against the risky-actions rule (decision 9) before running it, as code. Holds the secrets (rule 6). Logs every action together with the job that caused it.
- Runs actions through three workers, each as separate as Linux allows:
  - **Sandbox worker:** free commands, program command lines, builds, tests. Runs as its own Linux user inside the job's workspace. Cannot see personal files, cannot touch the system.
  - **Desktop worker:** runs inside the user's own desktop session, because program controls (accessibility) only exist there. It accepts program-control actions only (click this, type here, read that) and nothing else.
  - **Admin worker:** a fixed menu of verbs and nothing else — in 1c the root wrapper's list: `install`, `remove`, `pkg-list`, `service <name> enable|disable|restart|state`, `read-file`, `write-file`, `remove-file`, `make-dir`, `remove-dir`, `sandbox-run`. Every verb validates its own arguments (package and service names against a regex, paths against its allowed roots) and reports what it changed, so the reverse is known and recorded. **Approved file operations outside the workspace run here**, and **writes under `/etc` are never free**: they always need the user's yes, because a unit file, a cron entry, an apt source or a PAM line is root code execution. No general command exists here.
- Personal files are reached only through the executor's file operations, which apply decision 9.

### 4.8 Undo
- **Per-hand undo (1c).** Files: every project folder is a btrfs subvolume, and a read-only snapshot of it is taken as a job starts — undo puts that snapshot back. Everything else carries a **recorded reverse** written when the change is made: the packages an install really added, a service's previous state, a file's previous contents (or the fact that there was no file), a directory created, a setting's previous value. **The unit of undo is one job**, and the user triggers it by saying **undo**: the most recent finished, failed or cancelled job with anything left to put back goes first, its rows in reverse order, one line of plain words per reversal. A reversal that fails is reported and the rest still run.
- Everything the AI is allowed to change (installed software, configuration, workspaces, the user's data) lives on one disk that can hold snapshots. The base system underneath is read-mostly. On WSL that disk is a mounted virtual disk, because WSL cannot boot from a snapshot filesystem; on bare metal it is a partition. The undo code is the same both ways.
- **Files roll back instantly. System changes roll back plus a restart of what changed**, because running programs keep the old version until they restart.
- **Limit:** unsaved work inside an open program cannot be snapshotted. Decision 9 covers this: the AI asks before closing or discarding a window with unsaved work. Two more the reversal report says out loud: files a program wrote outside the project during the job, and a package version the archive no longer carries (a reinstall takes the current one).

### 4.9 User interface — the front door (designed 2026-09-15, brainstormed with mockups)

**Firm requirement (his call): the user never needs a terminal.** A terminal is a builder's tool. The product shows what it did in a way a non-Linux user reads at a glance — never a command, a log, or raw shell output. The terminal exists under the hood; the user is never sent to it.

**Interaction: typed chat first.** Voice is a later additive layer, not version 1.

**Surface: a docked side rail**, pinned to one edge of the desktop (default right, adjustable width), always visible. It is the single place the user talks to the AI OS and watches it work. When the AI uses an app, that app opens beside the rail in the main desktop area.

**Results are always shown visually, as cards in the rail. Four card types cover every job:**
| Card | When | What it shows |
|---|---|---|
| **Done** | a job finished | the real result — image preview, the opened file — with Open / Save-where-you-want / Try again. Never just a filename. |
| **Building** | a job is running | plain-language steps with ticks (✓ wrote the app, ⟳ testing) and a **Watch it work** button that opens the live app window beside the rail, so it is never a black box. |
| **Watch** | a background/watch job (4.2) | pinned at the top, quietly updating ("checked 2m ago, no change"). |
| **Needs your OK** | an action hits the risky-actions rule (decision 9) | stops and asks right in the rail — Send / Cancel / Edit. Anything reversible it just does; only "leaves the machine" or "destroys unsaved work" prompts. |
| **Needs your answer** | the AI has a clarifying question (rule 8) — before planning or mid-job | asks in plain words right in the rail; the answer is saved on the job and the job resumes. Never shown in creative mode. |

**The rail is a conversation, not a control panel** (his rule, 2026-09-16): the user types plain language and the AI works out whether it is chat, new work, an existing project, an answer, an approval or a cancel — and says what it understood before acting, so a misreading is corrected in one line. There are no start/answer/approve commands.

The rail also carries pause, cancel and take-over controls per running job.

**Handing over a window:** the user tells the AI to take a specific open window (decision 8); the AI works it through program controls (4.4 tier 2).

**Phone:** urgent notifications only in version 1 (decision 11).

### 4.10 Implementation stack (decided 2026-09-15)

The runtime is **system software that ships to machines we do not control**, so the stack is chosen for safe, self-contained distribution, not for familiarity. The actual model inference runs in the runner (llama.cpp/Ollama) as a separate process reached over HTTP, so the runtime language does no ML itself — it orchestrates the OS.

- **Language: Rust** for the whole shippable runtime — the core loop, the executor and its workers, the job manager, the daemon, and the chat rail (native GTK4, matching the GNOME choice). Chosen for: one self-contained binary (no interpreter or dependency hell on a stranger's machine), memory safety with no GC for the privileged executor that holds admin rights and secrets, low footprint for a long-running service, and first-class Linux integration (D-Bus via zbus, Wayland, GTK4).
- **Python is confined to the offline fine-tuning tooling (Phase 6)**, which never ships as part of the runtime.
- **Sandbox isolation:** a separate Linux user plus `systemd-run` scoped limits (proven available in Phase 0), not Docker — lighter and already present on the base.
- **Action protocol:** the model emits JSON actions against a fixed schema, grammar-forced (decision 13); the executor is a dispatcher that matches each action to a coded handler. No shell strings are ever executed.
- **State store: SQLite** — one file, survives restarts and model swaps (4.1), no server to run.
- **Model runner:** llama.cpp/Ollama class, talked to over HTTP; its Vulkan build is the cross-GPU fallback (4.3).

---

## 5. Where it runs

### 5.1 The workshop (this laptop)
- **Machine:** ALIEN, with an Intel Core 7 240H (16 threads), 32 GB RAM, an RTX 5060 Laptop GPU with 8 GB, and 376 GB free disk.
- **WSL2 limits:** 20 GB RAM, 12 threads, disk capped at 100 GB (his call, 2026-09-15: the C: drive must keep its headroom). Everything lives in one folder, `C:\WSL\ai-os\`.
- **Base:** the newest Ubuntu LTS (26.04), confirmed working in Phase 0. 26.04's GNOME is Wayland-only (no X11 session), which is why the screen fallback uses the compositor API (4.4 tier 3).
- **Desktop choice (Phase 0 decision): GNOME.** It is proven end-to-end in this environment — headless shell renders apps, accessibility reads and drives them, the compositor screencast produced a real screenshot. KDE is the candidate to revisit on bare metal: its `KWin.ScreenShot2` is a cleaner, dialog-free capture, but a bare KDE compositor is not a full session, so apps did not render in the WSL workshop. The choice is reversible — accessibility, the primary path, is identical on both.
- **Trial limits (measured, not guessed):** the workshop runs a **headless, invisible** session — no Remote Desktop, no screen shown on Windows (his call, 2026-09-15). It has no real input seat, so free-text typing does not commit here (4.4 tier 2); everything else — reading and operating apps, capture, the model, snapshot — works. Windows App Control is enforced on this laptop; WSL passed.
- **Setup:** everything is installed by one setup script, never by hand.
- **Seeing an app when needed:** WSL shows any single Linux window as a normal Windows window through WSLg. The AI's own work runs in the invisible session; nothing is displayed unless the user asks to see a specific window. Bare metal has a real screen and none of this applies.
- **Removal:** unregister the distro, switch WSL off, delete the folder.
- **User step needed:** installing WSL needs admin rights and a reboot.

### 5.2 The product
- **Two editions, both kept (his call, 2026-09-16):** first the WSL install file for Windows PCs, then the full Ubuntu image (USB/ISO) with the AI OS on top, built from the same base and the same setup script. On WSL the rail and any app the user asks to see appear as Windows windows through WSLg; on the full image they sit on the real GNOME desktop.
- The same setup script builds the WSL install file, and later the USB installer.
- The minimum hardware for other computers is set from the evaluation results, not guessed.

---

## 6. Evaluation plan

1. **Public test set:** a subset of OSWorld, about 370 real jobs on an Ubuntu desktop, each with an automatic pass/fail check. Only OSWorld's task definitions and checkers are ported; its own virtual-machine harness is not used. Expect a low absolute score from an 8B local model; the number that matters is the same jobs scored across models (point 5).
2. **Our own set:** about 30 jobs covering system jobs (install, configure, repair), window handover, build, create and watch.
3. **Every job runs 3–5 times.** Measured for each job:
   - success rate (dependable),
   - same outcome each run (repeatable),
   - time taken,
   - how often it asked the user,
   - how often undo was needed.
4. **Watch jobs:** we trigger a change on purpose and measure that the right message arrives on the right channel in time.
5. **Model comparison:** the same jobs run with different models: the local 8B, a bigger model on pc-worker over the network, and (switched on for the comparison only) a cloud model. This proves that a better model gives better results and picks the model.
6. **The rule:** the AI saying "done" never counts. Only the check does.

---

## 7. Build order

Each phase gets its own plan and its own approval.

| Phase | What | Output |
|---|---|---|
| **0. Trial** (throwaway) — **DONE 2026-09-15** | Answered the unknowns: GPU inside WSL ✓; accessibility reads + operates native apps ✓ (typing needs a real seat); screen fallback via the compositor API ✓; snapshot disk with rollback ✓; a 9B multimodal model at 35–40 tok/s, 100% grammar-forced parse, 8k context on 8 GB ✓; desktop = GNOME. Full-desktop-on-Windows dropped as not needed | `../findings/phase0-findings.md`; desktop = GNOME |
| 1. Foundation | Core loop, executor, direct abilities, undo, chat panel | It can do system jobs and prove them |
| 2. Handover | Program controls (accessibility) plus the screen fallback | It can work a window the user hands it |
| 3. Skills — **BUILT 2026-09-19 (v0.7.0)** | Notebooks of what worked: "This computer" plus one per craft, read at the start of every job, updated after it | It improves on repeated jobs |
| 4. Watch | Background jobs, desktop and Telegram notifications | It can monitor and inform |
| 5. Create | Local image generation and other creative tools | It can make graphics and assets |
| 6. Fine-tune | Train the open model on logged OS jobs | Our own model |
| 7. Package | WSL install file, then the full Ubuntu image (USB/ISO) — both editions ship (5.2) | Installable on other computers |

---

## 8. Open questions (not blocking the trial)

- The minimum hardware for other computers: decided from the evaluation.
- Product name.

Settled by the audit: undo uses a snapshot disk on both targets (4.8); "urgent" is fixed when a watch job is created (4.2).
Settled by Phase 0: GNOME over KDE for the WSL edition (5.1); 8k context floor on 8 GB (4.3); accessibility drives apps but free-text typing needs a seat (4.4).
Settled by the front-door brainstorm (2026-09-15): typed chat first, a docked side rail, four result-card types, no terminal ever (4.9).

## 9. The honest risk

None of the engineering above is the hard part. The hard part is whether an 8B local model is smart enough to drive the loop. The expectation: system jobs and scripted builds work; window handover partly; "create an app from scratch" poorly until a bigger model sits behind it. The architecture is right precisely because that upgrade is a configuration change, and section 6 measures it.

## 10. Out of scope

- A new kernel.
- Training a model from scratch.
- Using an existing agent as the brain.
- A literal second mouse pointer: tested, and most apps ignore it.
- Giving the AI jobs from the phone: a later step, not version 1.
- A general admin shell for the model, in any form.

## 11. Phase 1a status & carry-forwards (2026-09-15)

**Phase 1a (executor spine) is BUILT** on branch `phase1a-executor-spine`: the Rust `executor` crate at `runtime/executor/` — `Action` (JSON), `classify` risk rules (decision 9, pure), SQLite action log, the `Executor` dispatcher (a blocked action never reaches a worker and is always logged; an executed action's outcome is never lost to a logging failure), the real `SandboxWorker` (runs commands as unprivileged `ai-sandbox` via `systemd-run`, no network, workspace-scoped file ops with symlink resolution on both read and write), and a demo binary. 16 unit + 4 integration tests pass; the sandbox read/write escapes found in review are closed.

**Prerequisites for Phase 1b (core loop + model) and the secrets phase — carried forward from the Phase 1a review:**
1. **Filesystem-confine `RunCommand`.** It is `Auto` and the sandbox is unprivileged + network-isolated but **not** yet jailed to the workspace, so a command like `cat /etc/passwd` still reads outside it (it cannot exfiltrate — network is gated). Before the model runs or any secret exists, jail `RunCommand` to the workspace (mount namespace / `InaccessiblePaths`) or classify it more tightly. This is required for rule 6 (the model never sees a secret).
2. **Workspace permission model.** How each job workspace grants `ai-sandbox` access (setgid `/data/jobs`, or per-workspace group) is undecided; the orchestrator that creates workspaces owns this.
3. **Couple the workspace reference.** `Executor` (classification) and `SandboxWorker` (enforcement) hold independent `workspace` fields; the integrator must keep them identical (or refactor so one borrows the other).

**Phase 1b design agreed 2026-09-16** — `2026-09-16-phase1b-core-loop-design.md`: conversation front door, job loop with clarifying questions and creative mode, grammar-forced moves (reply / start / ask / plan / act / replan / done-with-check / give_up-naming-what-was-missing), swappable model connection (Ollama first), memory outside the chat (standing instructions, project blueprint, job record), `edit_file` + windowed `read_file`. Items 1–3 above are closed inside 1b (its §7). **Phase 1c scope:** admin worker (install / remove / config / service), the fetch-packages action with registry-only network, undo snapshots.

**Phase 1b BUILT 2026-09-16** on branch `phase1b-core-loop` (design `2026-09-16-phase1b-core-loop-design.md`; §7b/§8b there for the limits and the spike). The live acceptance test passes: qwen3.5:9b, through the real jailed sandbox and with no human, created project `primes`, wrote `BLUEPRINT.md` and `find_primes.py`, ran it, updated the blueprint, and said done with a check whose recorded output holds the tenth prime (≈38 s, 5 steps). Whole-branch review: sound; its fix wave closed the jail's `/mnt` hole (workshop only), job-id collisions, resume-contract defaults, the prompt budget for big writes, a blueprint-gate bypass, a silent `remember`, and pinned the `approved` flag with a test.

**Build order decided 2026-09-16 (his call):** **1c** hands + undo → **1d the rail** → **skills mechanism** (self-growth: he drives it as an average user; the AI never adds a *hand* — hands are our code with safety rules; it adds *skills* built from them). Then the rest.

**Carried forward to 1c / 1d (from the 1b final review):**
- 1c: **housekeeping is a third request kind** — "prep a folder where all projects live", settings, the machine's own layout — recognised as such, done through the admin hand with a snapshot, and changes a setting rather than creating a project (his question 2026-09-16). Listing and removing standing instructions is admin-hand work too.
- 1c: narrow the sudoers grant (`ai` → bare `systemd-run`) to a wrapper with a fixed property set before the admin hand runs privileged.
- 1c: the blueprint gate ignores code changed via `run_command` (e.g. `sed -i`), and a job can finish with no blueprint at all when nothing was written — make the gate absolute for new projects.
- 1d: the rail needs a third answer while an action waits for OK ("what does it send?" is a question, not a refusal); a message arriving while a job is left mid-work is not yet shown to the model ("interrupt a running job"); an out-of-turn move must never be drawn raw.
- Small carry-forwards: `edit_file` with an empty `find` gives a confusing message; one HTTP agent per call (no keep-alive); the HTTP error flattens status/transport/decode; standing instructions are unbounded and undeduplicated; `pending_reason`/`note_to_model` not cleared after an approval cycle; `allowed_moves` ↔ `run_turns` are two hand-kept lists (make one table both consult).

**Workshop-only shortcuts that MUST NOT ship to the product (packaging phase):**
- A sudoers `NOPASSWD: /usr/bin/systemd-run` grant for user `ai` (root-equivalent) — used because the workshop executor runs as `ai`; the product's executor daemon is already privileged and needs no such grant.
- `/data` is 0777 root-owned with no sticky bit, so the trial db could be created; the product sets `/data` ownership/permissions properly. Without the sticky bit any user who can write there can also rename or remove another user's entries.
- The wrapper is installed from the Windows-mounted repo (`/mnt/c/...`) by `trial/setup-admin.sh`: what runs as root is copied from a filesystem Windows can write. The product ships the privileged daemon from a package.

## Phase 1c status (2026-09-16)

**Phase 1c (the hands and undo) is BUILT** on branch `phase1c-hands-and-undo`, design `2026-09-16-phase1c-hands-and-undo-design.md`. The model now has an admin hand as well as the sandbox: `install`, `remove`, `service`, `make_dir`, `write_file`/`read_file` outside the workspace when the user approves, all through a root-owned shell wrapper (`/usr/local/libexec/ai-os-admin`) with a fixed verb menu that validates every name and path itself; `fetch_packages` opens the network for exactly one step to the package registry's addresses (pip, npm, cargo) and nothing else; `housekeep` is a third front-door move for the machine's own layout, with `set_setting projects_root` as the one setting v1 knows; every project folder is a btrfs subvolume snapshotted at job start, every admin action records its own reverse, and the word **undo** puts the last job back, one line of plain words per reversal. The sandbox itself now runs through the same wrapper.

**The live acceptance has passed** (1c design §11 Results): qwen3.5:9b, both hands, no human, four scripts in 346.90 s. It prepared `/data/work` and set `projects_root` to it, and the next new project landed there; it wrote a primes script inside that project and ran it; it installed cowsay through the wrapper and proved it with `/usr/games/cowsay hello`. On the word **undo** it removed the cowsay install and restored the project's files from the newest snapshot — and reported "could not undo" for the earlier job's files, because that job's own snapshot had been pruned when the second job in the same project took one (the "only the newest snapshot per project is kept" ceiling, finding 10) — then cleared the setting. The pip fetch of `tabulate` through the registry allowlist succeeded, and the 9B then gave up on the table itself after six replans: §9's honest risk, stated rather than smoothed over. Eighteen runs went into getting there, and the ten gaps the failures exposed (the model reaching for `mkdir`, for `apt` and for absolute paths instead of the hands; a `BLUEPRINT.md` written and re-read in a housekeeping job that has no project; `/usr/games` off the sandbox PATH; a successful `make_dir` whose detail said nothing; the jail's hidden `/data` read as the machine's truth; a workspace-relative program never running; the front door summarising a request's standing half away, which is why a job now carries the user's words verbatim; and a second job in a project consuming the older job's snapshot) are numbered in that Results block. Every one of them was fixed in the code, never in the test.

**The sudoers narrowing is done:** `/etc/sudoers.d/ai-os-admin` is the single line `ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin`. The `NOPASSWD:ALL` and bare `systemd-run` grants are gone and `ai` has been removed from the `sudo` group (`id ai` → `ai`, `ai-sandbox`). The first carry-forward of §11 above is closed; the wrapper is workshop transport and is itself replaced by the privileged daemon at packaging, where the verb list becomes its interface.

**Carry-forwards (accepted ceilings, not bugs):**
- A fetch step's resolver IP is reachable on every port for the length of that step (the systemd address filter is per-address, not per-port).
- A cargo config written under the workspace is model-writable, so a model that wanted to could re-point cargo's registry within its own project.
- DNS remains a channel out during a fetch step.
- Only the newest snapshot per project is kept: undo reaches one job back per project's files.
- A restore that fails after `ro=false` leaves the snapshot writable.
- A snapshot that cannot be taken fails the job start rather than running uncovered.
- `write-file` creates one missing parent level, no more.
- Only a `.service` suffix is normalised on a service name; other unit suffixes are refused rather than mapped.
- Projects that predate 1c are plain folders: their files are not covered by undo, and the reversal report says so for that job.
- 1d items are unchanged (the rail, undo as a button, the reversal report as a card), as are §12's out-of-scope items in the 1c design.

## Phase 1d status (2026-09-16)

**Phase 1d (the engine service and the chat rail) is BUILT** on branch `phase1d-rail`, design `2026-09-16-phase1d-rail-design.md`. The engine no longer speaks prose to whoever started it: it emits typed events — `said`, `you`, `understood`, `plan`, `step`, `needs_answer`, `needs_ok`, `done`, `failed`, `stopped`, `undone`, `busy`, `state`, `error` — and one function turns them back into the sentences the terminal has always printed, so nothing that worked before changed. The engine now runs as a background user service, `ai-os-engine`, holding the one engine on one thread and speaking JSON lines over a private socket at `$XDG_RUNTIME_DIR/ai-os.sock`; a job it is working keeps running when every window onto it is closed, and a socket something still answers on makes it refuse to start rather than put two engines on one database. `ai-os-chat` is a thin client of it, and `ai-os-rail` is the new one: a native GTK4 window that draws the conversation as cards — Building with its ticked steps, Needs your OK with Yes and No, Done with the files the job changed, an Open button on each and Undo underneath, Undone with the reversal lines — reconnecting on its own if the service restarts under it.

The two 1b carry-forwards are closed. **Stop lands mid-job**: the word raises a flag the engine checks between steps, so it costs at most one model call and one action, and it works whether the engine is busy or idle. **A question at a Needs-your-OK is a question**: the model answers it and the same OK is asked again, the pending action untouched, counted neither as a yes nor a no, and bounded at three. While a job is being worked, anything else from any client is answered `busy` without reaching the model; while the job waits for an answer or an OK it is queued like normal text.

**§5.1 display note:** the rail, and any app opened from it with the Open button, use the WSLg display and appear as ordinary Windows windows. The AI's own desktop work in Phase 2 uses the invisible session instead. On bare metal there is one display and the distinction disappears.

**The live acceptance has passed** (1d design §7 Results): qwen3.5:9b through the service on a temp socket, no human, four scripts in 60.2 s — a project job ending Done with its changed files listed, a `/etc` write held at a Needs-your-OK with the question answered and the OK re-asked before `yes` ran it, `undo` taking the `/etc` file off disk again and restoring the project's files, and a second client answered `busy` mid-job before `stop` landed. Nine runs went into it, and the seven product gaps the failures exposed — `make_dir` reached for a folder inside the project, a file outside the project written with `echo` in a sandbox that cannot reach there, a question at a Needs-your-OK racing the service's `running` flag into a `busy`, the model giving up for want of a yes it has no way to ask for, the same yes asked twice, the same successful action repeated nine times, and one retry where the blueprint gate needs several — were every one of them fixed in the code, never in the test, with a new test each. The rail is verified by a screenshot of the real window on the Windows desktop, `docs/superpowers/findings/phase1d-rail.png`.

**Carry-forwards (accepted ceilings, not bugs), from the 1d design §8:**
- Steering a running job with new text is still out: it waits for the skills phase.
- No pause, no take-over, no Watch card, no replaying earlier finished jobs in the rail, no pinning the rail to a desktop edge.
- Changed files are found by mtime, not by content, so a file touched but unchanged is listed; files the admin hand wrote outside the project folder are not on the Done card at all — the undo report is what covers those.
- No D-Bus front for the service; revisit when Phase 2 brings zbus in.
- One engine, one user: the socket is per-user by construction.
- A stop that arrives in the same instant a job finishes on its own is queued and then means "no job to stop"; the engine answers that itself — "Nothing is running now.", a fixed word in every state, never a model call.
- A yes now holds for the rest of the job it was given in, per exact action. A no still overrides it, and neither outlives the job.

## Phase 2a design agreed (2026-09-17)

**Phase 2 is split:** 2a the desktop hand, 2b the screen fallback; browsers through their own automation wait. Design: `2026-09-17-phase2a-desktop-hand-design.md` — five accessibility actions (`look`, `press`, `type`, `read`, `open_app`) with nothing per-app, "take my window" as a `housekeep` job (no new move), decision 9 for a press as a fixed word list in code, no undo inside a window said out loud on the Done card, the invisible session as a unit, and a look cap that follows the context budget. **Two corrections from its probe:** free text through the accessibility interface does not need a seat (it commits in GTK widgets on any display; §4.4's seat claim and Phase 0's are wrong), and LibreOffice Writer's document body is the exception, a property of Writer, carried forward. LibreOffice UNO is out as a text route: it is per-app code, a Phase 3 skill if ever needed.

## Phase 2a status (2026-09-17)

**Phase 2a (the desktop hand) is BUILT** on branch `phase2a-desktop-hand`, design `2026-09-17-phase2a-desktop-hand-design.md`. The AI has a third hand. It works any program through the accessibility bus with five actions and no per-app code anywhere: `look` with no window lists what is open, `look` with one lists its controls with short ids, `press` a control by id and by the name it echoes back, `type` into one, `read` one, `open_app` opens a program by its desktop-entry name. "Take my Text Editor window" is the `housekeep` job it always was — no new move, no new job kind, no new field — and so is a program the AI opens for itself to answer a question. A press whose name is on a fixed word list (close, delete, send, quit…) is a Needs-your-OK that says which control and why; nothing inside a window can be undone, and the Done card says so, naming the windows the job worked. The look is capped at the model's context divided by 200 — 40 controls on the 8 GB workshop — and says how many more there are. The invisible session is a user unit, `ai-os-desktop.service` on display `wayland-ai`, with the engine's unit ordered after it; a window the AI opens goes there unless the user asked to see it.

**§4.4's seat claim is corrected in code, not just on paper:** free text through the accessibility interface commits without a seat, on any display, and the hand types that way. LibreOffice Writer's document body is the exception and is carried forward.

**The live acceptance has passed** (2a design §12 Results): qwen3.5:9b through the service on a temp socket, no human, three scripts in 81 s, and again in 83 s on the final prompt — the user's Text Editor window taken, a line added and saved to disk; the calculator opened in the invisible session, 12 × 34 worked on its own controls and 408 said with nothing on screen; "close it without saving" held at a Needs-your-OK, `yes`, and the window gone. Fifteen runs went into it across two rounds and thirteen product gaps came out, every one fixed in the code with a test and never in the acceptance: the desktop unit waiting on a portal it does not need, a window that would not answer to the name the list gave it, GTK 4 popover menu items with no name at all, a wrapper refusal that named no alternative, a blank window name refused instead of read as "list the windows", a failure held against an action after the world had changed, recycled object ids with no cheap way back, a desktop question read as a new software project, a replan carrying an action instead of a plan, a "look first" the model had already done, a `=` key pushed off the end of the look by labels that only repeated the buttons beside them, and three ways for a refused step to end a job that was one nudge from right.

**Workshop note (2026-09-17, after the merge):** a GTK window created on WSLg before its RDP client has attached — the first seconds of a cold WSL boot, which is exactly when `wsl -d ai-os ai-os-rail` on a stopped VM starts it — shows, then never gets another frame callback once the seat and output arrive: it neither moves nor takes keys, and GTK never binds the late seat (Wayland trace, main thread idle in poll). Weston logs the attach, so `trial/rail.cmd` waits for that line and then starts the rail; proven on two cold boots. Not a product matter: bare metal has its seat before any session starts. The WSL edition's launcher needs the same wait at packaging.

**Carry-forwards (accepted ceilings, not bugs), from the 2a design §13:**
- The screen fallback — Mutter screencast and a pixel click, shown to the user while it happens — is 2b. Browsers go through their own automation; Electron apps with no tree go to the screen fallback.
- The clipboard-paste text route is still deferred: feasible as probed, not dependable, and only needed where a document does not take insertion.
- Telling a phantom insertion from a real one; a button with an innocent name that sends or destroys; "Watch it work", pause and take-over.
- **New from the build:** GTK 4 lists a menu button twice — a push-button wrapper carrying no action and the toggle button behind it that has the real one — and its popover menu items carry no accessible name at all, only a `keyshortcuts` attribute. The hand names a nameless control by its shortcut and points a refused press at the twin; a toolkit that does neither would leave those controls out of reach.
- **New from the build:** a toolkit may expose each control's caption as a label beside it, doubling the size of every look; a label that only repeats a control's name is dropped as filler. A toolkit that hides a control's name somewhere else again would need its own answer.
- **New from the build:** a job whose work is to close a window ends `Failed`, not `Done`: the model's own check is a `look` for the window, which is gone, which is the proof. The acceptance passes either way — the window is gone from the bus and the Needs-your-OK was asked — but the card is wrong, and a "gone is what you asked for" check is not something the hand can tell from a mistake today.
- **New from the build:** toolkits recycle their accessibility object paths, so an id is only as fresh as the last look. The guard catches it and the refusal says where that name went now, but a hand that wants to press ten buttons in a row still pays for a look between them.
- **New from the build:** on WSL the units are fine but the distro is not — it shuts down about sixty seconds after the last `wsl` command exits, taking the user manager with it. A cold start brings both units back. An unattended session that outlives the launching shell needs the Windows-side keep-alive §4.2 already calls for; bare metal has no such limit.

## The desktop edition (2026-09-18)

**§5.2's full Ubuntu edition, pulled forward from Phase 7 at his request** — design `2026-09-18-desktop-edition-design.md` (§9–§10 are what stands; §3's automated VM build was tried three times and withdrawn), plans `2026-09-18-desktop-edition.md` and `2026-09-18-install-command.md`. He installs Ubuntu 26.04 Desktop himself under his own user name; one command, `install/install.sh` (fetched by `install/get.sh` from the latest GitHub release), adds the AI OS for whoever runs it: `/data` as a btrfs loop file, the `ai-sandbox` account, the root wrapper and its single sudoers line, the engine as his user service, the rail opening with the session, and either a runner elsewhere (`--model-url`, his VM uses the Windows Ollama at `http://10.0.2.2:11434`) or Ollama installed locally with the Phase 0 settings. Nothing in the product names the user `ai` any more: the wrapper's owner is whoever sudo says, the prompt names the engine's own home. **WSL was uninstalled on 2026-09-18**, so the workshop of §5.1 no longer exists: GitHub Actions builds the programs inside Ubuntu 26.04, the repo `gdoumou85/ai-os` is public with no licence yet, and building, testing and the live acceptances move into his Ubuntu. The installer's first real run is his; Phase 2b and the rest continue there.

## 2026-09-19: models on the network, the screen fallback, the cloud pool, and Phase 3

Releases v0.2 to v0.7.0, all built by CI. None has had its live acceptance on the owner's VM yet
except by his ad-hoc runs.

- **Models on the network**, design `2026-09-19-network-models-design.md`. The engine speaks
  Ollama's `/api/chat` and OpenAI-style `/v1/chat/completions` (LM Studio and cloud providers).
  `ai-os-find` sweeps every private /24 for runners. A **Model** button switches between them.
- **Phase 2b, the screen fallback**, design `2026-09-19-phase2b-screen-fallback-design.md`: an
  8×6 grid, then a zoom, click and type. It stops if the person moves.
- **The cloud pool.** A **Cloud** switch and accounts in `~/.config/ai-os/cloud.tsv`, tried in
  order; a used-up account rests while the next one answers. Providers are Ollama (no longer free
  for the owner), NVIDIA (v0.6.4) and OpenRouter's `:free` models (v0.6.5).
- **Robustness from the owner's runs:**
  - Answers may take 30 min, and a job 200 steps.
  - An answer that is not a move is sent back as a rejected move (v0.6.3). LM Studio with a
    thinking Qwen let one past its grammar.
  - The replan bound counts replans in a row without a step that worked.
  - The prompt says pip packages live in the project's `.venv`.
- **Phase 3, skills**, design `2026-09-19-phase3-skills-design.md` (as built), plan
  `2026-09-19-phase3-skills.md`. It replaces §4.5's "ability map".
  - A skill is a craft notebook, and "This computer" holds the everyday know-how.
  - The tips come from the job record and are a tip sheet, not a replay. Only the most-used and
    matching ones are read, within 3000 characters.
  - One `learn` turn after each job decides what goes in.
  - Machine-changing entries wait for Keep, in their own table.
  - A **Skills** button shows the notebooks and deletes entries.
  - Its acceptance cases are the owner's: open a website twice, then make a round object in
    Blender twice.
- **Next,** at the owner's word: a live "thinking — N min, N words" line and a step counter, then
  his skills test. A Settings screen, with the answer timeout as its first item, comes later.

## 2026-09-19: full access (v0.8.0)

The owner reversed the confinement decisions of this design and of 1c/1d/2a/2b: "the AI should
have full access to everything; if it kills the OS it kills it — that's why we run it in a VM."
Design `2026-09-19-full-access-design.md`, plan `2026-09-19-full-access.md`.

- The AI runs as the owner with `NOPASSWD: ALL`, the network and every folder. One machine hand
  runs commands and file actions; the sandbox, the root helper and its fixed menu, the risk rules
  and the install/remove/service/make_dir/fetch_packages/http_post actions are gone.
- Nobody is asked: no Needs-your-OK card and no approval state.
- No Undo, no undo log, no project snapshots. The VirtualBox snapshot is the way back.
- The screen hand keeps working while the person uses the mouse.
- Stays: the loop guards and budgets, the answer timeout, Skills (Keep/Discard now counts a `sudo`
  command as changing the machine), the Stop button.

## 2026-09-20: what it is doing, and windows it cannot see (v0.8.1)

Two things the owner hit on his first full-access run.

- **The line under the cards says what is happening now**: which plan step of how many, and the
  command, file or window being worked this second; after twenty seconds it says how long the wait
  has been. The turn after a job ("thinking about what to remember") says so too — that one is
  minutes of silence on a local model. `Engine::tick` sends it to the sink only, never onto `out`:
  a status line is not a message and must not fill the front door's `recent_messages(4)`.
- **A window the accessibility bus cannot see is on the screen, not missing**: Blender (like games
  and anything drawing its own interface) lists nothing, so `look` and `open_app` both read as
  failure and the AI asked him what to do, then opened Blender three more times. Every miss now
  points at `screen_look` and at the program's own command line; `open_app` will not make a second
  copy within two minutes; the rules say to drive a program headless when it can be
  (`blender --background --python …`); "Would you like me to…" now counts as asking permission.
- **Same day, on his word** ("it will be the same issue with every app we install. The AI must just
  be able to understand"): the advice became behaviour. The desktop hand takes the screen look
  itself when `look` finds no window, when nothing lists one, or when `open_app` leaves none — the
  answer comes back with the picture in it — and the two special-case rules left the prompt. Asking
  the user what they want is allowed again; only asking for leave to act is refused.
- **Same day again** ("i told it just now to open blender and its first thought was to install it…
  apparently is not registered to its toolset"): nothing told a model what this machine already
  holds. A plain `apt-get install` whose packages dpkg already has is answered, not run; anything
  else (reinstall, fix, local .deb, pinned version) runs as written. A program with no desktop
  entry is no longer called uninstalled — `open_app` says to run it and to install only if that
  fails.
- **Same day, once more** ("blender successfully opened. But after that the ui appears as this is
  still working… the llm is doing nothing"): it had started Blender with `run_command blender`, and
  a program with a window never gives the command line back — the machine hand sat on it in
  silence to the 30-minute bound. A plain program name that has a desktop entry is now answered,
  not run ("open it with open_app blender"); a command-line use, anything carrying an option, runs
  as written. The screen grab runs under `timeout` too: it was the one action with no bound at all.
- **2026-09-21** ("8k window? only? isnt that as much context as i give it from LM Studio?"): it
  was not. Ollama is sent `num_ctx: 8192`, but LM Studio and the cloud runners are sent no context
  size at all — theirs is whatever the server loaded — so the hardcoded 8192 was only ever a guess
  about them, and its one real consequence was `look_cap` = context/200: forty controls listed per
  `look`, on a model holding four times that. The Model card now carries a bar, *How much it can
  hold*, with a notch per size a runner offers (8k to 256k); it writes `AI_OS_CONTEXT` into the
  same systemd drop-in the model choice already uses, and switching model keeps it. The step
  summary's own limits (last 6 steps in full, 36 in the list, 3000 characters of blueprint) are
  still fixed numbers and do not widen with it.
- **2026-09-23** ("It Aknowledges the task, but It doesnt perform the task given"): three times
  running — "sounds good. do it", "proceed" — the local model answered the front door with a
  `reply` that said "I will register Blender… let me proceed", and a reply runs nothing. A reply
  that promises work ("I will", "I'll", "let me", "I'm going to", "proceeding") is now asked once
  more with only `start` and `housekeep` in the grammar; the front door also says a reply runs
  nothing. Second finding, same run: after a stop, the next chat picked Blender straight back up —
  the front door's last four messages were never dropped, by Stop, Clear or a restart. The owner's
  call: Stop only stops (the chat stays, to say it again differently); Clear — the title bar's,
  and a new one on the Stopped card — sends `clear`, the service deletes the messages and
  broadcasts `cleared`, and every rail empties its screen on it. Projects, notebooks and standing
  instructions stay. (v0.8.6)
- **2026-09-23** ("I told it to not do a task it thought it should continue, yet it did"): after
  a Clear, "Hello AI" was answered with the Blender plan. Clear kept the standing instructions, and
  those held "(Noted for the future: …Blender… a Models folder)". "Clean your memories" had no move
  that could: `remember` only adds. The local model picked `housekeep`, planned the old Blender work
  from its notes, and noted one more. Clear now deletes the standing instructions too (projects and
  notebooks stay), and the front door says a request to forget is a reply pointing to Clear.
  Same day ("the context bar resets to 8k every time i open models"): three leaks. An update folded
  the model choice back into the unit and deleted the drop-in, but not `AI_OS_CONTEXT`, so every
  update reset it. A model button re-applied the size held when the card opened, not the bar as
  moved. And Ollama was sent a fixed `num_ctx: 8192` on every request, which reloads the model at
  8k whatever was set. The update keeps the size, the buttons read the bar, and Ollama is asked for
  `AI_OS_CONTEXT`. (v0.8.7)
- **2026-09-23** ("it still appears to be hogging old projects"): on v0.8.7, "Hi" was answered
  with the car-rental project. The front door lists every project, and the local model offered
  the only one. It then noted that project's description as a standing instruction on a job about
  the skill book. The front door now says never to bring a project up unprompted. The engine keeps
  a `remember` only when the user's own words ask for it ("always", "never", "from now on",
  "remember"…); a note the model makes up is dropped. Same run: a working job's plan had "ask the
  user whether…" steps. `ask` was in the grammar while working but not in the words ("Legal moves
  now: act, replan, done, give_up"), so the model wrote its questions into `ask_questions.json`,
  wrote it again, and the repeat guard gave up the job. The working hint now names `ask`. (v0.8.8)
- **2026-09-23** ("it doesn't actually OWN the OS… what installed apps already exist"): the
  machine map, `2026-09-23-machine-map-design.md`. Every prompt starts with *This machine*: the
  system line, the desktop entries (open_app names), the tools on PATH, and what the AI installed
  itself (an `installed` table the engine fills from successful apt/snap/flatpak commands; Clear
  keeps it). The system text says to install the standard way so the scan finds it. Skills shows
  *Installed on this computer* first, read-only. Discovery only, no usage guide (the owner's call).
  (v0.9.0)
- **2026-09-23** ("its memories are still attached to the previous project and it never starts
  FRESH"): after Clear, "Hi" started housekeeping on car-rental-broker. Two causes. The front door
  listed every project, and the 9B picked the only one whatever the prompt said: it now lists only
  the projects the message names (the name, or a 4+ letter word of it). And Clear left a job
  waiting on a question, so the next message went to it as the answer: Clear now stops an open
  job, the way "stop" does. (v0.9.1)
- **2026-09-23** ("it doesn't have permission to create files… stuck on create the project
  folder"): it had them — it wrote /opt/flappy_bird/BLUEPRINT.md. The job turn said only "its
  folder is the working directory", so the 9B planned "create the project folder and set
  projects_root" and took the v0.9.0 /opt rule (meant for loose program downloads) as the place.
  The job turn now gives the folder's absolute path and says it is already made; the /opt rule
  says "never a project". And in the engine, a project job's BLUEPRINT.md outside its folder or a
  projects_root change is a failed step naming the folder, never run. (v0.9.2)
- **2026-09-23** ("clear all local project work"): the housekeeping job was never told where
  projects live, and its only line about projects was the recipe for *moving* them. So it
  planned "set projects_root", guessed /home/user/projects, and ticked "list" and "delete" with
  `write_file` of that path (a plain file, rewritten each step). Every job turn now opens with
  *Where things are*: the home folder, projects_root (the install's folder or the user's
  setting), each project folder that exists, and the scratch folder. The housekeeping header says
  to remove projects with `rm -rf` on those folders, and the moving recipe is only for a move. A
  `write_file` of the path the last successful step wrote is a failed step, which names `ls`,
  `rm -rf` and `mkdir -p`. The file hand says "wrote a file of N bytes at <path>", not
  "written". An `act` for a plan step that does not exist is rejected. A working job asks only
  what the user's words and answers leave open.
- **2026-09-23**, the same run and a review of the whole engine for the same kind of fault:
  - A project the user places ("my site is in ~/work/site") is registered in that folder:
    `start` takes an optional `folder`. A known project whose folder is gone gets it made again.
  - `run_command` has no shell, so `rm -rf dir/*` removed nothing and still said ok. A command
    with `*` (for file commands), `~`, `$`, `|`, `>`, `&&` or `;` is sent back with "use
    bash -c", and the rules say so.
  - Cut output says it was cut, and a `read_file` past the end says how long the file is.
  - A command that works clears earlier command failures, so installing what was missing and
    running again is a retry, not a refusal.
  - Failure counts and the card's ticks restart with each plan (`plan_from`).
  - A write, edit or setting is never a `done` check.
  - Older step lines say what each step was ("ran ls -la /data/projects"), not just its kind.
  - A plan step out of range costs a replan, not one of the two fatal rejections.
  - The ask rules say to ask only what the user's words leave open, both before planning and
    while working. (v0.9.3)
- **2026-09-23**, the owner's chat after v0.9.3: "delete it" (new-project) got "I cannot delete
  the project … deletion is not a housekeeping task", and "where is it" a made-up
  /home/new-project. Only jobs were told *Where things are*; the front door never was, and its
  housekeep said "folders, settings, tools". The front door now opens with *Where things are*
  too, its housekeep names listing, moving and deleting files, folders and projects, and it says
  the AI has full access. A reply that says it cannot ("I cannot", "I'm unable") is asked again
  with only start and housekeep, as a reply that promises work already was; the Clear-button
  answer to "forget" is left alone.
- **2026-09-23**, the same chat: "create a new project folder… name it Project Flappy" and then
  the game's description were each taken as housekeeping (the front door said housekeep was for
  "folders"). That job asked where to put the folder, then ran mkdir -p three times, and the two
  repeats were two rejections, which is fatal. The front door now says a new project, or a folder
  for one, is always start (the engine makes its folder), and housekeep never makes a project.
  Repeating an action that just worked is not run and costs a replan (five, refilled by any step
  that works), not a rejection.
- **2026-09-23**, the same chat: the Flappy job wrote game.js, asked its questions, and wrote
  game.js again with the answers in it. v0.9.3's "you just wrote this same file" refused that,
  and the retry was refused as already failed: dead. That rule now holds only names with no
  extension (a folder's name written as a file, the fake ticks it was made for). (v0.9.4)
- **2026-09-23**, the owner after Clear: "delete any flappy bird plans. we start a new project"
  searched the project folders, asked where, and gave up after six replans. Flappy's files were
  made by a housekeeping job (before v0.9.4 sent new projects to start), and a housekeeping job
  wrote nothing down. The owner's rule: each message starts clean, and every task updates the
  documentation the next chat reads. Every finished job now appends one line to
  `<scratch>/JOURNAL.md` (engine-written, no model turn): project or housekeeping, how it
  ended, the request, the closing words, and the paths it touched (files written, absolute paths
  in commands that worked). The front door and each housekeeping turn get only the lines that
  share a telling word with the request (a stop list drops "delete", "project" and the like), at
  most five; a project job has its blueprint instead. Clear leaves the journal. A housekeeping
  job asked for a project too does the rest and says the project is to be asked for. (v0.9.5)
- **2026-09-24**, one loop (spec `2026-09-23-one-loop-design.md`): the front door, the job types,
  ask-then-plan, the blueprint gate, the done check and the rejection budget are gone. A message
  starts a turn; the model makes one move at a time (reply, ask, todo, act, remember) over the
  chat since Clear, with a context block that pins the request and the to-do list. New hands:
  web_read, web_search, key, scroll, drag, start_program, program_output, stop_program, wait.
  Words sent mid-work join the turn. The engine holds only Stop, 100 actions, the same failure
  3/5 times, and the notes reminder. (v0.10.0)
- **2026-09-24**, no ask loop: the answer to a question used to start a turn whose pinned request
  was the answer alone, and a 9B asked again round after round. The answer now carries the
  request and the question ("(you asked) … (their answer) …"), and that turn cannot ask again (a
  Clear since the question drops it). The brief: ask what the user wants, never how. (v0.10.1)
- **2026-09-24**, no to-do loop: a 27B re-sent the same one-item list every 20 seconds and never
  acted, and only the 200-move cap would have ended it. Right after a to-do list, todo is out of
  the allowed moves; one sent anyway is answered, not shown. (v0.10.2)
- **2026-09-24**, no repeat loop: a 27B enlarged one screen square some twenty times in Blender
  and never clicked; only failures were counted. Any action but key, scroll, wait and
  program_output now warns at three identical in a row and ends the turn at five. (v0.10.3)
- **2026-09-24**, scripting first (the owner): a program that can be scripted or run from the
  command line (blender --background --python, libreoffice --headless, gimp -b, inkscape
  --actions, ffmpeg) is worked that way, not through its screen, and the result is opened in the
  program when the user asked for it. Also: a stop typed behind a queued request is counted, not
  cleared before the request runs. (v0.10.4)
- **2026-09-24**, projects kept apart (the owner): a project is any folder with a BLUEPRINT.md
  under the projects folder, also inside a group folder (WEb Games/Pool Game), and its notes name
  its other documents (designs, plans). A message naming another project than the chat's starts a
  fresh chat, said on screen; a restart does too; standing instructions stay. A project is named
  by a telling word only ("a snake game" is not Pool Game). One guard for moves that change
  nothing twice (todo, remember); after acting on an answer a new question may come. (v0.10.5)
- **2026-09-24**, the sidebar (spec `2026-09-24-watchers-and-sidebar-design.md` §3): a menu on
  the left (Chat, Projects, Skills; Model, Cloud, Help) picks the page; the header keeps Clear
  and Stop. The Projects page lists every project, newest first, from `Request::Projects`; the
  project list lives in `core::projects`. Answers stream: the answer timeout is the silence
  allowed, not the whole answer; Stop drops the stream; the status line counts the words; the
  chat leaves a quarter of the window for the answer. (v0.11.0)
- **2026-09-24**, watchers and alerts (spec `2026-09-24-watchers-and-sidebar-design.md` §1-2, §4):
  timers, checks and live programs in a `watchers` table, each with its maker's reason; the
  service's scheduler runs them every 15 s and `ai-os-alert` lets a program raise one. An alert
  is a turn of its own in its own chat (`messages.chat`), taken between the owner's turns; an
  urgent one parks the turn in hand, which then carries on. Pop-up, orange Alert card, and a
  Watchers page with Pause/Resume/Delete. (v0.12.0)
- **2026-09-25**, small steps (the owner's reports on v0.11.0): `append_file` adds to a file's end,
  and the brief says a file over about 150 lines goes in parts and "small steps" means one per
  turn. A project's top-level files come with its notes; a new file in a project is noted in its
  BLUEPRINT.md at once, from the move's own thought; writing one file again and again counts as
  the same action for the repeat guard. (v0.12.0)
- **2026-09-25**, stuck answers and cloud caps (the owner's v0.12.0 run): an answer that streams
  300 pieces in a row with nothing to read (no letter, no thought) is dropped as stuck and asked
  once more; the 27B had written 40,000 tokens of blank space in 4 hours behind "33 words".
  Ollama requests no longer send `num_predict`: ollama.com refused the window's size as a length
  limit (nemotron-3-super caps at 65536). (v0.12.1)
- **2026-09-25**, cloud models found and switched (the owner: "the user should see what models
  exist and should be able to also manually pick a different model"): each Cloud account has
  Change model, the provider's list for its key again. When an account's model fails or is used
  up, the pool asks the provider what else the key opens (`aios_proto::chat_models`, the same
  filter as the card), tries up to three, keeps the first that answers in `cloud.tsv`, and says
  so once in the chat (`Model::news`). (v0.12.1)
