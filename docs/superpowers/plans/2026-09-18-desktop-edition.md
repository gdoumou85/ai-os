# The Desktop Edition Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One command on Windows builds a VirtualBox VM that is the AI OS as a person uses it: Ubuntu 26.04 with GNOME, user `ai` at the login screen, the rail opening with the session, the engine talking to the Windows Ollama over HTTP.

**Architecture:** A PowerShell script imports Ubuntu's cloud OVA, gives it two disks and a cloud-init seed ISO, shares the repo in, and starts it. On first boot cloud-init creates the user and runs `trial/vm/setup-vm.sh`, which installs the desktop and the AI OS from the WSL release build, then reboots into GDM. The product changes in two files: the engine reads the model address from `AI_OS_MODEL_URL`, and the rail gains an XDG autostart entry.

**Tech Stack:** VirtualBox 7.2 (`VBoxManage`), Windows PowerShell 5.1 (IMAPI2 for the seed ISO), cloud-init NoCloud, Ubuntu 26.04 cloud image, bash, Rust (two small edits).

**Spec:** `docs/superpowers/specs/2026-09-18-desktop-edition-design.md`

## Global Constraints

- Nothing per-app in the product; nothing WSL- or VirtualBox-specific outside `trial/` (parent spec §11). Everything under `trial/vm/` carries the comment `WORKSHOP ONLY`.
- All text files LF, except `*.cmd` which `.gitattributes` keeps CRLF (cmd.exe misparses LF batch files).
- Tests run inside the WSL distro: from the PowerShell tool, `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p <crate>`. Anything with `$` in a `wsl` command line goes in a script file (the login shell expands it first).
- Windows PowerShell 5.1 for `.ps1`: no `&&`, no `?:`, no `??`; `$ErrorActionPreference = 'Stop'`; native commands checked with `if ($LASTEXITCODE -ne 0) { throw ... }`.
- The login user is `ai` (uid 1000); the sandbox user is `ai-sandbox`; the only privilege is the sudoers line `ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin`.
- `/data` is btrfs on the second disk; `/data/live` subvolume, `/data/snapshots`, `/data/projects`, `/data/jobs`, `/data/housekeeping` as the existing scripts make them.
- The engine unit in the VM: no `ai-os-desktop.service` ordering; `AI_OS_DISPLAY_INVISIBLE=wayland-0`, `AI_OS_DISPLAY_VISIBLE=wayland-0`, `AI_OS_MODEL_URL=http://10.0.2.2:11434`.
- VM sizes: 6 CPUs, 8192 MB, VMSVGA 128 MB VRAM, 3D off, NAT, root disk 65536 MB (VDI), data disk 65536 MB dynamic VDI, folder `C:\VM\ai-os\`, VM name `ai-os`, share name `repo` mounted at `/mnt/repo`.
- Never push, tag or merge without his word. Commit each task.

---

## File map

| File | Responsibility |
|---|---|
| `runtime/core/src/model.rs` | `OllamaModel::from_env(model)`: the address from `AI_OS_MODEL_URL`, default `http://127.0.0.1:11434` |
| `runtime/core/src/bin/ai-os-engine.rs` | uses `from_env` |
| `runtime/rail/org.aios.Rail.desktop` | product: the rail's desktop entry, doubles as the autostart entry |
| `runtime/rail/tests/desktop_entry.rs` | proves the entry's `Exec`, `Type`, autostart flag |
| `trial/vm/user-data`, `trial/vm/meta-data` | cloud-init seed: user `ai`, guest utils, mount the share, run setup, reboot |
| `trial/vm/setup-vm.sh` | guest, root: `/data`, desktop, sandbox user, wrapper, binaries, unit, autostart, marker |
| `trial/vm/ai-os-engine.service` | the engine's user unit for one real display and the host model |
| `trial/vm/check.sh` | guest: prints `desktop edition ready` or the first failure |
| `trial/vm/make-vm.ps1`, `trial/vm/make-vm.cmd` | host: download, seed ISO, (re)build the VM, start, wait for the marker, snapshot `fresh` |
| `docs/superpowers/specs/2026-09-18-desktop-edition-design.md` | Results block after the first build |
| `docs/superpowers/specs/2026-09-15-ai-os-design.md` | status block |

---

### Task 1: The engine reads the model address from the environment

**Files:**
- Modify: `runtime/core/src/model.rs:45-47` (the `impl OllamaModel` block)
- Modify: `runtime/core/src/bin/ai-os-engine.rs:23`
- Test: `runtime/core/src/model.rs` tests module (append)

**Interfaces:**
- Produces: `OllamaModel::from_env(model: &str) -> OllamaModel` — url from `AI_OS_MODEL_URL` or `http://127.0.0.1:11434`. `local` stays for the tests that use it.

- [ ] **Step 1: Write the failing test.** In `runtime/core/src/model.rs`, inside the existing `#[cfg(test)] mod tests`, add:

```rust
    /// The desktop edition's engine reaches a runner on another machine (desktop design §4): one
    /// environment setting, read in one place. Only this test touches the variable.
    #[test]
    fn the_model_address_comes_from_the_environment() {
        std::env::remove_var("AI_OS_MODEL_URL");
        assert_eq!(OllamaModel::from_env("m").url, "http://127.0.0.1:11434");
        std::env::set_var("AI_OS_MODEL_URL", "http://10.0.2.2:11434");
        let m = OllamaModel::from_env("qwen3.5:9b");
        std::env::remove_var("AI_OS_MODEL_URL");
        assert_eq!((m.url.as_str(), m.model.as_str()), ("http://10.0.2.2:11434", "qwen3.5:9b"));
    }
```

- [ ] **Step 2: Run it, expect a compile failure** (`from_env` does not exist):

`wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p aios-core the_model_address`

- [ ] **Step 3: Implement.** Replace the `impl OllamaModel` block with:

```rust
impl OllamaModel {
    pub fn local(model: &str) -> Self { Self { url: "http://127.0.0.1:11434".into(), model: model.into() } }
    /// `AI_OS_MODEL_URL` when set (the desktop edition points it at the host's runner), else local.
    pub fn from_env(model: &str) -> Self {
        let mut m = Self::local(model);
        if let Ok(url) = std::env::var("AI_OS_MODEL_URL") { m.url = url; }
        m
    }
}
```

In `runtime/core/src/bin/ai-os-engine.rs` change `let llm = OllamaModel::local(&model);` to `let llm = OllamaModel::from_env(&model);`.

- [ ] **Step 4: Run the core suite, expect green:**

`wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p aios-core`

- [ ] **Step 5: Commit.**

```bash
git add runtime/core/src/model.rs runtime/core/src/bin/ai-os-engine.rs
git commit -m "feat(engine): the model address comes from AI_OS_MODEL_URL"
```

---

### Task 2: The rail's desktop entry

**Files:**
- Create: `runtime/rail/org.aios.Rail.desktop`
- Test: `runtime/rail/tests/desktop_entry.rs`

**Interfaces:**
- Produces: the file `runtime/rail/org.aios.Rail.desktop`, installed by Task 3 to `/etc/xdg/autostart/` and `/usr/share/applications/`.

- [ ] **Step 1: Write the failing test** at `runtime/rail/tests/desktop_entry.rs`:

```rust
//! The rail opens with the session on the full edition (desktop design §3 step 7, §4): one desktop
//! entry serves the app grid and XDG autostart. `desktop-file-validate` is not in the toolchain, so
//! this reads the file and checks the keys the two uses need.
const ENTRY: &str = include_str!("../org.aios.Rail.desktop");

fn value(key: &str) -> Option<&'static str> {
    ENTRY.lines().find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
}

#[test]
fn the_entry_launches_the_rail_and_autostarts() {
    assert_eq!(ENTRY.lines().next(), Some("[Desktop Entry]"));
    assert_eq!(value("Type"), Some("Application"));
    assert_eq!(value("Exec"), Some("ai-os-rail"));
    assert_eq!(value("Name"), Some("AI OS"));
    assert_eq!(value("X-GNOME-Autostart-enabled"), Some("true"));
    assert!(!ENTRY.contains('\r'), "LF only");
}
```

- [ ] **Step 2: Run it, expect failure** (file missing → `include_str!` error):

`wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p aios-rail --test desktop_entry`

- [ ] **Step 3: Create `runtime/rail/org.aios.Rail.desktop`** (LF):

```ini
[Desktop Entry]
Type=Application
Name=AI OS
Comment=Tell the AI what you want
Exec=ai-os-rail
Icon=utilities-terminal
Terminal=false
Categories=Utility;
X-GNOME-Autostart-enabled=true
```

- [ ] **Step 4: Run the rail suite, expect green:**

`wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh test -p aios-rail`

- [ ] **Step 5: Commit.**

```bash
git add runtime/rail/org.aios.Rail.desktop runtime/rail/tests/desktop_entry.rs
git commit -m "feat(rail): a desktop entry, also the session autostart on the full edition"
```

---

### Task 3: The guest side — seed, setup script, unit, check

**Files:**
- Create: `trial/vm/user-data`, `trial/vm/meta-data`, `trial/vm/setup-vm.sh`, `trial/vm/ai-os-engine.service`, `trial/vm/check.sh`

**Interfaces:**
- Consumes: `trial/setup-sandbox-user.sh`, `trial/setup-admin.sh` (both honour `AI_OS_REPO`), `runtime/rail/org.aios.Rail.desktop` (Task 2), the WSL release binaries at `$AI_OS_REPO/runtime/target/release/{ai-os-engine,ai-os-chat,ai-os-rail}`.
- Produces: the guest property `/ai-os/setup` = `ready` on success (Task 4 waits for it); `/var/log/ai-os-setup.log`; the token `@PASSWORD@` in `user-data` that Task 4 replaces.

- [ ] **Step 1: `trial/vm/meta-data`:**

```yaml
instance-id: ai-os-desktop-1
local-hostname: ai-os
```

- [ ] **Step 2: `trial/vm/user-data`:**

```yaml
#cloud-config
# WORKSHOP ONLY: the desktop edition's first boot (desktop design §3). make-vm.ps1 replaces
# @PASSWORD@ and packs this into seed.iso; the seed is never committed.
hostname: ai-os
users:
  - name: ai
    uid: "1000"
    shell: /bin/bash
    lock_passwd: false
chpasswd:
  expire: false
  users:
    - name: ai
      password: "@PASSWORD@"
      type: text
ssh_pwauth: false
package_update: true
packages:
  - virtualbox-guest-utils
  - btrfs-progs
runcmd:
  - modprobe vboxguest
  - modprobe vboxsf
  - mkdir -p /mnt/repo
  - echo 'repo /mnt/repo vboxsf uid=1000,gid=1000,nofail 0 0' >> /etc/fstab
  - mount /mnt/repo
  - [bash, -c, "set -o pipefail; bash /mnt/repo/trial/vm/setup-vm.sh 2>&1 | tee /var/log/ai-os-setup.log && reboot"]
```

- [ ] **Step 3: `trial/vm/ai-os-engine.service`:**

```ini
[Unit]
Description=AI OS engine (desktop edition)

[Service]
ExecStart=/usr/local/bin/ai-os-engine
Environment=AI_OS_DB=/data/ai-os.db
Environment=AI_OS_MODEL=qwen3.5:9b
Environment=AI_OS_MODEL_URL=http://10.0.2.2:11434
Environment=AI_OS_PROJECTS=/data/projects
Environment=AI_OS_DISPLAY_INVISIBLE=wayland-0
Environment=AI_OS_DISPLAY_VISIBLE=wayland-0
Restart=on-failure
RestartSec=2

[Install]
WantedBy=default.target
```

- [ ] **Step 4: `trial/vm/setup-vm.sh`:**

```bash
#!/usr/bin/env bash
# The desktop edition's guest setup (desktop design §3). Root, idempotent, run by cloud-init on the
# first boot and by hand after a rebuild of the binaries:  sudo is not available to ai, so from the
# VM's console as root, or again through cloud-init by rebuilding the VM.
# WORKSHOP ONLY (parent spec §11): binaries and scripts come from the shared repo; the product ships
# them from a package.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export AI_OS_REPO=${AI_OS_REPO:-/mnt/repo}
rel="$AI_OS_REPO/runtime/target/release"
for b in ai-os-engine ai-os-chat ai-os-rail; do
  [ -f "$rel/$b" ] || { echo "$rel/$b is missing: build the release in WSL first" >&2; exit 1; }
done

echo "== /data on the second disk"
disk=/dev/sdb
[ -b $disk ] || { echo "no second disk at $disk" >&2; exit 1; }
blkid $disk >/dev/null 2>&1 || mkfs.btrfs -q -L ai-os-data $disk
install -d /data
grep -q ' /data ' /etc/fstab || echo 'LABEL=ai-os-data /data btrfs noatime,compress=zstd 0 0' >>/etc/fstab
mountpoint -q /data || mount /data
[ -d /data/live ] || btrfs subvolume create /data/live
install -d /data/snapshots
chown ai:ai /data/live

echo "== the desktop"
# linux-generic: the cloud image's kernel flavour may lack modules-extra (the VMSVGA display driver);
# the reboot that ends the first boot picks the new kernel up.
apt-get update
apt-get install -y ubuntu-desktop-minimal gdm3 xdg-desktop-portal-gnome pipewire wireplumber \
  gnome-text-editor gnome-calculator libgtk-4-1 python3-gi linux-generic
systemctl set-default graphical.target
runuser -u ai -- dbus-run-session gsettings set org.gnome.desktop.interface toolkit-accessibility true || true

echo "== sandbox user and the wrapper"
bash "$AI_OS_REPO/trial/setup-sandbox-user.sh"
bash "$AI_OS_REPO/trial/setup-admin.sh"

echo "== binaries"
for b in ai-os-engine ai-os-chat ai-os-rail; do install -m 0755 -o root -g root "$rel/$b" /usr/local/bin/$b; done

echo "== the engine as ai's service"
install -d -o ai -g ai -m 0755 /home/ai/.config/systemd/user
install -m 0644 -o ai -g ai "$AI_OS_REPO/trial/vm/ai-os-engine.service" /home/ai/.config/systemd/user/ai-os-engine.service
sed -i 's/\r$//' /home/ai/.config/systemd/user/ai-os-engine.service
loginctl enable-linger ai
# ai's manager may not be up yet on the first boot; enabling by symlink needs no manager.
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 systemctl --user enable ai-os-engine.service 2>/dev/null \
  || { install -d -o ai -g ai /home/ai/.config/systemd/user/default.target.wants
       ln -sf ../ai-os-engine.service /home/ai/.config/systemd/user/default.target.wants/ai-os-engine.service
       chown -h ai:ai /home/ai/.config/systemd/user/default.target.wants/ai-os-engine.service; }

echo "== the rail with the session"
install -m 0644 -o root -g root "$AI_OS_REPO/runtime/rail/org.aios.Rail.desktop" /usr/share/applications/org.aios.Rail.desktop
install -d /etc/xdg/autostart
install -m 0644 -o root -g root "$AI_OS_REPO/runtime/rail/org.aios.Rail.desktop" /etc/xdg/autostart/org.aios.Rail.desktop
sed -i 's/\r$//' /usr/share/applications/org.aios.Rail.desktop /etc/xdg/autostart/org.aios.Rail.desktop

echo "== text files open in the editor"
runuser -u ai -- env XDG_RUNTIME_DIR=/run/user/1000 xdg-mime default org.gnome.TextEditor.desktop \
  text/x-python text/markdown text/plain text/x-shellscript application/json || true

VBoxControl guestproperty set /ai-os/setup ready
echo "desktop edition set up"
```

- [ ] **Step 5: `trial/vm/check.sh`** (run inside the VM as `ai`, or from the host with `VBoxManage guestcontrol ai-os run --username ai --password <pw> -- /bin/bash /mnt/repo/trial/vm/check.sh`):

```bash
#!/usr/bin/env bash
# The desktop edition's smoke check (desktop design §6): the first failing line is the answer.
# WORKSHOP ONLY. Run as ai inside the VM.
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/1000} DBUS_SESSION_BUS_ADDRESS=${DBUS_SESSION_BUS_ADDRESS:-unix:path=/run/user/1000/bus}
fail() { echo "FAIL: $1"; exit 1; }
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || fail "/data is not btrfs"
[ "$(cat /etc/sudoers.d/ai-os-admin 2>/dev/null)" = 'ai ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin' ] || fail "sudoers is not the one line"
id ai-sandbox >/dev/null 2>&1 || fail "no ai-sandbox user"
systemctl --user is-active ai-os-engine.service >/dev/null || fail "engine unit not active"
[ -S "$XDG_RUNTIME_DIR/ai-os.sock" ] || fail "no engine socket"
curl -fsS --max-time 5 http://10.0.2.2:11434/api/tags | grep -q qwen3.5 || fail "the host model does not answer at 10.0.2.2:11434"
busctl --user call org.a11y.Bus /org/a11y/bus org.a11y.Bus GetAddress >/dev/null 2>&1 || fail "no accessibility bus on the session bus"
[ -f /etc/xdg/autostart/org.aios.Rail.desktop ] || fail "no rail autostart entry"
echo "desktop edition ready"
```

- [ ] **Step 6: Syntax-check both scripts in WSL**, one call each:

`wsl -d ai-os -u ai -- bash -n /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/vm/setup-vm.sh` and the same for `check.sh`. Expected: no output. Also `python3 -c "import yaml,sys; yaml.safe_load(open(sys.argv[1]))" trial/vm/user-data` from Windows (PyYAML is installed with Python here; if not, `pip install pyyaml` in the scratchpad venv is fine, or check the file by eye: two-space indents, the `runcmd` list item is a flow list).

- [ ] **Step 7: LF check and commit.**

```bash
grep -c $'\r' trial/vm/user-data trial/vm/meta-data trial/vm/setup-vm.sh trial/vm/ai-os-engine.service trial/vm/check.sh   # all 0
git add trial/vm/user-data trial/vm/meta-data trial/vm/setup-vm.sh trial/vm/ai-os-engine.service trial/vm/check.sh
git commit -m "workshop(vm): the guest side of the desktop edition — seed, setup, unit, check"
```

---

### Task 4: The host side, the first build, the measurements

**Files:**
- Create: `trial/vm/make-vm.ps1`, `trial/vm/make-vm.cmd`
- Modify: `docs/superpowers/specs/2026-09-18-desktop-edition-design.md` (append a Results block to §6), `docs/superpowers/specs/2026-09-15-ai-os-design.md` (status block at the end)

**Interfaces:**
- Consumes: Task 3's files; the guest property `/ai-os/setup`; the WSL release build (`trial/run-tests.sh build --release` builds it: `cargo build --release` in the distro).

- [ ] **Step 1: `trial/vm/make-vm.ps1`:**

```powershell
# WORKSHOP ONLY: builds the desktop edition as a VirtualBox VM (desktop design §3). Rerunnable:
# an existing "ai-os" VM is thrown away and rebuilt from the cloud image. Windows PowerShell 5.1.
param(
  [string]$Repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
  [string]$Dir = 'C:\VM\ai-os',
  [string]$Password = 'ai',
  [int]$WaitMinutes = 45
)
$ErrorActionPreference = 'Stop'
$vbm = 'C:\Program Files\Oracle\VirtualBox\VBoxManage.exe'
if (-not (Test-Path $vbm)) { throw "VirtualBox is not installed ($vbm)" }
function VBM { & $vbm @args; if ($LASTEXITCODE -ne 0) { throw "VBoxManage $($args -join ' ') failed ($LASTEXITCODE)" } }

foreach ($b in 'ai-os-engine','ai-os-chat','ai-os-rail') {
  if (-not (Test-Path "$Repo\runtime\target\release\$b")) { throw "$b is not built: run trial/run-tests.sh build --release in WSL first" }
}
New-Item -ItemType Directory -Force $Dir | Out-Null

# 1. The cloud image (26.04 "resolute"; the OVA carries the generic kernel VirtualBox needs).
$ova = "$Dir\resolute-server-cloudimg-amd64.ova"
if (-not (Test-Path $ova)) {
  Write-Host "downloading the Ubuntu 26.04 cloud image (about 800 MB)"
  Start-BitsTransfer -Source 'https://cloud-images.ubuntu.com/resolute/current/resolute-server-cloudimg-amd64.ova' -Destination $ova
}

# 2. The seed ISO: user-data with the password filled in, packed with Windows' own IMAPI2.
$seed = "$Dir\seed"; New-Item -ItemType Directory -Force $seed | Out-Null
$ud = (Get-Content "$Repo\trial\vm\user-data" -Raw) -replace '\r', ''
[IO.File]::WriteAllText("$seed\user-data", $ud.Replace('@PASSWORD@', $Password))
[IO.File]::WriteAllText("$seed\meta-data", ((Get-Content "$Repo\trial\vm\meta-data" -Raw) -replace '\r', ''))
Add-Type -TypeDefinition @'
public class IsoWriter {
  public unsafe static void Write(string path, object stream, int blockSize, int totalBlocks) {
    int bytes = 0; byte[] buf = new byte[blockSize]; var ptr = (System.IntPtr)(&bytes);
    var o = System.IO.File.OpenWrite(path);
    var i = stream as System.Runtime.InteropServices.ComTypes.IStream;
    while (totalBlocks-- > 0) { i.Read(buf, blockSize, ptr); o.Write(buf, 0, bytes); }
    o.Flush(); o.Close();
  }
}
'@ -CompilerParameters (New-Object CodeDom.Compiler.CompilerParameters -Property @{ CompilerOptions = '/unsafe' })
$fsi = New-Object -ComObject IMAPI2FS.MsftFileSystemImage
$fsi.FileSystemsToCreate = 3      # ISO9660 + Joliet
$fsi.VolumeName = 'cidata'        # cloud-init's NoCloud label
$fsi.Root.AddTree($seed, $false)
$img = $fsi.CreateResultImage()
$iso = "$Dir\seed.iso"; if (Test-Path $iso) { Remove-Item $iso -Force }
[IsoWriter]::Write($iso, $img.ImageStream, $img.BlockSize, $img.TotalBlocks)

# 3. Throw away the old machine.
if ((& $vbm list vms) -match '^"ai-os" ') {
  & $vbm controlvm ai-os poweroff 2>$null; Start-Sleep 3
  VBM unregistervm ai-os --delete-all
}
if (Test-Path "$Dir\ai-os") { Remove-Item "$Dir\ai-os" -Recurse -Force }

# 4. Import and shape it.
VBM import $ova --vsys 0 --vmname ai-os --basefolder $Dir --cpus 6 --memory 8192
VBM modifyvm ai-os --graphicscontroller vmsvga --vram 128 --accelerate3d off --clipboard-mode bidirectional --nic1 nat
$info = & $vbm showvminfo ai-os --machinereadable
$ctl = ($info | Select-String '^storagecontrollername0="(.+)"').Matches[0].Groups[1].Value
$disk = ($info | Select-String '^"(.+)-(\d+)-(\d+)"="(.+\.vmdk)"').Matches[0]
VBM storageattach ai-os --storagectl $ctl --port $disk.Groups[2].Value --device $disk.Groups[3].Value --medium none
VBM storagectl ai-os --name $ctl --remove
$root = "$Dir\ai-os\root.vdi"
VBM clonemedium disk $disk.Groups[4].Value $root --format VDI
VBM closemedium disk $disk.Groups[4].Value --delete
VBM modifymedium disk $root --resize 65536
$data = "$Dir\ai-os\data.vdi"
VBM createmedium disk --filename $data --size 65536 --format VDI
VBM storagectl ai-os --name SATA --add sata --controller IntelAhci --portcount 3 --bootable on
VBM storageattach ai-os --storagectl SATA --port 0 --device 0 --type hdd --medium $root
VBM storageattach ai-os --storagectl SATA --port 1 --device 0 --type hdd --medium $data
VBM storageattach ai-os --storagectl SATA --port 2 --device 0 --type dvddrive --medium $iso
VBM sharedfolder add ai-os --name repo --hostpath $Repo

# 5. Boot, wait for the guest to say it is set up, then keep a checkpoint of "freshly installed".
$t0 = Get-Date
VBM startvm ai-os
Write-Host "first boot: the desktop and the AI OS are installing inside (typically 20-40 minutes)"
$deadline = $t0.AddMinutes($WaitMinutes)
do {
  Start-Sleep 20
  $ready = (& $vbm guestproperty get ai-os /ai-os/setup 2>$null) -match 'Value: ready'
} until ($ready -or (Get-Date) -gt $deadline)
if (-not $ready) { throw "the guest did not report ready within $WaitMinutes minutes; look at the VM window and /var/log/ai-os-setup.log" }
Write-Host ("set up after {0:n0} s; waiting for the reboot to the login screen" -f ((Get-Date) - $t0).TotalSeconds)
Start-Sleep 90
VBM snapshot ai-os take fresh --live
Write-Host ("login screen after about {0:n0} s. Log in as ai (password: {1}); the rail opens with the session." -f ((Get-Date) - $t0).TotalSeconds, $Password)
```

- [ ] **Step 2: `trial/vm/make-vm.cmd`** (CRLF, kept so by `.gitattributes`):

```bat
@echo off
rem WORKSHOP ONLY: build (or rebuild) the desktop edition VM. See make-vm.ps1.
powershell -NoProfile -ExecutionPolicy Bypass -File "%~dp0make-vm.ps1" %*
```

- [ ] **Step 3: Build the release in WSL**, then run the build from the PowerShell tool and time it. The release build: `wsl -d ai-os -u ai -- bash -l /mnt/c/Users/gdoum/Desktop/projects/ai-os/trial/run-tests.sh build --release` (several minutes the first time). Then: `cmd /c C:\Users\gdoum\Desktop\projects\ai-os\trial\vm\make-vm.cmd` with a 50-minute tool timeout, or in the background with the output file read afterwards. Expected: it ends with the "login screen after about N s" line and `VBoxManage snapshot list ai-os` shows `fresh`.

  If it fails before the guest reports ready: `VBoxManage controlvm ai-os screenshotpng <scratchpad>\vm.png` and read the image; the guest's log is at `/var/log/ai-os-setup.log` (readable through `VBoxManage guestcontrol ai-os copyfrom --username ai --password ai /var/log/ai-os-setup.log <scratchpad>\setup.log` once guest utils are running). Fix the cause in Task 3's files (they are the product of this plan; the seed is regenerated on every run) and rerun the command. Each rerun costs a full first boot; the OVA download is cached.

- [ ] **Step 4: The three measurements** (desktop design §6). From the host:
  - time from `startvm` to the login screen: the script prints it;
  - the model from the guest: `VBoxManage guestcontrol ai-os run --username ai --password ai -- /bin/bash /mnt/repo/trial/vm/check.sh` — expected last line `desktop edition ready`. If the `10.0.2.2` line fails, apply §5's fallback (his steps: `setx OLLAMA_HOST 0.0.0.0`, restart Ollama, one inbound rule for 11434) and record which it was;
  - GNOME usability is his judgement at the keyboard (typing, dragging a window); record "pending his report" and leave the line for him.

  A screenshot of the login screen (`controlvm ai-os screenshotpng`) goes into the scratchpad, not the repo.

- [ ] **Step 5: Record.** Append to §6 of `docs/superpowers/specs/2026-09-18-desktop-edition-design.md`:

```markdown
### Results (first build, 2026-09-18)

- `make-vm.cmd`: <N> s from `startvm` to "ready", about <M> s to the login screen; OVA cached at `C:\VM\ai-os\`.
- `check.sh` from the host: `desktop edition ready` — the Windows Ollama answers at `10.0.2.2:11434` with no change on Windows (or: the fallback was needed: <which>).
- GNOME under the Windows hypervisor layer: <his judgement, pending>.
- Snapshot `fresh` taken.
```

  And append to `docs/superpowers/specs/2026-09-15-ai-os-design.md`:

```markdown
## The desktop edition (2026-09-18)

**Design agreed and built** — `2026-09-18-desktop-edition-design.md`, plan `2026-09-18-desktop-edition.md`. §5.2's full Ubuntu edition, pulled forward from Phase 7 at his request so he uses the AI OS as a person at a desktop while the later phases are built: a VirtualBox VM from Ubuntu's 26.04 cloud image, GNOME, user `ai`, the engine as his service, the rail opening with the session, `/data` on btrfs on a second disk, the model on the Windows Ollama over HTTP (`AI_OS_MODEL_URL`, the one product change, plus the rail's desktop entry). One command rebuilds it: `trial\vm\make-vm.cmd`. The WSL distro stays the build-and-test workshop. Phase 2b and the rest continue inside it.
```

- [ ] **Step 6: Commit.**

```bash
git add trial/vm/make-vm.ps1 trial/vm/make-vm.cmd docs/superpowers/specs/2026-09-18-desktop-edition-design.md docs/superpowers/specs/2026-09-15-ai-os-design.md
git commit -m "workshop(vm): make-vm builds the desktop edition; first build recorded"
```

---

## Self-review

- **Spec coverage:** §1–2 decisions → constraints and Task 4's sizes; §3 host steps 1–5 → Task 4 step 1; §3 guest first boot → Task 3 step 2; §3 setup steps 1–9 → Task 3 step 4 and Task 4's snapshot; §4 → Tasks 1–2; §5 → Task 3's unit and Task 4 step 4; §6 → Task 3 step 5 and Task 4 steps 3–5; §7–8 are scope and risks, no task. `setup-admin.sh` installs `cargo npm python3-venv` for the fetch verbs, unchanged, as §3 says.
- **Placeholders:** the `<N>`, `<M>`, `<which>` in Task 4 step 5 are the measured values to fill in, named as such.
- **Consistency:** `from_env` in Task 1 and the bin; `org.aios.Rail.desktop` in Tasks 2 and 3; `/ai-os/setup` in Tasks 3 and 4; share `repo` at `/mnt/repo` in the user-data, setup, check and host script; SATA ports 0/1/2 match `/dev/sdb` for the data disk in `setup-vm.sh` (the DVD is not a `/dev/sd*`).
