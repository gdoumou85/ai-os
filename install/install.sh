#!/usr/bin/env bash
# Adds the AI OS to an Ubuntu 26.04 desktop, for the user who runs it (desktop design §9.2).
#   bash install.sh [--model-url http://host:11434] [--model qwen3.5:9b]
# Run it as yourself, not as root: it asks sudo for the root steps. Safe to run again.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
model=qwen3.5:9b; model_url=""
while [ $# -gt 0 ]; do case "$1" in
  --model-url) model_url=${2:?--model-url needs a value}; shift 2 ;;
  --model)     model=${2:?--model needs a value}; shift 2 ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac; done
[ "$(id -u)" -ne 0 ] || { echo "run this as your own user, not as root" >&2; exit 1; }
owner=$(id -un); uid=$(id -u); ogroup=$(id -gn)
[ "$(uname -m)" = x86_64 ] || { echo "only x86_64 is built" >&2; exit 1; }
. /etc/os-release; [ "${VERSION_ID:-}" = 26.04 ] || echo "warning: built and tested on Ubuntu 26.04, this is ${PRETTY_NAME:-unknown}" >&2
for f in bin/ai-os-engine bin/ai-os-chat bin/ai-os-rail ai-os-admin org.aios.Rail.desktop check.sh ai-os-engine.service.in; do
  [ -f "$here/$f" ] || { echo "missing next to install.sh: $f" >&2; exit 1; }
done
echo "== installing the AI OS for $owner (sudo will ask for your password)"
sudo -v

echo "== packages"
sudo env DEBIAN_FRONTEND=noninteractive apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y btrfs-progs libgtk-4-1 curl gnome-text-editor \
  python3-venv npm cargo

echo "== /data (btrfs, where the AI works and what undo covers)"
if ! mountpoint -q /data; then
  sudo install -d /var/lib/ai-os /data
  if [ ! -f /var/lib/ai-os/data.img ]; then
    free_g=$(df -BG --output=avail /var/lib/ai-os | tail -1 | tr -dc 0-9)
    size_g=$(( free_g / 2 )); [ "$size_g" -le 50 ] || size_g=50
    [ "$size_g" -ge 5 ] || { echo "less than 10 GB free: not enough for /data" >&2; exit 1; }
    sudo truncate -s "${size_g}G" /var/lib/ai-os/data.img     # sparse: takes space as it fills
    sudo mkfs.btrfs -q -L ai-os-data /var/lib/ai-os/data.img
  fi
  grep -q ' /data ' /etc/fstab || echo '/var/lib/ai-os/data.img /data btrfs loop,noatime,compress=zstd 0 0' | sudo tee -a /etc/fstab >/dev/null
  sudo mount /data
fi
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || { echo "/data is mounted but is not btrfs" >&2; exit 1; }
[ -d /data/live ] || sudo btrfs subvolume create /data/live >/dev/null
sudo chown "$owner:$ogroup" /data/live

echo "== the sandbox account"
id ai-sandbox >/dev/null 2>&1 || sudo useradd --system --no-create-home --shell /usr/sbin/nologin ai-sandbox
sudo usermod -aG ai-sandbox "$owner"
sudo install -d -o ai-sandbox -g ai-sandbox -m 0770 /data/jobs
sudo install -d -o "$owner" -g ai-sandbox -m 2770 /data/projects /data/snapshots /data/housekeeping

echo "== the root helper and its one permission"
sudo install -d -m 0755 /usr/local/libexec
sudo install -m 0755 -o root -g root "$here/ai-os-admin" /usr/local/libexec/ai-os-admin
echo "$owner ALL=(root) NOPASSWD: /usr/local/libexec/ai-os-admin" | sudo tee /etc/sudoers.d/ai-os-admin.tmp >/dev/null
sudo chmod 0440 /etc/sudoers.d/ai-os-admin.tmp
sudo visudo -cf /etc/sudoers.d/ai-os-admin.tmp >/dev/null
sudo mv -f /etc/sudoers.d/ai-os-admin.tmp /etc/sudoers.d/ai-os-admin

echo "== programs"
for b in ai-os-engine ai-os-chat ai-os-rail; do sudo install -m 0755 -o root -g root "$here/bin/$b" /usr/local/bin/$b; done
sudo install -m 0644 "$here/org.aios.Rail.desktop" /usr/share/applications/org.aios.Rail.desktop
sudo install -d /etc/xdg/autostart
sudo install -m 0644 "$here/org.aios.Rail.desktop" /etc/xdg/autostart/org.aios.Rail.desktop

echo "== the model"
if [ -n "$model_url" ]; then
  # Asked for as a whole answer, not piped into grep: under pipefail, `curl | grep -q` reports the
  # pipeline as failed when grep finds the name and stops reading before curl has finished writing.
  tags=$(curl -fsS --max-time 5 "$model_url/api/tags" 2>/dev/null) || tags=""
  case "$tags" in
    *"\"$model\""*) echo "the runner at $model_url has $model" ;;
    *) echo "warning: $model_url did not answer with $model — the AI will say so until it does" >&2 ;;
  esac
  url_line="Environment=AI_OS_MODEL_URL=$model_url"
else
  # A native install: the runner lives on this machine. Untested until a machine with a GPU runs it
  # (desktop design §9.2). The two settings are Phase 0's measurement: they are what keeps an 8k
  # context fully on an 8 GB GPU.
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y zstd     # Ollama's installer unpacks with it
  command -v ollama >/dev/null || curl -fsSL https://ollama.com/install.sh | sh
  sudo install -d /etc/systemd/system/ollama.service.d
  printf '[Service]\nEnvironment=OLLAMA_FLASH_ATTENTION=1\nEnvironment=OLLAMA_KV_CACHE_TYPE=q8_0\n' \
    | sudo tee /etc/systemd/system/ollama.service.d/ai-os.conf >/dev/null
  sudo systemctl daemon-reload; sudo systemctl enable --now ollama; sudo systemctl restart ollama
  for _ in $(seq 30); do curl -fsS --max-time 2 http://127.0.0.1:11434/api/tags >/dev/null 2>&1 && break; sleep 1; done
  ollama pull "$model"      # several GB, once
  command -v nvidia-smi >/dev/null || lspci 2>/dev/null | grep -qiE 'vga.*(amd|radeon)'     || echo "note: no NVIDIA or AMD graphics driver found — the model will run on the processor, slowly. On NVIDIA: sudo ubuntu-drivers install, then restart." >&2
  url_line=""
fi

echo "== the engine, as your service"
install -d "$HOME/.config/systemd/user"
# Filled in by the shell, not by sed, so a model name or a URL carrying a backslash or the
# delimiter needs no escaping. The replacements are quoted: unquoted, an & in them would stand
# for the placeholder it replaces.
unit=$(cat "$here/ai-os-engine.service.in")
unit=${unit//@MODEL@/"$model"}
unit=${unit//@MODEL_URL_LINE@/"$url_line"}
printf '%s\n' "$unit" > "$HOME/.config/systemd/user/ai-os-engine.service"
sudo loginctl enable-linger "$owner"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$uid}
# With nobody logged in, logind makes the runtime directory as lingering starts the user manager,
# a moment after the command above returns.
for _ in $(seq 20); do [ -d "$XDG_RUNTIME_DIR" ] && break; sleep 1; done
systemctl --user daemon-reload
systemctl --user enable ai-os-engine.service
systemctl --user restart ai-os-engine.service
gsettings set org.gnome.desktop.interface toolkit-accessibility true 2>/dev/null || true
xdg-mime default org.gnome.TextEditor.desktop text/x-python text/markdown text/plain text/x-shellscript application/json 2>/dev/null || true

sleep 2
bash "$here/check.sh" ${AI_OS_CHECK_ARGS:-}
# The engine runs under your user manager, which lingers across logouts and so keeps its old groups:
# only a restart hands it the ai-sandbox membership it needs to make job folders.
echo "Restart the computer once. After that the chat window opens whenever you log in."
