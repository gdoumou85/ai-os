# AI OS — Design

**Date:** 2026-09-15
**Status:** Design for review. Nothing is built or installed.

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
| 9 | **Risky actions** | Anything undo can reverse, the AI does on its own: installing, removing, configuring. It asks first before sending messages, paying, deleting personal files, or closing a window with unsaved work. |
| 10 | **"Deterministic" = dependable** | The job gets finished and a check proves it. Exact repeatability applies only to saved procedures, which replay the same steps. |
| 11 | **Notifications** | Routine messages appear on the desktop. Urgent ones also go to the user's phone through a private Telegram bot. |
| 12 | **The model never holds admin rights** | A separate executor holds the keys and checks every action against the rules. |

---

## 3. Design rules that follow from the purpose

1. **The system is the product; the model is a replaceable part.** Abilities, memory, checks and undo do not depend on any one model. Swapping in a better model improves results with no redesign.
2. **It starts from a person's basic abilities, not from a task list.** It can look at the screen, use programs, run commands, read and write files, browse the web, and install software. Every job is a combination of these.
3. **It knows what it can do, and learns what it cannot do yet.** It keeps a live map of its abilities and looks up what fits each request. When nothing fits, it works the job out from basic abilities. If the result passes its check, the method is saved as a new skill.
4. **Saying "done" is never proof.** Only a check counts: the app starts, the file opens, the tests pass, the page responds. When quality is a matter of taste (design, graphics), the user judges.
5. **Content is data, never orders.** Text found on websites, in documents or inside apps can never give the AI new permissions or new instructions.

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
│  EXECUTOR   holds admin rights · checks rules · logs every   │
│             action · takes a snapshot before system changes  │
├──────────────────────────────────────────────────────────────┤
│  UNDO       snapshots · instant rollback                     │
├──────────────────────────────────────────────────────────────┤
│  UBUNTU     kernel, drivers, desktop (unchanged)             │
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
| **Watch** | "Monitor this and tell me" | Never "finishes". Survives reboots, wakes on a timer or an event, and decides whether a finding is routine or urgent |

### 4.3 Model layer
- A model runner (llama.cpp or Ollama class) loads model files and runs them on the GPU. It makes no decisions; it is an engine part.
- On 8 GB of VRAM only one model is loaded at a time. The text model and the screenshot-reading model are swapped in as needed.
- Cloud is a switch per job (decision 6). It is never a silent fallback.

### 4.4 Hands and senses (the basic abilities)
Tried in this order, most dependable first:
1. **Direct:** commands, files, system services, program command lines and APIs.
2. **Program controls:** the accessibility layer, which exposes buttons, fields, menus and text. This is how window handover works (decision 8).
3. **Screen:** screenshot plus a pixel click. Only a fallback, and it is clearly shown to the user while it happens.

Other abilities: web browsing, installing and removing software, desktop notifications, Telegram messages to the user.

### 4.5 Ability map and skills library
- Each entry records: what it does, what it needs, what it produces, what must be true first, its side effects, how to check it worked, and how to recover.
- Updates itself when software is installed or removed.
- For each request, the core **searches** the map for relevant entries rather than reading the whole map, because small models have limited room.
- New skills are saved only after their check passed.

### 4.6 Memory
- The user (preferences, projects), past jobs, and which methods worked or failed.
- Stored outside the model, so it survives a model swap.

### 4.7 Executor
- The only part with admin rights.
- Checks every action against the risky-actions rule (decision 9) before running it.
- Logs every action together with the job that caused it.

### 4.8 Undo
- Takes a snapshot before every system change: install, remove, configure, and file operations outside the job's own workspace.
- Rolls back instantly.
- **Limit:** unsaved work inside an open program cannot be snapshotted. Decision 9 covers this: the AI asks before closing or discarding a window with unsaved work.

### 4.9 User interface
- A chat panel on the desktop, always available. It shows running jobs, progress and results, and has pause, cancel and take-over buttons.
- Handing over a window: the user tells the AI to take that window.
- Phone: urgent notifications only in version 1.

---

## 5. Where it runs

### 5.1 The workshop (this laptop)
- **Machine:** ALIEN, with an Intel Core 7 240H (16 threads), 32 GB RAM, an RTX 5060 Laptop GPU with 8 GB, and 376 GB free disk.
- **WSL2 limits:** 20 GB RAM, 12 threads, disk capped at 200 GB. Everything lives in one folder, `C:\WSL\ai-os\`.
- **Base:** the newest Ubuntu LTS (26.04). Fall back to 24.04 if the trial finds the WSL image or GPU support not ready.
- **Setup:** everything is installed by one setup script, never by hand.
- **Desktop:** WSL normally shows single Linux apps as Windows windows, not a whole desktop. The workshop runs a full Linux desktop and shows it on Windows through a Remote Desktop window.
- **Removal:** unregister the distro, switch WSL off, delete the folder.
- **User step needed:** installing WSL needs admin rights and a reboot.

### 5.2 The product
- The same setup script builds the WSL install file, and later the USB installer.
- The minimum hardware for other computers is set from the evaluation results, not guessed.

---

## 6. Evaluation plan

1. **Public test set:** a subset of OSWorld, about 370 real jobs on an Ubuntu desktop, each with an automatic pass/fail check.
2. **Our own set:** about 30 jobs covering system jobs (install, configure, repair), window handover, build, create and watch.
3. **Every job runs 3–5 times.** Measured for each job:
   - success rate (dependable),
   - same outcome each run (repeatable),
   - time taken,
   - how often it asked the user,
   - how often undo was needed.
4. **Watch jobs:** we trigger a change on purpose and measure that the right message arrives on the right channel in time.
5. **Model comparison:** the same jobs run with different models, which proves that a better model gives better results and picks the model.
6. **The rule:** the AI saying "done" never counts. Only the check does.

---

## 7. Build order

Each phase gets its own plan and its own approval.

| Phase | What | Output |
|---|---|---|
| **0. Trial** (throwaway) | Answer the unknowns: GPU inside WSL; a full desktop on Windows; accessibility quality in GNOME vs KDE on real apps (Firefox, LibreOffice, VS Code, Chromium); speed and tool choice of a local 8B model; how undo works inside WSL | A findings report and the GNOME/KDE choice |
| 1. Foundation | Core loop, executor, direct abilities, undo, chat panel | It can do system jobs and prove them |
| 2. Handover | Program controls (accessibility) plus the screen fallback | It can work a window the user hands it |
| 3. Skills | Ability map, skills library, learning new skills | It improves on repeated jobs |
| 4. Watch | Background jobs, desktop and Telegram notifications | It can monitor and inform |
| 5. Create | Local image generation and other creative tools | It can make graphics and assets |
| 6. Fine-tune | Train the open model on logged OS jobs | Our own model |
| 7. Package | WSL install file, then USB installer | Installable on other computers |

---

## 8. Open questions (not blocking the trial)

- GNOME or KDE: the trial decides, based on how well real apps expose their controls.
- The undo method inside WSL: the trial decides.
- Who decides "urgent": the AI, within rules the user sets.
- The minimum hardware for other computers: decided from the evaluation.
- Product name.

## 9. Out of scope

- A new kernel.
- Training a model from scratch.
- Using an existing agent as the brain.
- A literal second mouse pointer: tested, and most apps ignore it.
- Giving the AI jobs from the phone: a later step, not version 1.
