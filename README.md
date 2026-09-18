# AI OS

A local AI that does jobs on your Ubuntu machine: it writes and changes files, installs software,
changes settings, and works the windows you hand it — and it keeps a record of what it did, so you
can read it back and undo it. It thinks with a model that runs on your own machine through Ollama;
nothing you give it is sent anywhere. It is early, and it is one person's project.

## What you need

- Ubuntu 26.04 desktop, x86_64, and a user account that can use `sudo`.
- A graphics card with 8 GB of memory for the model the installer downloads by default. Without one
  the model runs on the processor and is slow. If the model already runs on another machine, point
  the AI OS at it with `--model-url` instead and no model is downloaded here.

## Installing it

One command, run as yourself (it asks for your password when it needs root):

```
curl -fsSL https://raw.githubusercontent.com/gdoumou85/ai-os/master/install/get.sh | bash
```

To use a model runner on another machine instead of installing one here:

```
curl -fsSL https://raw.githubusercontent.com/gdoumou85/ai-os/master/install/get.sh | bash -s -- --model-url http://HOST:11434
```

## What it changes on your machine

- A file `/var/lib/ai-os/data.img`, formatted btrfs and mounted at `/data`, where the AI keeps its
  work. It takes half the free space, at most 50 GB, and grows only as it fills. One line is added
  to `/etc/fstab` so it mounts at every boot.
- A system account `ai-sandbox`, which owns the folders a job runs in. Your account is added to its
  group.
- A root helper at `/usr/local/libexec/ai-os-admin` and one line in `/etc/sudoers.d/ai-os-admin`
  that lets your user run that one helper without a password. That is the only root power the AI
  has; everything else asks for a password it does not have.
- Three programs in `/usr/local/bin`: `ai-os-engine`, `ai-os-chat`, `ai-os-rail`.
- A service `ai-os-engine` under your own user account, started at login and kept running.
- The chat window's entry in `/usr/share/applications`, so it is in the app grid, and in
  `/etc/xdg/autostart`, so it opens when you log in.
- Ollama, its service settings, and the model — only when you did not pass `--model-url`.

Restart the computer once after it finishes. The engine picks up its new group membership then, and
the chat window opens when you log back in.

## Checking it

```
bash check.sh
```

`AI OS ready` means the disk, the sandbox account, the root helper, the engine, its socket, the
model runner and the chat window's autostart entry are all in place. Anything else is the first
thing it found wrong, in plain words.

## Things to know

- There is no uninstaller yet. Removing it is by hand.
- The download is trusted through GitHub over HTTPS and nothing more: the release is not signed yet.
- No licence is granted. The code is readable here; all rights are reserved.
- The design this is built from is in `docs/superpowers/specs/`.
