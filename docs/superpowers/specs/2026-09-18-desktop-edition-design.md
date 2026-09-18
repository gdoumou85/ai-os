# The desktop edition — design (2026-09-18)

Parent: `2026-09-15-ai-os-design.md` §5.2 (two editions) and §7 (build order). This pulls the full
Ubuntu edition forward from Phase 7 so that he can use the AI OS as a person at a desktop while the
later phases are built. His words: "I want to use the Linux OS as a normal user would, then ask the
integrated AI anything I want."

## 1. What it is

A complete Ubuntu 26.04 machine with the GNOME desktop, running as a VirtualBox virtual machine on the
Windows laptop, with the AI OS installed on it exactly as the product will be: the engine as the user's
service, the rail opening with the session, the root wrapper with its single sudoers line, the sandbox
user, `/data` on btrfs. The model stays on the host GPU: the engine talks to the Windows Ollama over
HTTP.

Decisions taken in the brainstorm (2026-09-18):

- **Hypervisor: VirtualBox 7.2** (installed by him, 7.2.18). Windows 11 Home has no Hyper-V; with WSL2
  on, VirtualBox and VMware both run through Windows' hypervisor layer, so neither is faster, and
  VirtualBox is fully scriptable through `VBoxManage`.
- **Model runner: the Windows Ollama serves the VM** (option 1). It already owns the GPU and has
  `qwen3.5:9b`. The WSL distro keeps its own Ollama for the tests; they contend only if both load at once.
- **Build: Ubuntu's cloud image plus a first-boot seed** (approach 1). No installer, no clicking; one
  command rebuilds the machine from scratch. The seed format (cloud-init user-data) is what the USB
  installer's autoinstall wraps later, so nothing is throwaway.
- **Login user stays `ai`** (uid 1000), the account every script assumes; the product installer asks
  for a real name in Phase 7. The sandbox user stays `ai-sandbox`.
- **Two disks:** the image's root disk untouched; a second virtual disk for `/data`, btrfs, which is
  what the AI is allowed to change (parent §4.8).

## 2. What runs where

| Windows host | The VM |
|---|---|
| VirtualBox 7.2 | Ubuntu 26.04 (cloud image, generic kernel) + `ubuntu-desktop-minimal`, GDM |
| Ollama for Windows, `qwen3.5:9b`, the GPU | user `ai`, the engine as `ai`'s user unit, the rail as an XDG autostart entry |
| the repo, shared into the VM as `/mnt/repo` | `/usr/local/bin/ai-os-engine`, `ai-os-chat`, `ai-os-rail`; `/usr/local/libexec/ai-os-admin` |
| | `/data` on the second disk (btrfs, `/data/live`, `/data/snapshots`, `/data/projects`, `/data/jobs`, `/data/housekeeping`) |

The VM never gets a compiler: the binaries are the WSL release build, copied from the share. The WSL
distro stays the build-and-test workshop; the rule "tests run inside the distro" is unchanged.

What disappears in the VM: the invisible session and `ai-os-desktop.service`, the two-display split
(both display settings name `wayland-0`, as 2a §3 says for bare metal), WSLg, `rail.cmd`. What stays
identical: binaries, database, units, wrapper, sudoers line.

## 3. The build, end to end

**Host side, `trial/vm/make-vm.cmd` → `make-vm.ps1`** (workshop only, parent §11):

1. Downloads `resolute-server-cloudimg-amd64.ova` from `cloud-images.ubuntu.com` into
   `C:\VM\ai-os\` if not present (Ubuntu 26.04 "Resolute"; the `.ova` variant carries the generic
   kernel VirtualBox needs).
2. Builds the seed ISO `seed.iso` (volume label `cidata`, files `user-data` and `meta-data`) with
   Windows' built-in IMAPI2 COM objects — no extra tool. The user-data lives in the repo as
   `trial/vm/user-data`; the host script only packs it.
3. If a VM named `ai-os` exists: powers it off and unregisters it with its disks (that is the rebuild).
4. `VBoxManage import` of the OVA as `ai-os`, then `modifyvm`: 6 CPUs, 8192 MB, VMSVGA with 128 MB
   VRAM and 3D off, NAT network, clipboard bidirectional; the imported root disk is a VMDK, which
   cannot be resized, so `clonemedium --format VDI` to `root.vdi`, `modifymedium --resize 65536` on
   that, and it replaces the VMDK on the controller; a new 64 GB dynamic VDI `data.vdi` attached as the second disk; `seed.iso` on the DVD;
   `sharedfolder add repo` pointing at the repo, automount, mount point `/mnt/repo`.
5. `startvm`. The rest happens inside.

**Guest side, first boot, `trial/vm/user-data`** (cloud-init NoCloud):

- `users`: `ai`, uid 1000, shell bash, no sudo group — the wrapper's grant is the only privilege, as in
  the workshop. The password: `make-vm.ps1` asks for it once at build time and writes it into the seed's
  `chpasswd` (the repo's `user-data` carries the token `@PASSWORD@`; the seed ISO is never committed).
  `ssh_pwauth` off; no SSH.
- `growpart`/`resizefs`: default, the root file system fills the resized disk.
- `packages`: `virtualbox-guest-utils` (for the share), `btrfs-progs`.
- `runcmd`: mount the share (`mount -t vboxsf -o uid=1000,gid=1000 repo /mnt/repo`, and an fstab
  line for later boots), then `bash /mnt/repo/trial/vm/setup-vm.sh`, then reboot.

**Guest side, `trial/vm/setup-vm.sh`** (root, idempotent, workshop only, `AI_OS_REPO=/mnt/repo`):

1. `/data`: `mkfs.btrfs` on the second disk if it has no file system, fstab entry (`noatime,
   compress=zstd`), mount, `/data/live` subvolume, `/data/snapshots` — the `snapshot` section of
   `setup-trial.sh` minus the loop file.
2. Desktop: `ubuntu-desktop-minimal gdm3 xdg-desktop-portal-gnome pipewire wireplumber
   gnome-text-editor gnome-calculator libgtk-4-1`, `systemctl set-default graphical.target`, GDM
   autologin **off** (he logs in), `toolkit-accessibility` on for `ai`.
3. Sandbox user: `trial/setup-sandbox-user.sh` as it is (it creates `/data/jobs`, `/data/projects`).
4. Admin wrapper and the one sudoers line: `trial/setup-admin.sh` as it is (needs `cargo npm
   python3-venv` for the fetch verbs; unchanged).
5. Binaries: `install` of `ai-os-engine`, `ai-os-chat`, `ai-os-rail` from
   `$AI_OS_REPO/runtime/target/release` (fails loudly if not built — the WSL release build is a
   precondition, checked by `make-vm.ps1` before it starts).
6. The engine unit: `trial/vm/ai-os-engine.service` → `/home/ai/.config/systemd/user/`, enabled,
   `loginctl enable-linger ai` (a job survives logout, as on the workshop). It differs from the workshop
   unit in three lines: no `After=/Wants=ai-os-desktop.service`; `AI_OS_DISPLAY_INVISIBLE=wayland-0`
   and `AI_OS_DISPLAY_VISIBLE=wayland-0`; `AI_OS_MODEL_URL=http://10.0.2.2:11434`.
7. The rail with the session: `runtime/rail/org.aios.Rail.desktop` (product file: `Exec=ai-os-rail`,
   `Type=Application`, `X-GNOME-Autostart-enabled=true`) installed to `/etc/xdg/autostart/` and to
   `/usr/share/applications/` so it is also in the app grid.
8. Text-file defaults for `ai` (the `xdg-mime` lines from `setup-rail.sh`).
9. A VirtualBox snapshot is not taken from inside; `make-vm.ps1` takes one named `fresh` when the VM
   reaches the login screen the first time (it polls `VBoxManage guestproperty` for the marker
   `setup-vm.sh` writes on success: `/VirtualBox/GuestAdd/ai-os` = `ready`).

## 4. The one product change

`OllamaModel::local` hard-codes `http://127.0.0.1:11434`. The engine binary reads `AI_OS_MODEL_URL`
(default the same address) and passes it in; `OllamaModel { url, model }` already takes it. Unit test:
the env value is what the model gets. Nothing else in the product knows where it runs.

The rail's autostart entry is a product file too (§3 step 7): the rail opens with the session on the
full edition; on WSL there is no session to open with, so the file is simply not installed there.

## 5. Network and security

VirtualBox NAT places the host's loopback at `10.0.2.2` from inside the guest, so the Windows Ollama
listening on `127.0.0.1:11434` should be reachable without any change on Windows. The first task
proves this with `curl http://10.0.2.2:11434/api/tags` from the guest. If it is not reachable,
the fallback is `OLLAMA_HOST=0.0.0.0` as a Windows user setting plus one inbound firewall rule for
port 11434 limited to the VirtualBox NAT source — his steps, on his machine, and the design records
which of the two it was.

The VM has no port forwarding in, no SSH, and the share is read-write only because the WSL workshop's
is too; the AI's sandbox cannot see `/mnt` (the jail replaces it, 1b), so the share is his and the
setup script's, not the model's.

## 6. Testing and acceptance

- **Task 1 is a spike inside the plan:** import, boot, GNOME login by hand, and three measurements
  written into this spec's Results: time from `startvm` to the login screen, whether GNOME is usable
  under the Windows hypervisor layer (his judgement, typing and window drag), and whether
  `10.0.2.2:11434` answers. If the desktop is not usable, stop and rethink before building the rest.
- **Unit tests** in WSL as always: the `AI_OS_MODEL_URL` read; the desktop entry file parses
  (`desktop-file-validate` is not in the toolchain, so a test reads the file and checks `Exec` and
  `Type`).
- **`trial/vm/check.sh`**, run inside the VM from a terminal or from the host through
  `VBoxManage guestcontrol run`: engine unit active, socket present, `curl` to the model answers, the
  a11y bus is on the session bus, `/data` is btrfs, the sudoers file is the one line. Prints
  `desktop edition ready` or the first failure.
- **Acceptance is his, at the keyboard:** log in, the rail is open, open Text Editor from the app grid,
  tell the rail to take that window and add a line, watch it happen, see the Done card. This is 2a's
  item 5, done where it was always meant to be done. Then any question he likes.

## 7. Out of scope

The USB image and the real installer (Phase 7), GPU passthrough, the WSL keep-alive (parent §4.2, still
needed for the WSL edition), the screen fallback (2b), and any change to how the engine works. The
user-data password handling is workshop-only; the product installer sets it.

## 8. Risks

- **Speed under the Windows hypervisor layer.** VirtualBox cannot use its own engine while WSL2 is on.
  Measured in Task 1; the rethink if it fails is VMware Workstation (same layer, so unlikely to differ)
  or a bare-metal install on a spare disk.
- **Ubuntu 26.04 cloud OVA availability.** The URL pattern is stable across releases; if 26.04's OVA is
  missing, the `.img` plus `VBoxManage convertfromraw` after `qemu-img` is the fallback, which adds a
  tool. Checked in Task 1.
- **Guest additions from Ubuntu's package** (`virtualbox-guest-utils`) rather than VirtualBox's ISO:
  they lag VirtualBox by a version but the share and the guest properties are old, stable interfaces.

## 9. Change of approach (2026-09-18, his call): he installs Ubuntu, one command adds the AI OS

Three automated builds in (the cloud image's first boot cannot mount the VirtualBox share; the setup
moved to a one-shot unit on the next boot and then worked; the guest's vCPUs were starved for 200 s at
6 vCPUs under the Windows hypervisor layer) he stopped it: **"I install the Ubuntu myself, as any other
user would. And then we use an install command to add the AI layer as any future user would."** And:
**"I will create my own user name for my Ubuntu."** §3 (the cloud image, the seed, `make-vm`) and
`trial/vm/` are withdrawn; §1's decisions on the hypervisor, the model runner and the two-disk split are
his machine's business now, not the product's. What replaces them:

**9.1 The owner is whoever installs it.** Nothing in the product names the user `ai` any more.
- The root wrapper takes its owner from `SUDO_USER`, which sudo sets itself and the caller cannot forge;
  the sudoers line grants exactly one user, so the owner is the only one who can arrive there. The
  owner's home comes from `getent passwd`. Every `/home/ai` becomes that home, every `ai:` that owner.
  No `SUDO_USER`, `root`, or a home that is empty, `/` or not a directory: refused before any verb.
- The job prompt's one mention of `/home/ai` becomes the engine's own `$HOME`.
- The sandbox account stays `ai-sandbox` (a system account the installer makes).
- The installer does **not** take the owner out of the `sudo` group: the workshop did that because its
  `ai` was the AI's account; here it is a person. The engine still reaches root only through
  `sudo -n <wrapper>`, the single NOPASSWD grant; everything else asks for a password no engine has.

**9.2 The install command** — `install/install.sh`, product, run by the owner (not as root; it calls
`sudo` for the root steps): runtime packages; `/data` as a sparse btrfs loop file (half the free space,
at most 50 GB — a hand-installed Ubuntu has one disk); `ai-sandbox` and the `/data` folders owned
`<owner>:ai-sandbox`; the wrapper and the one sudoers line, validated by `visudo`; the three binaries;
the engine as the owner's user unit with linger; the rail's desktop entry in the app grid and in XDG
autostart; `toolkit-accessibility` on. **The model:** `--model-url URL` points the engine at a runner
elsewhere (his VM: `http://10.0.2.2:11434`, the Windows Ollama); without it the installer installs
Ollama on the machine and pulls `qwen3.5:9b` — written, and untested until a machine with a GPU runs
it. It ends by running `install/check.sh`, which prints `AI OS ready` or the first failure.
Uninstall is not in this round.

**9.3 Distribution** — one tarball, `ai-os-linux-amd64.tar.gz`: `install.sh`, `check.sh`, the three
binaries, the wrapper, the desktop entry, `VERSION`. `install/make-release.sh` builds it in the WSL
workshop (binaries are built on Ubuntu 26.04 and promise nothing on older releases). It is attached to
a GitHub release of the project's repo; `install/get.sh` is the one line a user runs: download the
latest tarball, unpack, run `install.sh` with the same arguments. Creating the repo and publishing wait
for his word on public or private.

**9.4 Proof before his VM** — a fresh Ubuntu 26.04 WSL distro imported beside the workshop, a user with
a name that is not `ai`, the tarball installed with `--model-url`, `check.sh` green minus the desktop
session, the wrapper's own test suite green as that user; then the distro is unregistered. His Ubuntu
is the acceptance (§6's last item, unchanged).
