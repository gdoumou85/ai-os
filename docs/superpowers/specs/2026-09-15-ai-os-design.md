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
| 13 | **One model that reads text and images** | Swapping a text model and a vision model on 8 GB costs seconds per swap and makes a screen-driven loop unusable. One multimodal model is loaded, with a fixed context budget per step. |

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
- Stops a failing approach after a set number of retries and tries another way, or asks the user.

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
- **The chat is not memory** (his rule, 2026-09-16; detail in the Phase 1b design). A local model on an 8k budget cannot carry a long history and the design does not pretend it does. Three places remember instead: **standing instructions** (things the user told it to keep, re-read on every message), the **project blueprint** (one small `BLUEPRINT.md` per project — what it is, the decisions, how to run and check it, what is left; the code and the blueprint are the truth; read first every time it works there; **updated before any change**, kept as small as possible, edited by replacing lines, never piling on), and the **job record** (working memory for the running job only). The model is told: if it is worth remembering, write it down; you will not see this conversation again.

### 4.7 Executor
- The model's only hands. The core turns model output into structured actions; nothing the model writes is ever run as a shell string with admin rights.
- Checks every action against the risky-actions rule (decision 9) before running it, as code. Holds the secrets (rule 6). Logs every action together with the job that caused it.
- Runs actions through three workers, each as separate as Linux allows:
  - **Sandbox worker:** free commands, program command lines, builds, tests. Runs as its own Linux user inside the job's workspace. Cannot see personal files, cannot touch the system.
  - **Desktop worker:** runs inside the user's own desktop session, because program controls (accessibility) only exist there. It accepts program-control actions only (click this, type here, read that) and nothing else.
  - **Admin worker:** a fixed menu of operations: install or remove a package, change a config file, enable or disable a service, write a file outside the workspace. Each has its own rule, its own check and its own snapshot. No general command exists here.
- Personal files are reached only through the executor's file operations, which apply decision 9.

### 4.8 Undo
- Everything the AI is allowed to change (installed software, configuration, workspaces, the user's data) lives on one **snapshot disk**. The base system underneath is read-mostly. On WSL that disk is a mounted virtual disk, because WSL cannot boot from a snapshot filesystem; on bare metal it is a partition. The undo code is the same both ways.
- A snapshot is taken before every system change and before file operations outside the job's workspace.
- **Files roll back instantly. System changes roll back plus a restart of what changed**, because running programs keep the old version until they restart.
- **Limit:** unsaved work inside an open program cannot be snapshotted. Decision 9 covers this: the AI asks before closing or discarding a window with unsaved work.

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
| 3. Skills | Ability map, skills library, learning new skills | It improves on repeated jobs |
| 4. Watch | Background jobs, desktop and Telegram notifications | It can monitor and inform |
| 5. Create | Local image generation and other creative tools | It can make graphics and assets |
| 6. Fine-tune | Train the open model on logged OS jobs | Our own model |
| 7. Package | WSL install file, then USB installer | Installable on other computers |

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

**Phase 1b design agreed 2026-09-16** — `2026-09-16-phase1b-core-loop-design.md`: conversation front door, job loop with clarifying questions and creative mode, grammar-forced moves (reply / start / ask / plan / act / done-with-check / give_up), swappable model connection (Ollama first), memory outside the chat (standing instructions, project blueprint, job record), `edit_file` + windowed `read_file`. Items 1–3 above are closed inside 1b (its §7). **Phase 1c scope:** admin worker (install / remove / config / service), the fetch-packages action with registry-only network, undo snapshots.

**Workshop-only shortcuts that MUST NOT ship to the product (packaging phase):**
- A sudoers `NOPASSWD: /usr/bin/systemd-run` grant for user `ai` (root-equivalent) — used because the workshop executor runs as `ai`; the product's executor daemon is already privileged and needs no such grant.
- `/data` was made world-writable (777) so the trial db could be created; the product sets `/data` ownership/permissions properly.
