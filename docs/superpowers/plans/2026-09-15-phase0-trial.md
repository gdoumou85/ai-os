# Phase 0 Trial Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Answer the eight unknowns in the design spec with measured results, on a throwaway Ubuntu 26.04 in WSL2, and produce a findings report that picks GNOME or KDE.

**Architecture:** Nothing here is product code. Each task is one experiment: a script that sets something up, a probe that measures it, and a pass/fail line written into the findings report. All Linux-side setup goes through `trial/setup-trial.sh` so the real setup script in Phase 1 can be grown from it. The whole distro is deleted when Phase 1 starts.

**Tech Stack:** WSL2, Ubuntu 26.04 (`.wsl` format), Ollama (model runner for the trial only), GNOME 50 + gnome-remote-desktop, KDE Plasma 6 (nested), AT-SPI via `python3-pyatspi`, xdg-desktop-portal via `python3-gi`, btrfs on a loop image.

**Spec:** `docs/superpowers/specs/2026-09-15-ai-os-design.md` (v2, commit `effba2c`)

## Global Constraints

- Base: Ubuntu 26.04 LTS. Fall back to 24.04 only if the 26.04 WSL image or its GPU support is not ready (spec 5.1).
- WSL2 limits: 20 GB RAM, 12 threads, disk capped at 200 GB (spec 5.1).
- Everything lives in `C:\WSL\ai-os\`. The only other Windows-side file is `%USERPROFILE%\.wslconfig` (spec 5.1).
- Everything Linux-side is installed by script, never by hand (spec 5.1). For the trial that script is `trial/setup-trial.sh`.
- No cloud model is called at any point in the trial (spec decision 6).
- The trial is throwaway: results go into the findings report, the distro is unregistered at the start of Phase 1 (spec 7, Phase 0).
- The executor of this plan runs on Windows. Linux commands are run as `wsl -d ai-os -u root -- bash -lc "<command>"`. The repo is visible inside the distro at `/mnt/c/Users/gdoum/Desktop/projects/ai-os`, referred to below as `$REPO`.
- Every task ends by appending its result to `docs/superpowers/findings/phase0-findings.md` and committing. A failed experiment is a result, not a blocker; record it and move on.
- Time box per task: 3 hours of executor time. When the box runs out, record what was reached and move on.

## The eight unknowns (spec 7, Phase 0)

| # | Unknown | Task |
|---|---|---|
| U1 | GPU inside WSL | 2 |
| U2 | A full desktop shown on Windows | 3 |
| U3 | Accessibility quality on real apps, GNOME vs KDE | 4, 6 |
| U4 | Does working a window steal the user's keyboard focus | 4, 6 |
| U5 | Can Wayland's screenshot/input permission be granted once and remembered | 5, 6 |
| U6 | A snapshot disk in WSL with rollback proven | 7 |
| U7 | One multimodal 8B model: speed per step, tool-call parse rate with a grammar, context fit | 2 |
| U8 | GNOME or KDE | 8 |

## File structure

```
trial/
  wslconfig                 # copied to %USERPROFILE%\.wslconfig
  setup-trial.sh            # the one Linux-side install script; idempotent; sections gated by $1
  probes/
    model_probe.py          # U1/U7: speed, parse rate, context fit, vision
    a11y_probe.py           # U3/U4: AT-SPI on Firefox, Chrome, VS Code, LibreOffice
    portal_probe.py         # U5: RemoteDesktop/ScreenCast portal with a restore token
    snapshot_probe.sh       # U6: btrfs loop image, snapshot, rollback
docs/superpowers/findings/
  phase0-findings.md        # one row per unknown, filled in task by task
```

---

### Task 1: WSL engine check, `.wslconfig`, import the distro

**Files:**
- Create: `trial/wslconfig`
- Create: `trial/setup-trial.sh` (section `base`)
- Create: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: a registered WSL distro named `ai-os` at `C:\WSL\ai-os\`, with systemd running, a user `ai` (uid 1000) with passwordless sudo, and `setup-trial.sh` runnable as `bash $REPO/trial/setup-trial.sh <section>`.

- [ ] **Step 1: Confirm the WSL engine is installed (the user's step is done)**

Run (PowerShell): `wsl --version`
Expected: several lines starting `WSL version: 2.x`. If the command errors, stop and tell the user the engine install did not complete.

- [ ] **Step 2: Write the WSL limits**

`trial/wslconfig`:
```ini
[wsl2]
memory=20GB
processors=12
defaultVhdSize=200GB
```

Run (PowerShell): `Copy-Item trial\wslconfig $env:USERPROFILE\.wslconfig; wsl --shutdown`
Expected: no output. (`defaultVhdSize` applies to distros created after this point, which is why it is set before the import.)

- [ ] **Step 3: Find the exact 26.04 distro name**

Run (PowerShell): `wsl --list --online`
Expected: a row like `Ubuntu-26.04    Ubuntu 26.04 LTS`. Use that first column below. If no 26.04 row exists, use `Ubuntu-24.04` and record "26.04 image not on the WSL list on <date>" in the findings.

- [ ] **Step 4: Install it into our folder, without launching**

Run (PowerShell):
```powershell
New-Item -ItemType Directory -Force C:\WSL\ai-os | Out-Null
wsl --install Ubuntu-26.04 --name ai-os --location C:\WSL\ai-os --no-launch
wsl --list --verbose
```
Expected: `ai-os` listed with version 2, state Stopped. `C:\WSL\ai-os\ext4.vhdx` exists.

- [ ] **Step 5: Write the base section of the setup script**

`trial/setup-trial.sh`:
```bash
#!/usr/bin/env bash
# Phase 0 trial setup. Run as root inside the ai-os distro:
#   bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh <section>
# Sections: base gpu desktop-gnome apps kde snapshot. Each is safe to re-run.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
REPO=/mnt/c/Users/gdoum/Desktop/projects/ai-os
section=${1:?section name required}

base() {
  cat >/etc/wsl.conf <<'EOF'
[boot]
systemd=true
[user]
default=ai
[interop]
appendWindowsPath=false
EOF
  id ai >/dev/null 2>&1 || useradd -m -u 1000 -s /bin/bash -G sudo ai
  echo 'ai ALL=(ALL) NOPASSWD:ALL' >/etc/sudoers.d/ai
  chmod 440 /etc/sudoers.d/ai
  apt-get update
  apt-get install -y curl git python3 python3-gi gir1.2-glib-2.0 jq
  install -d -o ai -g ai /home/ai/probes
  cp "$REPO"/trial/probes/* /home/ai/probes/ 2>/dev/null || true
  chown -R ai:ai /home/ai/probes
}

case "$section" in
  base) base ;;
  *) echo "unknown section: $section" >&2; exit 2 ;;
esac
echo "section $section done"
```

- [ ] **Step 6: Run the base section**

Run (PowerShell):
```powershell
wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh base"
wsl --terminate ai-os
wsl -d ai-os -- bash -lc "whoami; systemctl is-system-running; cat /etc/os-release | head -2"
```
Expected: `section base done`; then `ai`, `running` (or `degraded`, which is fine in WSL), and `PRETTY_NAME="Ubuntu 26.04..."`. If the first launch shows Ubuntu's setup wizard asking for a username, answer `ai`, then re-run the base section.

- [ ] **Step 7: Start the findings report**

`docs/superpowers/findings/phase0-findings.md`:
```markdown
# Phase 0 findings

Started 2026-09-15. Throwaway distro `ai-os` on ALIEN. Spec v2 = `effba2c`.

| # | Unknown | Result | Evidence |
|---|---|---|---|
| U1 | GPU inside WSL | | |
| U2 | Full desktop on Windows | | |
| U3 | Accessibility on real apps | | |
| U4 | Focus stealing | | |
| U5 | Remembered screen permission | | |
| U6 | Snapshot disk with rollback | | |
| U7 | Local 8B: speed / parse rate / context | | |
| U8 | GNOME or KDE | | |

## Task 1: base
- Distro name on the WSL list: <fill in from step 3>
- os-release: <paste PRETTY_NAME>
- systemd state: <paste>
```

Fill the three lines in from the outputs above.

- [ ] **Step 8: Commit**

```bash
git add trial/wslconfig trial/setup-trial.sh docs/superpowers/findings/phase0-findings.md
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: WSL limits, distro import, base setup section"
```

---

### Task 2: GPU inside WSL and the local model (U1, U7)

**Files:**
- Modify: `trial/setup-trial.sh` (add section `gpu`)
- Create: `trial/probes/model_probe.py`
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: Ollama listening on `127.0.0.1:11434` inside the distro with one multimodal model pulled; `model_probe.py <model-tag>` prints a JSON summary with keys `tokens_per_s`, `parse_rate_schema`, `parse_rate_free`, `gpu_share_at_16k`, `vision_ok`.

- [ ] **Step 1: Check the GPU is visible**

Run: `wsl -d ai-os -- bash -lc "nvidia-smi --query-gpu=name,memory.total --format=csv"`
Expected: `NVIDIA GeForce RTX 5060 Laptop GPU, 8151 MiB` (or similar). If `nvidia-smi` is missing, `ls /usr/lib/wsl/lib/` must show it; the Windows NVIDIA driver provides it. If absent, record U1 = FAIL and skip to Task 3.

- [ ] **Step 2: Add the gpu section to the setup script**

Add before the `case` line in `trial/setup-trial.sh`:
```bash
gpu() {
  # Ollama is the trial runner only; the product picks its runner in Phase 1.
  command -v ollama >/dev/null || curl -fsSL https://ollama.com/install.sh | sh
  systemctl enable --now ollama
  # One multimodal model (spec decision 13). Newest 8B-class VL tag on ollama.com/library.
  sudo -u ai ollama pull "${MODEL_TAG:-qwen3-vl:8b}"
}
```
Add `gpu) gpu ;;` to the `case`.

- [ ] **Step 3: Run it**

Run: `wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh gpu"`
Expected: pull completes; `ollama list` shows the tag. Before running, check https://ollama.com/library for a newer 8B multimodal Qwen tag (e.g. a `qwen3.5-vl` or `qwen3.6` family) and pass it as `MODEL_TAG=<tag>`; record the tag used.

- [ ] **Step 4: Write the model probe**

`trial/probes/model_probe.py`:
```python
#!/usr/bin/env python3
"""U1/U7 probe. Usage: model_probe.py <model-tag> [screenshot.png]
Prints one JSON line: tokens_per_s, parse_rate_schema, parse_rate_free, gpu_share_at_16k, vision_ok."""
import base64, json, subprocess, sys, time, urllib.request

URL = "http://127.0.0.1:11434/api/chat"
MODEL = sys.argv[1]
TOOL_SCHEMA = {
    "type": "object",
    "properties": {
        "tool": {"type": "string", "enum": ["run_command", "read_file", "write_file", "click", "type_text", "ask_user"]},
        "args": {"type": "object"},
        "why": {"type": "string"},
    },
    "required": ["tool", "args", "why"],
}
SYSTEM = ("You are the core of an AI operating system. Reply with ONE JSON object only: "
          '{"tool": <one of run_command, read_file, write_file, click, type_text, ask_user>, '
          '"args": {...}, "why": "<one sentence>"}. No prose outside the JSON.')
TASKS = [
    "List the files in the user's home folder.",
    "Open the file /etc/hostname and tell me the machine name.",
    "Create a file notes.txt in the workspace containing the word hello.",
    "Press the Save button in the open LibreOffice window.",
    "Type 'quarterly report' into the document title field.",
    "The user asked to delete all photos. What do you do first?",
    "Install the package htop.",
    "Check whether port 8000 is in use.",
    "Rename report_final.docx to report_v2.docx in the workspace.",
    "Find out how much free disk space there is.",
]

def chat(messages, num_ctx=8192, fmt=None, images=None):
    body = {"model": MODEL, "messages": messages, "stream": False, "options": {"num_ctx": num_ctx, "temperature": 0}}
    if fmt is not None:
        body["format"] = fmt
    if images:
        body["messages"][-1]["images"] = images
    req = urllib.request.Request(URL, json.dumps(body).encode(), {"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.load(r)

def parse_ok(text):
    try:
        obj = json.loads(text)
        return isinstance(obj, dict) and obj.get("tool") in TOOL_SCHEMA["properties"]["tool"]["enum"] and isinstance(obj.get("args"), dict)
    except Exception:
        return False

def parse_rate(fmt):
    ok = 0
    for task in TASKS:
        for _ in range(3):  # 30 calls per mode
            resp = chat([{"role": "system", "content": SYSTEM}, {"role": "user", "content": task}], fmt=fmt)
            ok += parse_ok(resp["message"]["content"])
    return ok / (len(TASKS) * 3)

out = {}
# speed: one 300-token answer
resp = chat([{"role": "user", "content": "Explain in about 300 words how to install a package on Ubuntu."}])
out["tokens_per_s"] = round(resp["eval_count"] / (resp["eval_duration"] / 1e9), 1)
out["parse_rate_schema"] = parse_rate(TOOL_SCHEMA)
out["parse_rate_free"] = parse_rate(None)
# context fit: load at 16k and read the GPU/CPU split
chat([{"role": "user", "content": "hi"}], num_ctx=16384)
ps = subprocess.run(["ollama", "ps"], capture_output=True, text=True).stdout
out["gpu_share_at_16k"] = next((line.split()[-2] + " " + line.split()[-1] for line in ps.splitlines() if MODEL in line), "not loaded")
out["ollama_ps"] = ps.strip()
# vision: only if a screenshot path is given
out["vision_ok"] = None
if len(sys.argv) > 2:
    img = base64.b64encode(open(sys.argv[2], "rb").read()).decode()
    resp = chat([{"role": "user", "content": "What application is shown in this screenshot? One line."}], images=[img])
    out["vision_answer"] = resp["message"]["content"].strip()
    out["vision_ok"] = len(out["vision_answer"]) > 0
print(json.dumps(out, indent=1))
```

- [ ] **Step 5: Run the probe (no screenshot yet; Task 5 supplies one)**

Run: `wsl -d ai-os -- bash -lc "cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/model_probe.py ~/probes/ && python3 ~/probes/model_probe.py qwen3-vl:8b"` (use the tag actually pulled)
Expected: a JSON block. Pass criteria to record:
- `tokens_per_s` ≥ 25 (an 8B Q4 on this GPU should manage this; below 10 means it is running on CPU: check `ollama_ps`).
- `parse_rate_schema` = 1.0 and `parse_rate_free` recorded for comparison (this is the number that justifies decision 13's grammar rule).
- `gpu_share_at_16k` shows `100% GPU`. If it shows a CPU split, re-run with `num_ctx=12288` in the script and record the largest context that stays fully on the GPU.

- [ ] **Step 6: Record U1 and U7 in the findings**

Append to `docs/superpowers/findings/phase0-findings.md`:
```markdown
## Task 2: GPU and model
- nvidia-smi: <name, memory>
- model tag: <tag>
- tokens/s: <n>   parse rate with schema: <n>   without: <n>   GPU share at 16k: <text>
- largest context fully on GPU: <n>
```
Fill the U1 and U7 rows of the table: PASS/FAIL plus the numbers.

- [ ] **Step 7: Commit**

```bash
git add trial/setup-trial.sh trial/probes/model_probe.py docs/superpowers/findings/phase0-findings.md
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: GPU check and model probe (U1, U7)"
```

---

### Task 3: A full GNOME desktop shown on Windows (U2)

**Files:**
- Modify: `trial/setup-trial.sh` (add section `desktop-gnome`)
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: a way to see and use a GNOME 50 Wayland session from Windows, recorded in the findings as one of: **A** headless RDP via the system `gnome-remote-desktop` daemon, **B** `gnome-shell --headless` plus the user daemon, or **C** `gnome-shell --nested` inside a WSLg window. Later tasks run their probes inside whichever works and refer to it as "the GNOME session".

- [ ] **Step 1: Add the desktop-gnome section**

Add before the `case` line in `trial/setup-trial.sh`:
```bash
desktop_gnome() {
  apt-get install -y ubuntu-desktop-minimal gnome-remote-desktop gnome-session gdm3 \
    xdg-desktop-portal xdg-desktop-portal-gnome pipewire wireplumber \
    python3-pyatspi gnome-text-editor gstreamer1.0-pipewire gstreamer1.0-tools gstreamer1.0-plugins-good
  systemctl set-default graphical.target
  # attempt A: system-level headless RDP login (GNOME 46+ "remote login")
  grdctl --system rdp set-credentials ai trial-only || true
  grdctl --system rdp enable || true
  systemctl enable --now gnome-remote-desktop.service || true
  systemctl enable gdm3 || true
}
```
Add `desktop-gnome) desktop_gnome ;;` to the `case`. (`trial-only` is a throwaway password; port 3389 is reachable from this laptop only under WSL's default NAT networking.)

- [ ] **Step 2: Run it, then restart the distro**

Run:
```powershell
wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh desktop-gnome"
wsl --terminate ai-os
wsl -d ai-os -- bash -lc "systemctl is-active gnome-remote-desktop gdm3; ss -ltnp | grep 3389"
```
Expected for attempt A: both `active`, and a listener on `:3389`.

- [ ] **Step 3: Attempt A: connect with Remote Desktop**

Run (PowerShell): `mstsc /v:localhost:3389`
Expected: a Windows RDP window shows a GNOME login; log in as `ai` / `trial-only`; a GNOME desktop appears. Take a screenshot of the RDP window (Windows `Win+Shift+S` or the `computer-use` screenshot tool) and save it to `docs/superpowers/findings/u2-gnome-rdp.png`. If the window shows a black screen or the connection fails, wait 60 s, retry once, then go to attempt B.

- [ ] **Step 4: Attempt B (only if A failed): headless shell plus user daemon**

Run:
```powershell
wsl -d ai-os -- bash -lc "grdctl --headless rdp set-credentials ai trial-only; grdctl --headless rdp enable; nohup dbus-run-session -- bash -c 'gnome-shell --headless --virtual-monitor 1920x1080 & sleep 5; gnome-remote-desktop-daemon --headless' >~/headless.log 2>&1 &"
wsl -d ai-os -- bash -lc "sleep 8; ss -ltnp | grep 3389; tail -5 ~/headless.log"
```
Then `mstsc /v:localhost:3389` as in step 3. Screenshot to `docs/superpowers/findings/u2-gnome-rdp.png`.

- [ ] **Step 5: Attempt C (only if A and B failed): nested inside WSLg**

Run: `wsl -d ai-os -- bash -lc "MUTTER_DEBUG_DUMMY_MODE_SPECS=1920x1080 nohup dbus-run-session gnome-shell --nested --wayland >~/nested.log 2>&1 &"`
Expected: a window titled "GNOME Shell" appears on the Windows desktop through WSLg. Screenshot to `docs/superpowers/findings/u2-gnome-nested.png`.

- [ ] **Step 6: Record U2**

Append to the findings:
```markdown
## Task 3: GNOME desktop on Windows
- Attempt A (system RDP): <worked / failed: error text>
- Attempt B (headless shell): <worked / failed / not needed>
- Attempt C (nested in WSLg): <worked / failed / not needed>
- Used from here on: <A/B/C>. Screenshot: <file>.
- How to start it again: <the exact command(s)>
```
Fill the U2 row: PASS with the letter, or FAIL if none worked (then Tasks 4–6 run their probes on a nested shell anyway if C gave even a partial window; otherwise record them as blocked).

- [ ] **Step 7: Commit**

```bash
git add trial/setup-trial.sh docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: GNOME desktop shown on Windows (U2)"
```

---

### Task 4: Accessibility on real apps and focus stealing, GNOME (U3, U4)

**Files:**
- Modify: `trial/setup-trial.sh` (add section `apps`)
- Create: `trial/probes/a11y_probe.py`
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: `a11y_probe.py <app-name> <launch-command...>` prints one JSON line per app: `nodes`, `buttons`, `editable`, `menu_opened`, `text_roundtrip`, `focus_stolen`. Task 6 runs the same script on KDE.

- [ ] **Step 1: Add the apps section**

Add before the `case` line in `trial/setup-trial.sh`:
```bash
apps() {
  # Firefox and Chrome as debs (the snap versions add confinement that would muddy the a11y result).
  install -d -m 0755 /etc/apt/keyrings
  curl -fsSL https://packages.mozilla.org/apt/repo-signing-key.gpg -o /etc/apt/keyrings/packages.mozilla.org.asc
  echo "deb [signed-by=/etc/apt/keyrings/packages.mozilla.org.asc] https://packages.mozilla.org/apt mozilla main" >/etc/apt/sources.list.d/mozilla.list
  printf 'Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n' >/etc/apt/preferences.d/mozilla
  curl -fsSL https://dl.google.com/linux/linux_signing_key.pub | gpg --dearmor -o /etc/apt/keyrings/google.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/google.gpg] https://dl.google.com/linux/chrome/deb/ stable main" >/etc/apt/sources.list.d/google-chrome.list
  curl -fsSL https://packages.microsoft.com/keys/microsoft.asc | gpg --dearmor -o /etc/apt/keyrings/microsoft.gpg
  echo "deb [arch=amd64 signed-by=/etc/apt/keyrings/microsoft.gpg] https://packages.microsoft.com/repos/code stable main" >/etc/apt/sources.list.d/vscode.list
  apt-get update
  apt-get install -y firefox google-chrome-stable code libreoffice-writer
  # tell toolkits an assistive technology is present
  sudo -u ai dbus-launch gsettings set org.gnome.desktop.interface toolkit-accessibility true || true
}
```
Add `apps) apps ;;` to the `case`. Run: `wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh apps"`
Expected: `section apps done`; `which firefox google-chrome code libreoffice` prints four paths.

- [ ] **Step 2: Write the accessibility probe**

`trial/probes/a11y_probe.py`:
```python
#!/usr/bin/env python3
"""U3/U4 probe. Run INSIDE the desktop session (needs DBUS_SESSION_BUS_ADDRESS of that session).
Usage: a11y_probe.py <app-name-substring> <launch command...>
Prints one JSON line: nodes, buttons, editable, menu_opened, text_roundtrip, focus_stolen, error."""
import json, os, subprocess, sys, time
import pyatspi

name, cmd = sys.argv[1], sys.argv[2:]
env = dict(os.environ, GNOME_ACCESSIBILITY="1", ACCESSIBILITY_ENABLED="1", QT_LINUX_ACCESSIBILITY_ALWAYS_ON="1")
desk = pyatspi.Registry.getDesktop(0)

def active_window():
    for app in desk:
        try:
            for w in app:
                if w.getState().contains(pyatspi.STATE_ACTIVE):
                    return f"{app.name}:{w.name}"
        except Exception:
            pass
    return None

def find_app():
    for app in desk:
        if name.lower() in (app.name or "").lower():
            return app
    return None

def walk(acc, depth=0, limit=4000):
    stack = [acc]
    while stack and limit > 0:
        node = stack.pop()
        limit -= 1
        yield node
        try:
            stack.extend(node[i] for i in range(min(node.childCount, 200)))
        except Exception:
            pass

out = {"app": name, "nodes": 0, "buttons": 0, "editable": 0, "menu_opened": None, "text_roundtrip": None, "focus_stolen": None, "error": None}
try:
    # a decoy window holds focus; the probe must not move it
    decoy = subprocess.Popen(["gnome-text-editor", "--new-window"], env=env)
    time.sleep(4)
    focus_before = active_window()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    app = None
    for _ in range(30):
        time.sleep(1)
        app = find_app()
        if app and app.childCount:
            break
    if not app:
        raise RuntimeError("app never appeared on the a11y bus")
    time.sleep(5)
    # give focus back to the decoy the way a user would: it is the last window the user clicked
    editable = None
    menu = None
    for node in walk(app):
        out["nodes"] += 1
        role = node.getRoleName()
        if role in ("push button", "toggle button"):
            out["buttons"] += 1
        if role in ("text", "entry", "document web", "document text", "paragraph") and node.getState().contains(pyatspi.STATE_EDITABLE):
            out["editable"] += 1
            editable = editable or node
        if role == "menu" and menu is None and node.name in ("File", "Edit", "View"):
            menu = node
    if menu is not None:
        try:
            menu.queryAction().doAction(0)
            time.sleep(1.5)
            out["menu_opened"] = any(menu[i].getState().contains(pyatspi.STATE_SHOWING) for i in range(min(menu.childCount, 30)))
            menu.queryAction().doAction(0)  # close it again
        except Exception as e:
            out["menu_opened"] = f"error: {e}"
    if editable is not None:
        try:
            et = editable.queryEditableText()
            et.insertText(0, "ai-os probe", len("ai-os probe"))
            time.sleep(1)
            got = editable.queryText().getText(0, -1)
            out["text_roundtrip"] = "ai-os probe" in got
            et.deleteText(0, len("ai-os probe"))
        except Exception as e:
            out["text_roundtrip"] = f"error: {e}"
    time.sleep(1)
    out["focus_stolen"] = active_window() != focus_before
    out["focus_before"], out["focus_after"] = focus_before, active_window()
except Exception as e:
    out["error"] = str(e)
finally:
    for p in ("proc", "decoy"):
        if p in dir():
            locals()[p].terminate()
print(json.dumps(out))
```

- [ ] **Step 3: Run it inside the GNOME session, once per app**

The probe must run on the session bus of the GNOME session from Task 3. Open a terminal *inside* the GNOME session (RDP or nested window: Activities → Terminal), then:
```bash
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/a11y_probe.py ~/probes/
cd ~/probes
python3 a11y_probe.py firefox firefox --new-window about:blank        | tee -a a11y-gnome.jsonl
python3 a11y_probe.py chrome  google-chrome --new-window about:blank  | tee -a a11y-gnome.jsonl
python3 a11y_probe.py code    code --new-window                       | tee -a a11y-gnome.jsonl
python3 a11y_probe.py soffice libreoffice --writer --norestore        | tee -a a11y-gnome.jsonl
```
Expected: four JSON lines. Pass per app = `nodes` > 20, `buttons` > 0, and at least one of `menu_opened` / `text_roundtrip` true. If Chrome or VS Code show `nodes` ≤ 5, re-run them with `--force-renderer-accessibility` appended and record that they needed forcing.

- [ ] **Step 4: Copy the results out and record U3/U4 (GNOME half)**

Run: `wsl -d ai-os -- bash -lc "cp ~/probes/a11y-gnome.jsonl /mnt/c/Users/gdoum/Desktop/projects/ai-os/docs/superpowers/findings/"`

Append to the findings:
```markdown
## Task 4: accessibility on GNOME
| App | nodes | buttons | editable | menu opened | text round-trip | focus stolen | forced? |
|---|---|---|---|---|---|---|---|
| Firefox | | | | | | | |
| Chrome | | | | | | | |
| VS Code | | | | | | | |
| LibreOffice Writer | | | | | | | |
```
Fill from `a11y-gnome.jsonl`. The U3/U4 rows in the table stay open until Task 6 adds KDE.

- [ ] **Step 5: Commit**

```bash
git add trial/setup-trial.sh trial/probes/a11y_probe.py docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: accessibility and focus probe on GNOME (U3, U4)"
```

---

### Task 5: Remembered screen permission and input injection on Wayland, GNOME (U5)

**Files:**
- Create: `trial/probes/portal_probe.py`
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: `portal_probe.py` which, run twice, must show the permission dialog at most once; writes `~/probes/shot.png` (used by Task 2's vision check) and prints `dialog_expected`, `restore_token_saved`, `stream_started`, `pointer_moved`.

- [ ] **Step 1: Write the portal probe**

`trial/probes/portal_probe.py`:
```python
#!/usr/bin/env python3
"""U5 probe. Run inside the desktop session. First run: a permission dialog is expected; approve it.
Second run: must start with NO dialog, using the saved restore token. Also moves the pointer and grabs one frame."""
import json, os, subprocess, sys, time
import gi
gi.require_version("Gio", "2.0"); gi.require_version("GLib", "2.0")
from gi.repository import Gio, GLib

TOKEN_FILE = os.path.expanduser("~/probes/restore_token")
bus = Gio.bus_get_sync(Gio.BusType.SESSION)
sender = bus.get_unique_name()[1:].replace(".", "_")
loop = GLib.MainLoop()
result = {}
counter = [0]

def call(iface, method, params):
    """Call a portal method and wait for its Request Response signal. Returns the results dict."""
    counter[0] += 1
    tok = f"t{counter[0]}"
    req_path = f"/org/freedesktop/portal/desktop/request/{sender}/{tok}"
    got = {}
    def on_resp(conn, s, path, i, sig, p):
        got["code"], got["res"] = p.unpack()
        loop.quit()
    sub = bus.signal_subscribe("org.freedesktop.portal.Desktop", "org.freedesktop.portal.Request", "Response", req_path, None, 0, on_resp)
    params[-1]["handle_token"] = GLib.Variant("s", tok)
    bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", iface, method,
                  GLib.Variant.new_tuple(*[GLib.Variant(t, v) if not isinstance(v, GLib.Variant) else v for t, v in params_types(method, params)]),
                  None, Gio.DBusCallFlags.NONE, -1, None)
    loop.run()
    bus.signal_unsubscribe(sub)
    if got["code"] != 0:
        raise RuntimeError(f"{method} response code {got['code']}")
    return got["res"]

def params_types(method, params):
    # (type, value) pairs per method; the options dict is always last
    if method == "CreateSession":
        return [("a{sv}", params[0])]
    if method in ("SelectDevices", "SelectSources", "Start"):
        return [("o", params[0])] + ([("s", params[1])] if method == "Start" else []) + [("a{sv}", params[-1])]
    raise ValueError(method)

RD = "org.freedesktop.portal.RemoteDesktop"
SC = "org.freedesktop.portal.ScreenCast"
restore = open(TOKEN_FILE).read().strip() if os.path.exists(TOKEN_FILE) else None
result["dialog_expected"] = restore is None

sess_tok = f"s{int(time.time())}"
res = call(RD, "CreateSession", [{"session_handle_token": GLib.Variant("s", sess_tok)}])
session = res["session_handle"]
dev_opts = {"types": GLib.Variant("u", 7), "persist_mode": GLib.Variant("u", 2)}
if restore:
    dev_opts["restore_token"] = GLib.Variant("s", restore)
call(RD, "SelectDevices", [session, dev_opts])
src_opts = {"types": GLib.Variant("u", 1), "persist_mode": GLib.Variant("u", 2)}
if restore:
    src_opts["restore_token"] = GLib.Variant("s", restore)
call(SC, "SelectSources", [session, src_opts])
res = call(RD, "Start", [session, "", {}])
token = res.get("restore_token")
result["restore_token_saved"] = bool(token)
if token:
    open(TOKEN_FILE, "w").write(token)
streams = res.get("streams", [])
result["stream_started"] = len(streams) > 0

# pointer: move to (100,100) then (400,300) on the first stream
try:
    node_id = streams[0][0]
    for x, y in ((100.0, 100.0), (400.0, 300.0)):
        bus.call_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", RD, "NotifyPointerMotionAbsolute",
                      GLib.Variant("(oa{sv}udd)", (session, {}, node_id, x, y)), None, Gio.DBusCallFlags.NONE, -1, None)
        time.sleep(0.3)
    result["pointer_moved"] = True
except Exception as e:
    result["pointer_moved"] = f"error: {e}"

# one frame from the PipeWire stream
try:
    reply, fds = bus.call_with_unix_fd_list_sync("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", SC, "OpenPipeWireRemote",
                                                 GLib.Variant("(oa{sv})", (session, {})), None, Gio.DBusCallFlags.NONE, -1, None, None)
    fd = fds.get(reply.unpack()[0])
    shot = os.path.expanduser("~/probes/shot.png")
    subprocess.run(["gst-launch-1.0", "-q", f"pipewiresrc fd={fd} path={node_id} num-buffers=1 ! videoconvert ! pngenc ! filesink location={shot}"],
                   shell=False, check=True, pass_fds=(fd,), timeout=30)
    result["screenshot"] = shot if os.path.getsize(shot) > 1000 else "empty"
except Exception as e:
    result["screenshot"] = f"error: {e}"
print(json.dumps(result))
```
Note for the executor: `gst-launch-1.0` takes its pipeline as separate arguments; if the single-string form fails, split it on spaces: `["gst-launch-1.0", "-q", "pipewiresrc", f"fd={fd}", f"path={node_id}", "num-buffers=1", "!", "videoconvert", "!", "pngenc", "!", "filesink", f"location={shot}"]`.

- [ ] **Step 2: Run it twice inside the GNOME session**

In the session's terminal:
```bash
cp /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/portal_probe.py ~/probes/
rm -f ~/probes/restore_token
python3 ~/probes/portal_probe.py     # dialog appears: approve "share screen" and "remote control"
python3 ~/probes/portal_probe.py     # must produce NO dialog
```
Expected: first run `dialog_expected: true, restore_token_saved: true, stream_started: true`; second run `dialog_expected: false` and no dialog seen on screen, `stream_started: true`, `pointer_moved: true`, `screenshot: /home/ai/probes/shot.png`. If the second run shows a dialog, U5 on GNOME = FAIL (record "restore token not honoured").

- [ ] **Step 3: Feed the screenshot to the vision check from Task 2**

Run: `wsl -d ai-os -- bash -lc "python3 ~/probes/model_probe.py qwen3-vl:8b ~/probes/shot.png | jq '{vision_ok, vision_answer}'"` (use the pulled tag)
Expected: `vision_ok: true` and an answer that names what is on screen. Add the answer to the Task 2 findings block.

- [ ] **Step 4: Record U5 (GNOME half)**

Append to the findings:
```markdown
## Task 5: screen permission on GNOME
- First run: dialog shown <yes/no>, token saved <yes/no>
- Second run: dialog shown <yes/no>, stream <yes/no>, pointer moved <yes/no>, screenshot <ok/error>
- Vision model read the screenshot as: "<answer>"
```

- [ ] **Step 5: Commit**

```bash
git add trial/probes/portal_probe.py docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: portal permission and input probe on GNOME (U5)"
```

---

### Task 6: The same three probes on KDE Plasma (U3, U4, U5)

**Files:**
- Modify: `trial/setup-trial.sh` (add section `kde`)
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Consumes: `a11y_probe.py` and `portal_probe.py` unchanged from Tasks 4 and 5.
- Produces: the KDE half of U3, U4, U5.

- [ ] **Step 1: Add the kde section**

Add before the `case` line in `trial/setup-trial.sh`:
```bash
kde() {
  apt-get install -y kde-plasma-desktop plasma-workspace-wayland xdg-desktop-portal-kde konsole kate
}
```
Add `kde) kde ;;` to the `case`. Run: `wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh kde"`

- [ ] **Step 2: Show a Plasma session on Windows**

Attempt A (RDP, if Task 3 used A): log out of GNOME in the RDP window, pick "Plasma (Wayland)" at the session chooser, log in. Attempt B: nested inside WSLg:
`wsl -d ai-os -- bash -lc "nohup dbus-run-session -- kwin_wayland --xwayland --width 1920 --height 1080 --exit-with-session=startplasma-wayland >~/kde.log 2>&1 &"`
Expected: a Plasma desktop window on Windows. Screenshot to `docs/superpowers/findings/u2-kde.png`. Record which attempt worked in the Task 3 block of the findings as "KDE shown via <A/B>".

- [ ] **Step 3: Run the accessibility probe on KDE**

In a Konsole inside the Plasma session, with the decoy changed to Kate:
```bash
cd ~/probes
sed 's/gnome-text-editor/kate/' a11y_probe.py > a11y_probe_kde.py
python3 a11y_probe_kde.py firefox firefox --new-window about:blank        | tee -a a11y-kde.jsonl
python3 a11y_probe_kde.py chrome  google-chrome --new-window about:blank  | tee -a a11y-kde.jsonl
python3 a11y_probe_kde.py code    code --new-window                       | tee -a a11y-kde.jsonl
python3 a11y_probe_kde.py soffice libreoffice --writer --norestore        | tee -a a11y-kde.jsonl
cp a11y-kde.jsonl /mnt/c/Users/gdoum/Desktop/projects/ai-os/docs/superpowers/findings/
```
Expected: four JSON lines, same pass rule as Task 4 step 3.

- [ ] **Step 4: Run the portal probe twice on KDE**

```bash
rm -f ~/probes/restore_token
python3 ~/probes/portal_probe.py     # approve the KDE dialog
python3 ~/probes/portal_probe.py     # must produce NO dialog
```
Expected: same pass rule as Task 5 step 2.

- [ ] **Step 5: Record the KDE halves and close U3, U4, U5**

Append to the findings:
```markdown
## Task 6: KDE
| App | nodes | buttons | editable | menu opened | text round-trip | focus stolen | forced? |
|---|---|---|---|---|---|---|---|
| Firefox | | | | | | | |
| Chrome | | | | | | | |
| VS Code | | | | | | | |
| LibreOffice Writer | | | | | | | |
- Portal first run: dialog <yes/no>, token saved <yes/no>. Second run: dialog <yes/no>, stream <yes/no>, pointer moved <yes/no>, screenshot <ok/error>
```
Now fill the U3, U4 and U5 rows of the summary table with "GNOME: … / KDE: …".

- [ ] **Step 6: Commit**

```bash
git add trial/setup-trial.sh docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: accessibility and portal probes on KDE (U3, U4, U5)"
```

---

### Task 7: Snapshot disk with rollback inside WSL (U6)

**Files:**
- Modify: `trial/setup-trial.sh` (add section `snapshot`)
- Create: `trial/probes/snapshot_probe.sh`
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: a btrfs filesystem in a loop image at `/var/lib/ai-os/data.img`, mounted at `/data`, with a `live` subvolume and a `snapshots/` directory; `snapshot_probe.sh` exits 0 only if a file changed after a snapshot is restored by rollback and the mount survives a distro restart.

Why a loop image and not `wsl --mount --vhd`: mounting a VHD needs Windows administrator rights on every WSL start, which the product cannot ask for. A loop image needs nothing from Windows and is the same code on bare metal.

- [ ] **Step 1: Confirm the WSL kernel has btrfs**

Run: `wsl -d ai-os -- bash -lc "grep -w btrfs /proc/filesystems || (sudo modprobe btrfs && grep -w btrfs /proc/filesystems)"`
Expected: a line containing `btrfs`. If absent, record U6 = FAIL ("WSL kernel lacks btrfs") and skip to Task 8.

- [ ] **Step 2: Add the snapshot section**

Add before the `case` line in `trial/setup-trial.sh`:
```bash
snapshot() {
  apt-get install -y btrfs-progs
  install -d /var/lib/ai-os /data
  if [ ! -f /var/lib/ai-os/data.img ]; then
    truncate -s 50G /var/lib/ai-os/data.img      # sparse; the product sizes this from free space
    mkfs.btrfs -q -L ai-os-data /var/lib/ai-os/data.img
  fi
  grep -q ' /data ' /etc/fstab || echo '/var/lib/ai-os/data.img /data btrfs loop,noatime,compress=zstd 0 0' >>/etc/fstab
  mountpoint -q /data || mount /data
  [ -d /data/live ] || btrfs subvolume create /data/live
  install -d /data/snapshots
  chown ai:ai /data/live
}
```
Add `snapshot) snapshot ;;` to the `case`. Run: `wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/setup-trial.sh snapshot && findmnt /data"`
Expected: `findmnt` shows `/data` with `btrfs`.

- [ ] **Step 3: Write the rollback probe**

`trial/probes/snapshot_probe.sh`:
```bash
#!/usr/bin/env bash
# U6 probe: snapshot, change, roll back, prove the change is gone. Run as root.
set -euo pipefail
live=/data/live; snaps=/data/snapshots
echo "version 1" >"$live/file.txt"
t0=$(date +%s%N)
btrfs subvolume snapshot -r "$live" "$snaps/before" >/dev/null
t1=$(date +%s%N)
echo "version 2" >"$live/file.txt"; echo junk >"$live/junk.txt"
# rollback = swap the live subvolume for a writable copy of the snapshot
t2=$(date +%s%N)
btrfs subvolume snapshot "$snaps/before" "$live.new" >/dev/null
mv "$live" "$live.old" && mv "$live.new" "$live"
btrfs subvolume delete "$live.old" >/dev/null
t3=$(date +%s%N)
[ "$(cat "$live/file.txt")" = "version 1" ] || { echo "FAIL: file not restored"; exit 1; }
[ ! -e "$live/junk.txt" ] || { echo "FAIL: junk survived rollback"; exit 1; }
btrfs subvolume delete "$snaps/before" >/dev/null
echo "PASS snapshot_ms=$(( (t1-t0)/1000000 )) rollback_ms=$(( (t3-t2)/1000000 ))"
```

- [ ] **Step 4: Run it, then prove the mount survives a restart**

Run:
```powershell
wsl -d ai-os -u root -- bash -lc "bash /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/probes/snapshot_probe.sh"
wsl --terminate ai-os
wsl -d ai-os -- bash -lc "findmnt -no FSTYPE /data && ls /data"
```
Expected: `PASS snapshot_ms=<n> rollback_ms=<n>` (both well under 1000), then `btrfs` and `live  snapshots`.

- [ ] **Step 5: Measure the write cost of the loop layer**

Run: `wsl -d ai-os -- bash -lc "dd if=/dev/zero of=/data/live/speed bs=1M count=1024 conv=fdatasync 2>&1 | tail -1; dd if=/dev/zero of=/home/ai/speed bs=1M count=1024 conv=fdatasync 2>&1 | tail -1; rm /data/live/speed /home/ai/speed"`
Expected: two throughput lines. Record both; the loop layer should be within 2× of the plain ext4 figure.

- [ ] **Step 6: Record U6**

Append to the findings:
```markdown
## Task 7: snapshot disk
- btrfs in kernel: yes
- snapshot ms: <n>, rollback ms: <n>, mount survived restart: <yes/no>
- write speed on /data: <x> vs plain root: <y>
```
Fill the U6 row.

- [ ] **Step 7: Commit**

```bash
git add trial/setup-trial.sh trial/probes/snapshot_probe.sh docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: btrfs loop image snapshot and rollback (U6)"
```

---

### Task 8: The GNOME/KDE decision and the findings report (U8)

**Files:**
- Modify: `docs/superpowers/findings/phase0-findings.md`

**Interfaces:**
- Produces: the finished report the Phase 1 plan is written from.

- [ ] **Step 1: Score the two desktops**

Append to the findings and fill it in from Tasks 4–6:
```markdown
## Task 8: GNOME or KDE
| Criterion | GNOME | KDE | Weight |
|---|---|---|---|
| Apps passing the a11y probe (of 4) | | | high |
| Apps that needed forcing | | | medium |
| Focus stolen on any app | | | high |
| Permission remembered on second run | | | high |
| Pointer injection worked | | | high |
| Screenshot frame captured | | | medium |
| Shown on Windows via | | | low |
| X11 session still available (escape hatch) | no | | low |

**Decision:** <GNOME/KDE>, because <the high-weight rows that differ>.
```
Rule: a desktop that fails "permission remembered" or steals focus loses, regardless of the rest. If both pass everything, pick GNOME (the Ubuntu default; fewer packages for the product).

- [ ] **Step 2: Write the summary paragraph at the top of the report**

Under the title in `phase0-findings.md`, add a five-line "Summary" listing: the desktop chosen, the model tag and its numbers, the undo method result, the way the desktop is shown on Windows, and anything that changes the spec (for example, "26.04 image not on the WSL list; trial ran on 24.04").

- [ ] **Step 3: Check every table row is filled**

Run (PowerShell): `Select-String -Path docs\superpowers\findings\phase0-findings.md -Pattern '\| *\|' | Measure-Object`
Expected: `Count : 0` for empty cells in the summary table (a manual scan of the eight U-rows is the real check: no row may be blank; "blocked" or "FAIL" with a reason is a valid result).

- [ ] **Step 4: Commit and hand back**

```bash
git add docs/superpowers/findings/
git -c user.name="George" -c user.email="212589141+gdoumou85@users.noreply.github.com" commit -m "trial: phase 0 findings report and desktop decision (U8)"
```
Then report to the user: the summary paragraph, verbatim, and the question "Phase 1 plan next?" The trial distro stays registered until the Phase 1 plan starts, then `wsl --unregister ai-os` and delete `C:\WSL\ai-os\`.
