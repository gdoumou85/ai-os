#!/usr/bin/env bash
# Adds the AI OS to an Ubuntu 26.04 desktop, for the user who runs it (desktop design §9.2).
#   bash install.sh [--model-url http://host:port] [--model NAME] [--model-key KEY] [--update]
# With no --model-url it looks for Ollama and LM Studio on the home network and asks which to use.
# Run it as yourself, not as root: it asks sudo for the root steps. Safe to run again.
set -euo pipefail
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
model=""; model_url=""; model_key=""; context=""; update=0
while [ $# -gt 0 ]; do case "$1" in
  --model-url) model_url=${2:?--model-url needs a value}; shift 2 ;;
  --model)     model=${2:?--model needs a value}; shift 2 ;;
  --model-key) model_key=${2:?--model-key needs a value}; shift 2 ;;
  --update)    update=1; shift ;;
  *) echo "unknown option: $1" >&2; exit 2 ;;
esac; done
[ "$(id -u)" -ne 0 ] || { echo "run this as your own user, not as root" >&2; exit 1; }
owner=$(id -un); uid=$(id -u); ogroup=$(id -gn)
[ "$(uname -m)" = x86_64 ] || { echo "only x86_64 is built" >&2; exit 1; }
. /etc/os-release; [ "${VERSION_ID:-}" = 26.04 ] || echo "warning: built and tested on Ubuntu 26.04, this is ${PRETTY_NAME:-unknown}" >&2
for f in bin/ai-os-engine bin/ai-os-chat bin/ai-os-rail bin/ai-os-find org.aios.Rail.desktop check.sh ai-os-engine.service.in; do
  [ -f "$here/$f" ] || { echo "missing next to install.sh: $f" >&2; exit 1; }
done
echo "== installing the AI OS for $owner (sudo will ask for your password)"
sudo -v

echo "== packages"
sudo env DEBIAN_FRONTEND=noninteractive apt-get update
sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y btrfs-progs libgtk-4-1 curl gnome-text-editor \
  python3-venv npm cargo at-spi2-core \
  gstreamer1.0-tools gstreamer1.0-pipewire imagemagick   # the screen hand: a frame, and the grid drawn on it

echo "== /data (btrfs, where the AI keeps its projects)"
if ! mountpoint -q /data; then
  # Files already at /data would be re-owned here and then hidden under the mount, with no way for
  # the person to guess where they went. Stop while they are still visible.
  if [ -n "$(sudo ls -A /data 2>/dev/null)" ]; then
    echo "/data already has files in it; move them aside first — the AI OS needs /data for itself" >&2; exit 1
  fi
  sudo install -d /var/lib/ai-os /data
  if [ ! -f /var/lib/ai-os/data.img ]; then
    free_g=$(df -BG --output=avail /var/lib/ai-os | tail -1 | tr -dc 0-9)
    size_g=$(( free_g / 2 )); [ "$size_g" -le 50 ] || size_g=50
    [ "$size_g" -ge 5 ] || { echo "less than 10 GB free: not enough for /data" >&2; exit 1; }
    sudo truncate -s "${size_g}G" /var/lib/ai-os/data.img     # sparse: takes space as it fills
  fi
  # On the filesystem, not on the file: a run stopped between truncate and mkfs leaves the image
  # there but empty, and the next run has to finish the job rather than mount nothing.
  sudo blkid /var/lib/ai-os/data.img >/dev/null 2>&1 || sudo mkfs.btrfs -q -L ai-os-data /var/lib/ai-os/data.img
  # nofail: a missing or broken image must not drop the desktop into an emergency shell at boot.
  # The leading newline is the point: an /etc/fstab whose last line has no newline of its own
  # would otherwise swallow this one, and the mount would never be read.
  grep -q ' /data ' /etc/fstab || printf '\n%s\n' '/var/lib/ai-os/data.img /data btrfs loop,noatime,compress=zstd,nofail 0 0' | sudo tee -a /etc/fstab >/dev/null
  sudo mount /data
fi
[ "$(findmnt -no FSTYPE /data)" = btrfs ] || { echo "/data is mounted but is not btrfs — unmount it (and take its line out of /etc/fstab) and run this again" >&2; exit 1; }
# The engine runs as you and keeps its database at /data/ai-os.db: the mount root has to be yours.
sudo chown "$owner:$ogroup" /data
[ -d /data/live ] || sudo btrfs subvolume create /data/live >/dev/null
sudo chown "$owner:$ogroup" /data/live

echo "== the AI's folders, yours"
sudo install -d -o "$owner" -g "$ogroup" -m 0755 /data/projects /data/housekeeping
# Before full access (v0.8.0) a sandbox account wrote in these: hand everything back to you.
sudo chown -R "$owner:$ogroup" /data/projects /data/housekeeping

echo "== full access: root for the AI, with no password (the VM is the safety net)"
echo "$owner ALL=(ALL) NOPASSWD: ALL" | sudo tee /etc/sudoers.d/ai-os.tmp >/dev/null
sudo chmod 0440 /etc/sudoers.d/ai-os.tmp
sudo visudo -cf /etc/sudoers.d/ai-os.tmp >/dev/null || {
  sudo rm -f /etc/sudoers.d/ai-os.tmp
  echo "the permission line for $owner did not pass visudo; nothing was changed" >&2; exit 1; }
sudo mv -f /etc/sudoers.d/ai-os.tmp /etc/sudoers.d/ai-os
# The root helper of earlier versions, and its narrow permission, are gone.
sudo rm -f /etc/sudoers.d/ai-os-admin /usr/local/libexec/ai-os-admin

echo "== programs"
for b in ai-os-engine ai-os-chat ai-os-rail ai-os-find; do sudo install -m 0755 -o root -g root "$here/bin/$b" /usr/local/bin/$b; done
# Which build this is, for check.sh and for anyone reporting a problem. Older tarballs have none.
if [ -f "$here/VERSION" ]; then
  sudo install -d /usr/local/share/ai-os
  sudo install -m 0644 "$here/VERSION" /usr/local/share/ai-os/VERSION
fi
# The check outlives the unpacked tarball, which get.sh deletes on its way out.
sudo install -m 0755 "$here/check.sh" /usr/local/bin/ai-os-check
sudo install -m 0644 "$here/org.aios.Rail.desktop" /usr/share/applications/org.aios.Rail.desktop
sudo install -d /etc/xdg/autostart
# Machine-wide, not per user: this edition assumes one person per machine.
sudo install -m 0644 "$here/org.aios.Rail.desktop" /etc/xdg/autostart/org.aios.Rail.desktop
# A tarball unpacked from a machine that rewrote line endings would leave `#!/usr/bin/env bash\r`
# in the wrapper and a stray \r in every desktop-entry value.
sudo sed -i 's/\r$//' /usr/local/bin/ai-os-check \
  /usr/share/applications/org.aios.Rail.desktop /etc/xdg/autostart/org.aios.Rail.desktop

echo "== the model"
# --update (the rail's Update now): the model chosen last time, read back from the unit and the
# key file, so an update asks nothing but the password.
saved_kind=""; native_update=0
if [ "$update" = 1 ]; then
  unit_file="$HOME/.config/systemd/user/ai-os-engine.service"
  [ -f "$unit_file" ] || { echo "nothing to update: the AI OS is not installed for $owner yet — run the install command without --update" >&2; exit 1; }
  # What the engine runs with now — the chat window's Model button writes a drop-in over the
  # unit, so the unit file alone can be out of date. The file is the fallback.
  env_now=$(systemctl --user show ai-os-engine.service -p Environment --value 2>/dev/null | tr ' ' '\n' || true)
  saved() { { printf '%s\n' "$env_now" | sed -n "s/^$1=//p"; sed -n "s/^Environment=$1=//p" "$unit_file"; } | sed -n 1p; }
  # `sed -n 1p`, not `head -1`: head leaves early, and under pipefail the SIGPIPE it hands the
  # writers would stop the whole installer here.
  model=${model:-$(saved AI_OS_MODEL)}; model_url=${model_url:-$(saved AI_OS_MODEL_URL)}; saved_kind=$(saved AI_OS_MODEL_KIND)
  # How much it can hold (the Model card's bar): lost here, every update reset it to 8k (the owner, 2026-09-23).
  context=$(saved AI_OS_CONTEXT)
  model_key=${model_key:-$(sed -n 's/^AI_OS_MODEL_KEY=//p' "$HOME/.config/ai-os/model.env" 2>/dev/null || true)}
  [ -n "$model_url" ] || native_update=1
  echo "keeping ${model:-the model} ${model_url:+at $model_url}"
fi
# ai-os-find prints one model per line: kind<TAB>url<TAB>model, or kind<TAB>url<TAB>-<TAB>needs-key.
# The key goes to it through the environment, never on a command line anyone can read with ps.
find_models() { AI_OS_MODEL_KEY="$model_key" "$here/bin/ai-os-find" "$@" || true; }
# Shows the lines as a numbered list and prints the one picked; with offer-native=1 a last choice
# installs Ollama here instead and prints "native". Questions go to the terminal itself: under
# `curl | bash` stdin is this script.
pick() {   # pick <offer-native> <line>...
  local native=$1; shift
  local n=$# i=0 line k u m choice
  [ -r /dev/tty ] || { echo "no terminal to ask on — pass --model-url and --model" >&2; exit 1; }
  for line in "$@"; do
    i=$((i+1)); IFS=$'\t' read -r k u m _ <<<"$line"
    if [ "$k" = openai ]; then k="LM Studio"; else k=Ollama; fi
    [ "$m" != - ] || m="(needs its API key)"
    printf '%3d) %-28s %-10s %s\n' "$i" "$u" "$k" "$m" >/dev/tty
  done
  [ "$native" = 0 ] || printf '%3d) install Ollama on this machine instead (slow without a GPU)\n' $((n+1)) >/dev/tty
  local max=$((n+native))
  [ "$max" -gt 0 ] || { echo "nothing to choose from" >&2; exit 1; }
  while :; do
    read -rp "which one (1-$max)? " choice </dev/tty || exit 1
    case "$choice" in ''|*[!0-9]*) continue ;; esac
    [ "$choice" -ge 1 ] && [ "$choice" -le "$max" ] && break
  done
  if [ "$choice" -gt "$n" ]; then echo native; else echo "${!choice}"; fi
}
if [ -n "$model_url" ]; then
  mapfile -t found < <(find_models --url "$model_url")
  if [ ${#found[@]} -eq 0 ] && [ "$update" = 1 ]; then
    # An update must not fail because the model's machine is switched off right now.
    echo "warning: $model_url is not answering right now; keeping it as it was" >&2
    found=("${saved_kind:-ollama}"$'	'"$model_url"$'	'"$model")
  fi
  [ ${#found[@]} -gt 0 ] || { echo "nothing answered at $model_url as Ollama or LM Studio — check the address, and that the runner accepts connections from the network" >&2; exit 1; }
  if [ -n "$model" ] || [ ${#found[@]} -eq 1 ]; then line=${found[0]}; else line=$(pick 0 "${found[@]}") || exit 1; fi
elif [ "$native_update" = 1 ]; then
  line=native
else
  echo "looking for Ollama and LM Studio on your network — a few seconds"
  mapfile -t found < <(find_models)
  [ ${#found[@]} -gt 0 ] || echo "none found: to use another machine, turn on its runner's network setting and run this again"
  line=$(pick 1 "${found[@]}") || exit 1
fi
kind=ollama
if [ "$line" != native ]; then
  IFS=$'\t' read -r kind model_url m _ <<<"$line"
  if [ "$m" = - ]; then
    # Not echoed; kept only in a file only this account can read (below).
    read -rsp "that LM Studio needs its API key (LM Studio → Developer → Server settings): " model_key </dev/tty; echo
    mapfile -t found < <(find_models --url "$model_url")
    case "${found[0]:-}" in ''|*$'\t-\t'*) echo "LM Studio did not accept that key" >&2; exit 1 ;; esac
    if [ -n "$model" ] || [ ${#found[@]} -eq 1 ]; then line=${found[0]}; else line=$(pick 0 "${found[@]}") || exit 1; fi
    IFS=$'\t' read -r kind model_url m _ <<<"$line"
  fi
  [ -n "$model" ] || model=$m
  # Captured whole, not piped into grep -q: under pipefail an early grep exit fails the pipeline.
  case $'\n'"$(printf '%s\n' "${found[@]}" | cut -f3)"$'\n' in
    *$'\n'"$model"$'\n'*) ;;
    *) echo "warning: $model_url does not list $model — the AI will say so until it does" >&2 ;;
  esac
  # Names and addresses come off the network and go into a unit file: plain characters only, so
  # none can smuggle in a line of its own (the chat window's Model button checks the same).
  for v in "$model" "$model_url"; do
    [[ $v =~ ^[A-Za-z0-9._:/@+-]+$ ]] || { echo "refusing an odd model name or address: $v" >&2; exit 1; }
  done
  echo "using $model at $model_url"
  [ "$kind" = ollama ] || echo "in LM Studio, load $model with a context length of at least 8192"
  url_line="Environment=AI_OS_MODEL_URL=$model_url"$'\n'"Environment=AI_OS_MODEL_KIND=$kind"
else
  model_url=""; model=${model:-qwen3.5:9b}
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
  ollama pull "$model" || {      # several GB, once
    echo "the model $model did not download — check the name and the network, then run this command again" >&2; exit 1; }
  sudo -v      # the download can outlast sudo's timestamp, and the rest of this still needs root
  # Captured rather than piped into grep, for the same reason as the tags check above.
  gpu=$(lspci 2>/dev/null || true)
  command -v nvidia-smi >/dev/null || grep -qiE 'vga.*(amd|radeon)' <<<"$gpu" || echo "note: no NVIDIA or AMD graphics driver found — the model will run on the processor, slowly. On NVIDIA: sudo ubuntu-drivers install, then restart." >&2
  url_line=""
fi
# A number or nothing: it goes into the unit file too.
[[ $context =~ ^[0-9]+$ ]] && url_line+=$'\n'"Environment=AI_OS_CONTEXT=$context"

echo "== the engine, as your service"
install -d "$HOME/.config/systemd/user"
# Filled in by the shell, not by sed, so a model name or a URL carrying a backslash or the
# delimiter needs no escaping. The replacements are quoted: unquoted, an & in them would stand
# for the placeholder it replaces.
unit=$(cat "$here/ai-os-engine.service.in")
unit=${unit//@MODEL@/"$model"}
unit=${unit//@MODEL_URL_LINE@/"$url_line"}
printf '%s\n' "$unit" > "$HOME/.config/systemd/user/ai-os-engine.service"
sed -i 's/\r$//' "$HOME/.config/systemd/user/ai-os-engine.service"
# The chat window's Model choice was folded into the unit above (an update reads it back first),
# so its drop-in would only shadow what was just chosen.
rm -f "$HOME/.config/systemd/user/ai-os-engine.service.d/model.conf"
# The LM Studio key: out of the unit (which any user can read) and in a file only you can.
install -d -m 0700 "$HOME/.config/ai-os"
if [ -n "$model_key" ]; then
  (umask 077; printf 'AI_OS_MODEL_KEY=%s\n' "$model_key" > "$HOME/.config/ai-os/model.env")
else
  rm -f "$HOME/.config/ai-os/model.env"
fi
sudo loginctl enable-linger "$owner"
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$uid}
# With nobody logged in, the user manager starts because of the linger just asked for, a moment
# after the command above returns. Wait for the manager itself, not just for its directory.
for _ in $(seq 20); do systemctl --user show -p Version >/dev/null 2>&1 && break; sleep 1; done
systemctl --user daemon-reload
systemctl --user enable ai-os-engine.service
systemctl --user restart ai-os-engine.service
gsettings set org.gnome.desktop.interface toolkit-accessibility true 2>/dev/null || true
xdg-mime default org.gnome.TextEditor.desktop text/x-python text/markdown text/plain text/x-shellscript application/json 2>/dev/null || true

sleep 2
# The check's verdict is the script's, but the restart instruction is printed either way: a failure
# here is usually the model runner, which the restart and a working network settle.
rc=0; bash "$here/check.sh" ${AI_OS_CHECK_ARGS:-} || rc=$?
# The chat window opens with the session, so the first one after a restart is where it appears.
echo "Restart the computer once. After that the chat window opens whenever you log in."
exit $rc
