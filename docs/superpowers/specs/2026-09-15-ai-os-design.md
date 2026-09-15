# AI OS — Design

**Date:** 2026-09-15
**Status:** Design for review, version 2 after the engineering audit. Nothing is built or installed.

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

---

## 4. How it is built

```
┌──────────────────────────────────────────────────────────────┐
│  USER   desktop chat panel · handed-over windows · phone      │
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
- One multimodal model is loaded (decision 13). On 8 GB an 8B model leaves room for roughly 16k tokens of context, so every step gets a fixed budget: job state, the searched ability entries, and the latest evidence must fit. This is why the ability map is searched, never read whole.
- The model's tool calls are forced through a grammar (JSON schema) so they always parse. Small models otherwise emit a broken call every few percent of steps, which is enough to wreck a long job.
- A bigger model on another machine (pc-worker, 20 GB) is a "remote local" option: private, same switch as cloud, and the cheapest way to prove that a better model gives better results.
- Cloud is a switch per job (decision 6). It is never a silent fallback.

### 4.4 Hands and senses (the basic abilities)
Tried in this order, most dependable first:
1. **Direct:** commands, files, system services, program command lines and APIs.
2. **Program controls:** the accessibility layer, which exposes buttons, fields, menus and text. This is how window handover works (decision 8).
3. **Screen:** screenshot plus a pixel click. Only a fallback, and it is clearly shown to the user while it happens. On Wayland (the only option on GNOME in 26.04) this goes through the desktop's permission system, so the consent must be granted once and remembered; if the desktop cannot remember it, this path is asked for per session.

Other abilities: web browsing, installing and removing software, desktop notifications, Telegram messages to the user.

### 4.5 Ability map and skills library
- Each entry records: what it does, what it needs, what it produces, what must be true first, its side effects, how to check it worked, and how to recover.
- Updates itself when software is installed or removed.
- For each request, the core **searches** the map for relevant entries rather than reading the whole map, because small models have limited room.
- New skills are saved only after their check passed, and they are built from the executor's action log (the structured steps that actually ran), never from the model's own description of what it did. A skill containing admin steps is shown to the user before it is kept.

### 4.6 Memory
- The user (preferences, projects), past jobs, and which methods worked or failed.
- Stored outside the model, so it survives a model swap.

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

### 4.9 User interface
- A chat panel on the desktop, always available. It shows running jobs, progress and results, and has pause, cancel and take-over buttons.
- Handing over a window: the user tells the AI to take that window.
- Phone: urgent notifications only in version 1.

---

## 5. Where it runs

### 5.1 The workshop (this laptop)
- **Machine:** ALIEN, with an Intel Core 7 240H (16 threads), 32 GB RAM, an RTX 5060 Laptop GPU with 8 GB, and 376 GB free disk.
- **WSL2 limits:** 20 GB RAM, 12 threads, disk capped at 100 GB (his call, 2026-09-15: the C: drive must keep its headroom). Everything lives in one folder, `C:\WSL\ai-os\`.
- **Base:** the newest Ubuntu LTS (26.04). Fall back to 24.04 if the trial finds the WSL image or GPU support not ready. Note: 26.04's GNOME has no X11 session at all, so the screen fallback must work through Wayland's permission system; KDE still offers X11, which counts in its favour if GNOME's permissions cannot be remembered.
- **Trial limits:** the desktop shown through Remote Desktop is a headless session with no real input devices, so permission dialogs and handover may behave a little differently from bare metal. Also, Windows App Control is enforced on this laptop; WSL and Remote Desktop are Microsoft-signed and should pass, but the trial confirms it.
- **Setup:** everything is installed by one setup script, never by hand.
- **Desktop:** WSL normally shows single Linux apps as Windows windows, not a whole desktop. The workshop runs a full Linux desktop and shows it on Windows through a Remote Desktop window.
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
| **0. Trial** (throwaway) | Answer the unknowns: GPU inside WSL; a full desktop on Windows; accessibility quality in GNOME vs KDE on real apps (Firefox, LibreOffice, VS Code, Chromium), including whether working a window steals the user's keyboard focus; whether Wayland's screenshot and input permission can be granted once and remembered; a snapshot disk mounted into WSL with rollback proven; one multimodal 8B model measured for speed per step, tool-call parse rate with a grammar, and how much context fits | A findings report and the GNOME/KDE choice |
| 1. Foundation | Core loop, executor, direct abilities, undo, chat panel | It can do system jobs and prove them |
| 2. Handover | Program controls (accessibility) plus the screen fallback | It can work a window the user hands it |
| 3. Skills | Ability map, skills library, learning new skills | It improves on repeated jobs |
| 4. Watch | Background jobs, desktop and Telegram notifications | It can monitor and inform |
| 5. Create | Local image generation and other creative tools | It can make graphics and assets |
| 6. Fine-tune | Train the open model on logged OS jobs | Our own model |
| 7. Package | WSL install file, then USB installer | Installable on other computers |

---

## 8. Open questions (not blocking the trial)

- GNOME or KDE: the trial decides, based on how well real apps expose their controls and whether screen permissions can be remembered.
- The minimum hardware for other computers: decided from the evaluation.
- Product name.

Settled by the audit: undo uses a snapshot disk on both targets (4.8); "urgent" is fixed when a watch job is created (4.2).

## 9. The honest risk

None of the engineering above is the hard part. The hard part is whether an 8B local model is smart enough to drive the loop. The expectation: system jobs and scripted builds work; window handover partly; "create an app from scratch" poorly until a bigger model sits behind it. The architecture is right precisely because that upgrade is a configuration change, and section 6 measures it.

## 10. Out of scope

- A new kernel.
- Training a model from scratch.
- Using an existing agent as the brain.
- A literal second mouse pointer: tested, and most apps ignore it.
- Giving the AI jobs from the phone: a later step, not version 1.
- A general admin shell for the model, in any form.
