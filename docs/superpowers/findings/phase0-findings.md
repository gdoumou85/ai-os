# Phase 0 findings

Started 2026-09-15. Throwaway distro `ai-os` on ALIEN. Spec v2 = `effba2c`.

| # | Unknown | Result | Evidence |
|---|---|---|---|
| U1 | GPU inside WSL | PASS | `nvidia-smi` in the distro: RTX 5060 Laptop, 8151 MiB, driver 610.60; Ollama runs the model on it |
| U2 | Full desktop on Windows | | |
| U3 | Accessibility on real apps | MIXED — native apps full-read + action-drive, browsers/Electron shallow | LibreOffice 2099 nodes / 114 buttons; **reads everything, and AT-SPI actions operate it** (Bold toggled false→true via `doAction`, menus opened). **Free-text typing does NOT commit headless** (no seat) — see the seat note below. Chrome 6 buttons + entry (needs `--force-renderer-accessibility`); Firefox shallow (51 nodes); VS Code/Electron never registered |
| U4 | Focus stealing | PASS by design (headless caveat) | AT-SPI actions drove LibreOffice's menus and toggled Bold without needing keyboard focus; the API acts on objects, not the focused window. Free-text keystroke injection is a separate mechanism that needs a real seat (see seat note). Definitive typing test belongs on bare metal / the 20 GB PC |
| U5 | Remembered screen permission | RESOLVED — use the compositor API, not the portal | Portal refuses to persist input sessions AND its Start dialog needs a visible shell; `Shell.Screenshot` blocked; uinput module absent in WSL. Compositor API (`org.gnome.Mutter.ScreenCast`/`.RemoteDesktop`) works dialog-free — proven by the earlier live RDP stream. Primary path (AT-SPI) needs no permission at all |
| U6 | Snapshot disk with rollback | PASS (restart check pending) | btrfs in the WSL kernel; loop image at `/var/lib/ai-os/data.img` mounted on `/data`; snapshot 5 ms, rollback 7 ms, change gone; writes 1.5 GB/s vs 2.1 GB/s on plain root |
| U7 | Local 8B: speed / parse rate / context / vision | PASS with a caveat | qwen3.5:9b: 35–40 tok/s, ~1.2 s per tool call, 100% parse with and without grammar; **read a real desktop screenshot and correctly named the app ("LibreOffice Writer")**; **8k context is the most that stays fully on the GPU** (q8_0 KV cache); 12k+ spills to CPU |
| U8 | GNOME or KDE | | |

## Task 1: base
- WSL engine: 2.7.14.0, kernel 6.18.33.2-2, WSLg 1.0.73.2
- Distro name on the WSL list: `Ubuntu-26.04` (Ubuntu 26.04 LTS)
- Disk cap: 100 GB (`defaultVhdSize`), set by the user on 2026-09-15 instead of the spec's 200 GB
- os-release: `Ubuntu 26.04.1 LTS`
- systemd state: `running`
- Root disk after base: 98 GB total, 1.6 GB used
- No setup wizard appeared on first launch (root-run script, then `[user] default=ai`)

## Task 2: GPU and model
- nvidia-smi inside WSL: NVIDIA GeForce RTX 5060 Laptop GPU, 8151 MiB, driver 610.60
- Runner: Ollama (trial only). Its installer needs `zstd`, which the 26.04 WSL image lacks; added to the gpu section.
- Model tag: `qwen3.5:9b` (6.6 GB, vision + tools + thinking). Chosen because the newer Qwen 3.6 / 3.8 families start at 27B and Gemma 4's smallest dense model is 12B; `qwen3-vl:8b` is the older alternative.
- Probe run with `think: false`, temperature 0, 8192 context:
  - tokens/s: **35.0**
  - tool-call parse rate with JSON-schema grammar: **1.0** (30/30); without grammar: **1.0** (30/30); 1.25 s per call either way
  - The grammar cost nothing in time and this model did not need it for these 10 tasks; keep it anyway (spec decision 13), the failure mode it prevents shows up on long jobs, not on 30 short calls.
- Context fit (`ollama ps` CPU/GPU split):
  - default KV cache: 4096 = 100% GPU; 8192 = 12% CPU; 12288 = 14% CPU; 16384 = 16% CPU
  - with `OLLAMA_FLASH_ATTENTION=1` + `OLLAMA_KV_CACHE_TYPE=q8_0`: **8192 = 100% GPU**; 12288 = 12% CPU; 16384 = 14% CPU
  - **Largest context fully on GPU: 8192** (with the q8_0 cache). VRAM at that point: 6.4 GB of 8.
- Consequence for Phase 1: the per-step context budget on 8 GB cards is 8k for a 9B model, not the 16k assumed in spec 4.3. Either the budget shrinks to 8k, or a 4B-class model is used when more context is needed. The evaluation (spec 6) should include both.
- **This is a workshop limit, not a product limit** (his note, 2026-09-15): the WSL build here is the development version. The 20 GB GPU PC it deploys to runs the same 9B model with its full context on the GPU, and a 27B-class model (`qwen3.6:27b`, ~16 GB at Q4) fits there too. So the model comparison in spec 6 can be run entirely on that PC. The 8 GB numbers define the *minimum* hardware tier, which is exactly what spec 5.2 asks for.
- Vision check: pending the screenshot from Task 5.

## Task 4: accessibility on GNOME (window handover feasibility — the key question)
Probe: read the whole AT-SPI tree, count buttons and editable fields, open a File/Edit/View menu, and type text into the first editable field then read it back. Run headless.

| App | nodes | buttons | editable | menu opened | action drives it | free-text typing (headless) | verdict |
|---|---|---|---|---|---|---|---|
| **LibreOffice Writer** (GTK) | 2099 | 114 | 8 | **yes** | **yes** (Bold toggled false→true) | **no** (needs a seat) | read + operate fully; typing pending a real seat |
| Google Chrome | 15 | 6 | (entry present) | — | — | — | shallow but usable; needs `--force-renderer-accessibility` |
| Firefox | 51 | 2 | — | — | — | — | shallow tree; chrome/toolbar not exposed headless |
| VS Code (Electron) | 0 | 0 | 0 | — | — | — | never registered on the a11y bus |

### Seat note (the corrected typing finding, 2026-09-15)
An earlier run reported "typed & read back = yes" for LibreOffice. **That was a false positive** — AT-SPI `EditableText.insertText` returns success but the text never commits; the status bar stayed "0 words, 0 characters". Verified three synthetic-input paths headless, all "0 words, 0 characters": (a) AT-SPI `insertText`; (b) bare Mutter `RemoteDesktop.NotifyKeyboardKeysym`; (c) a Mutter RemoteDesktop session linked to a ScreenCast monitor with a pointer click first. Free-typed characters need a real input **seat**, which headless WSL does not have (`/dev/uinput` also can't load here, U5).

What **does** work seat-free, proven: **AT-SPI actions** (`doAction`) — clicking buttons, opening menus, toggling formatting — because they act in-process on the object, not through the keyboard. So the AI can *operate* any native app's controls here; it just can't push free text until there's a seat.

This is **not** a model limitation — these probes contain no LLM; they type hardcoded strings. Free-text typing returns automatically on the deploy targets (the 20 GB GPU PC with a display, or bare-metal install). Two seat-free text paths remain to wire in Phase 2: the app's own scripting API (LibreOffice UNO) and clipboard-paste via an action.

**Reading:** accessibility quality is a property of the **toolkit**, not the desktop. Native GTK/Qt apps (LibreOffice is the hardest real case) expose a complete, controllable tree: the AI can read every control, click buttons, open menus and edit text — the whole basis of window handover (spec 4.4 tier 2) — and it does so **without taking keyboard focus**, because AT-SPI acts on objects, not on the focused window (U4). Chromium exposes a usable-but-shallow tree only when forced; Firefox's headless tree is shallow; Electron (VS Code) did not register at all (its a11y tree is built only when a screen reader is detected at launch).

**Consequence for the design (no change needed, it confirms spec 4.4's ordering):**
- Native apps → AT-SPI, tier 2. Works well.
- Browsers → drive through the browser's own automation (CDP/WebDriver), not AT-SPI. Add this as the browser path in Phase 2.
- Electron and anything with no tree → screen fallback, tier 3.
Because AT-SPI is identical on GNOME and KDE, U3/U4 do **not** decide the desktop; U5 does.

## Task 5: screen permission on GNOME (U5)
The question was "can screenshot/input permission be granted once and remembered?" The trial reframed it, because the AI is the session owner, not an untrusted app:
- **freedesktop portal (what a sandboxed app uses):** `SelectDevices`/`SelectSources` accept a persist flag, but a **RemoteDesktop (input) session cannot persist at all** on GNOME (`InvalidArgument: Remote desktop sessions cannot persist`), and `Start` shows a consent dialog that needs a visible shell — it never returns in a headless session. So the portal is the wrong path for the AI's own eyes/hands: it can't remember input consent and it needs a human to click.
- **`org.gnome.Shell.Screenshot`:** blocked in GNOME 50 ("Screenshot is not allowed").
- **uinput (ydotool):** `/dev/uinput` exists but the module can't load in the WSL kernel (`modprobe uinput: Operation not permitted`). Works on bare metal, not in the WSL edition.
- **Compositor API `org.gnome.Mutter.ScreenCast` + `org.gnome.Mutter.RemoteDesktop`:** present, session-owner, **no dialog**. This is what gnome-remote-desktop uses, and our live RDP session earlier proves it streams and injects input headlessly.

**Answer:** the AI's primary path (AT-SPI program control) needs no permission and works. The **screen fallback** (tier 3) must be built on the **compositor's** screencast/remote-desktop API, not the portal and not uinput. This binds the fallback to the compositor, so it is a further input to the GNOME/KDE choice (Phase 2 builds it).

## Task 3: GNOME desktop on Windows
- Installed `ubuntu-desktop-minimal` + `gnome-remote-desktop` (GNOME 50). After a distro restart: gdm3, gnome-remote-desktop and ollama all active.
- The system remote-desktop daemon **refuses to listen until it has a TLS certificate and key** (`grdctl --system status`: "TLS certificate and key not yet configured properly"). A self-signed one works; it must sit in a folder the `gnome-remote-desktop` user can enter (0755), or the daemon logs "certificate is invalid". Added to the desktop-gnome section. Port 3389 listens after that.
- **WSL kills the distro about 60 s after the last `wsl` command exits** (`vmIdleTimeout`). The journal showed unrequested power-offs at 19:44:50 and 19:49:20; every early RDP attempt hit a machine that was rebooting, and one restart came up with no default route and failing DNS. Fix for the trial: a hidden `wsl -d ai-os -- sleep infinity` process. For the product this *is* the keep-alive spec 4.2 asks for, and it must exist before anything else works, not only for watch jobs.
- Windows `localhost:3389` does not reach the distro's port even though it listens on `*:3389`; the distro's own address (`hostname -I`) does. Use that.
- The Windows credential prompt for RDP cannot be driven by automation (UIPI). Stored the throwaway login with `cmdkey /generic:TERMSRV/<ip>`; **remove with `cmdkey /delete:TERMSRV/<ip>` at the end of the trial.**
- First real connection: gdm launched the headless greeter, and gnome-shell aborted with `Failed to start X Wayland: Directory "/tmp/.X11-unix" is not writable` (WSLg mounts that folder read-only). Mutter itself was fine without a GPU ("Created surfaceless renderer without GPU"). Fix: a boot service that replaces the folder with a writable one, added to the desktop-gnome section.
- **Attempt A result: FAILED at the handover.** With the folder fix the greeter came up (`gnome-shell --mode=gdm`, Xwayland, portal, `gnome-remote-desktop-handover.service` all started) and the system daemon logged "Sending server redirection". The redirected connection then failed inside the greeter's daemon with `credssp_auth_authenticate: SEC_E_NO_CREDENTIALS` / "client authentication failure", and mstsc closed. The system→greeter→user handover is the fragile part; not worth more of the time box. Moving to attempt B.
- **Attempt B, first try:** `gnome-shell --headless --virtual-monitor` started cleanly with no GPU ("Created surfaceless renderer without GPU", virtual monitor added, the AT-SPI registry came up), then died when the `wsl` command that launched it exited: WSL tears down the launching session's processes. Also `grdctl --headless rdp enable` needs the user's real session bus (it talks to systemd), not a private `dbus-run-session` one.
- **Decision (his call, 2026-09-15 19:57): stop trying to display the Linux desktop on Windows.** The AI's own workers never need a visible desktop; the trial continues in an invisible headless GNOME session run under the user's systemd manager (`trial/headless-session.sh`), with apps launched into it and probes reading results from logs. Stored RDP login removed (`cmdkey /delete`), mstsc closed.
- **What this means for the product:** the WSL edition does not need RDP at all. Apps a user wants to *see* run under WSLg, which shows each Linux window as a normal Windows window; the AI drives them through the accessibility layer, which does not care which compositor the window is on. The AI's background and headless work runs in the invisible session. Bare metal has a real screen and none of this applies. U2 is therefore answered "not needed" rather than "passed".
