# Machine map — design (2026-09-23)

The owner, on v0.8.7: *"it gives me the feeling that it doesn't actually OWN the OS. What we are
missing is a system initialization, where the LLM understands what the OS is, what installed apps
already exist."* Then: *"when the AI installs something new, it should be installed in a way that
the discovery would catch it"*, and: *"I don't want a built-in guide. I want it only to discover
what programs already exist in this machine so it won't run off installing something already
existing."*

Today every job starts blind: the prompt says "Ubuntu Linux (python3…)", and whether Blender is
here is found by `run_command --help` or not at all. The owner has had to say "Blender is already
installed" himself, and runs have tried to install a Firefox that was already there.

## Decisions (the owner's)

- **Discovery only.** The engine scans what is installed and tells the model. No built-in guide on
  how to use programs: how to drive one stays with `--help` and the skill notebooks, as now.
- **The owner can see it**: a read-only *Installed on this computer* section at the top of Skills.
- **Installs are discoverable**: the AI installs the standard way (apt, snap, flatpak) so the scan
  sees it, and the engine records what its own install commands added, so windowless tools show too.

## 1. The scan — `core/src/machine.rs`

`machine::scan(app_dirs, path_dirs, store) -> Machine`, with no model involved:

- **System** (read once per engine process): `PRETTY_NAME` from `/etc/os-release`, memory from
  `/proc/meminfo`, the graphics line from `lspci` (VGA/3D), free space on `/data`, "root through
  sudo". One line.
- **Programs** (every job start, every front-door turn, every Skills request; a directory read, a
  few milliseconds): every `.desktop` file in `executor::atspi::APP_DIRS` (made `pub`) with
  `Type=Application` and not `NoDisplay=true`/`Hidden=true`. Kept: desktop id, `Name=`, the
  program of `Exec=` (first word, env prefixes and field codes dropped). De-duplicated by id,
  sorted by name.
- **Tools**: a fixed list looked up on `PATH` — python3, pip3, git, node, npm, cargo, ffmpeg,
  convert, curl, wget, sqlite3, docker, blender, soffice, gimp, inkscape. Only those present.
- **Installed by the AI**: the rows of the `installed` table (§3).

Tested against a temp folder of `.desktop` files (hidden, NoDisplay, a snap's `x_x` id, field codes
in Exec) and a fake `PATH`.

## 2. What the model sees

The front door and every job turn get a *This machine* block in place of today's one-line
"Machine: Ubuntu Linux (…)":

```
This machine: Ubuntu 26.04 LTS, 16 GB memory, NVIDIA GeForce RTX 3060, 120 GB free, root through sudo.
Programs (open_app name): Blender (blender), Firefox (firefox_firefox), Text Editor (org.gnome.TextEditor), …
Tools: python3, git, ffmpeg, …
Installed by you: ffmpeg (apt), yt-dlp (snap)
```

Program list capped at 60 entries. The system text says: *what is listed is installed — never
install it again; use it.* The front door stops needing "Blender is already installed".

## 3. Installs the scan catches

- **Rule, in the system text**: install with `sudo apt-get install -y`, else `snap install`, else
  `flatpak install -y flathub`. Never leave a loose download (AppImage, unpacked archive): such a
  program goes under `/opt/<name>` with its launcher written to
  `/usr/local/share/applications/<name>.desktop`, which the scan reads. Language packages stay in
  the project's own `.venv` / `node_modules`, as now.
- **Record**: new table `installed(package TEXT PRIMARY KEY, via TEXT, at INTEGER)`. After a
  `run_command` step that succeeded, the engine reads its command for `apt-get install`/`apt
  install`, `snap install`, `flatpak install` (commands split on `&&`, `;`, `|`; `sudo`, `VAR=x`
  prefixes, flags and a `flathub` remote dropped) and adds each package; `apt-get remove|purge`,
  `apt remove`, `snap remove`, `flatpak uninstall` delete them. Clear keeps this table: it is a fact
  about the machine, not memory.
  ponytail: parsed from the command text; a package removed by hand outside the AI stays listed
  until the AI removes it or a `dpkg-query` check is added.

Tested with commands as runs write them (`sudo apt-get update && sudo apt-get install -y blender`,
`sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ffmpeg imagemagick`, `flatpak install -y
flathub org.gimp.GIMP`); a failed step records nothing.

## 4. The Skills window

`skills_event` puts a notebook named **Installed on this computer** first: one entry per program
(topic = name, text = "open_app <id>") and one per AI-installed package (text = "installed by the
AI with <via>"). Kind `program`, which the rail draws without the ✕ (nothing to forget: it is what
is on disk). No protocol change.

## 5. Out of scope

- Any guide on how to use a program (the owner's call).
- An *Initialize* button: the scan runs every turn.
- A model-run setup job that explores the machine.

## Help guide

A *What the AI knows about this computer* paragraph: it sees what is installed and never installs
it twice, installs new programs so they show up, and the list is at the top of Skills.
