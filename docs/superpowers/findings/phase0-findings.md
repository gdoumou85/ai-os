# Phase 0 findings

Started 2026-09-15. Throwaway distro `ai-os` on ALIEN. Spec v2 = `effba2c`.

| # | Unknown | Result | Evidence |
|---|---|---|---|
| U1 | GPU inside WSL | PASS | `nvidia-smi` in the distro: RTX 5060 Laptop, 8151 MiB, driver 610.60; Ollama runs the model on it |
| U2 | Full desktop on Windows | | |
| U3 | Accessibility on real apps | | |
| U4 | Focus stealing | | |
| U5 | Remembered screen permission | | |
| U6 | Snapshot disk with rollback | PASS (restart check pending) | btrfs in the WSL kernel; loop image at `/var/lib/ai-os/data.img` mounted on `/data`; snapshot 5 ms, rollback 7 ms, change gone; writes 1.5 GB/s vs 2.1 GB/s on plain root |
| U7 | Local 8B: speed / parse rate / context | PASS with a caveat | qwen3.5:9b: 35 tok/s, 1.25 s per tool call, 100% parse with and without grammar; **8k context is the most that stays fully on the GPU** (with q8_0 KV cache); 12k+ spills to CPU |
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
