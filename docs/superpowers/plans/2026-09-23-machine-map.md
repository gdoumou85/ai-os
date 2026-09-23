# Machine Map Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The AI always knows what this machine has installed, so it uses it instead of installing it again (v0.9.0).

**Architecture:** A new `core/src/machine.rs` scans the system, the desktop entries and a tool list, and keeps an `installed` table of what the AI's own install commands added. The engine puts a *This machine* block in front of every front-door and job prompt and records installs after each successful `run_command`. The Skills window gets a read-only first notebook built from the same scan.

**Tech Stack:** Rust (core, executor, rail crates), rusqlite, GTK4 (rail). No local Rust on the dev PC: tests run in CI (`gh workflow run build.yml --ref <branch>`, `gh run watch`).

**Spec:** `docs/superpowers/specs/2026-09-23-machine-map-design.md`

## Global Constraints

- Discovery only: no guide on how to use a program.
- Install order in the system text: `sudo apt-get install -y`, else `snap install`, else `flatpak install -y flathub`; a loose download goes under `/opt/<name>` with a launcher in `/usr/local/share/applications/<name>.desktop`.
- Program list capped at 60 entries.
- Clear keeps the `installed` table.
- The Skills notebook is named `installed on this computer`, entries of kind `program`, drawn without ✕.
- Every visible feature edits `runtime/rail/src/guide.txt` in the same change.

---

### Task 1: `machine.rs` — scan, install parsing, the table, the block

**Files:**
- Create: `runtime/core/src/machine.rs`
- Modify: `runtime/core/src/lib.rs` (add `pub mod machine;`)
- Modify: `runtime/core/src/store.rs` (`init` calls `crate::machine::init(&conn)?`)
- Modify: `runtime/executor/src/atspi.rs:91` (`const APP_DIRS` → `pub const APP_DIRS`)
- Modify: `runtime/executor/src/worker.rs` (`pub(crate) fn installed` → `pub fn installed`)

**Interfaces:**
- Produces:
  - `pub struct App { pub id: String, pub name: String }`
  - `pub fn apps_in(dirs: &[&str]) -> Vec<App>`
  - `pub fn tools_on(path: &str) -> Vec<String>`
  - `pub fn system_line_from(os_release: &str, meminfo: &str, lspci: &str, df: &str) -> String`
  - `pub fn system_line() -> String` (reads the real files and commands)
  - `pub fn installs(argv: &[String]) -> Vec<(String, &'static str, bool)>` (package, via, true = added)
  - `pub fn init(c: &Connection) -> Result<(), StoreError>`, `pub fn record(c, argv: &[String]) -> Result<(), StoreError>`, `pub fn installed(c: &Connection) -> Result<Vec<(String, String)>, StoreError>`
  - `pub fn block(system: &str, apps: &[App], tools: &[String], installed: &[(String, String)]) -> String`
  - `pub fn current_block(c: &Connection, system: &str) -> String` (scans now, drops apt rows dpkg no longer has)

- [ ] **Step 1: Write the module with its tests**

```rust
//! What this machine has (the owner, 2026-09-23: "it doesn't actually OWN the OS"). The engine
//! scans it — no model involved — and every prompt carries the result, so the AI uses what is
//! installed instead of installing it again. Discovery only: how to use a program is `--help`'s
//! and the notebooks' (spec 2026-09-23-machine-map-design.md).
use crate::store::StoreError;
use rusqlite::Connection;

pub struct App { pub id: String, pub name: String }

/// Tools worth naming when present; programs with a window come from their desktop entries.
const TOOLS: [&str; 16] = ["python3", "pip3", "git", "node", "npm", "cargo", "ffmpeg", "convert", "curl", "wget",
    "sqlite3", "docker", "blender", "soffice", "gimp", "inkscape"];
const MAX_APPS: usize = 60;

/// The applications a person can open: `Type=Application`, not `NoDisplay`/`Hidden`, named by
/// the `[Desktop Entry]` section's own `Name=` (not a translation, not an action's).
pub fn apps_in(dirs: &[&str]) -> Vec<App> {
    let mut apps: Vec<App> = vec![];
    for d in dirs {
        let Ok(rd) = std::fs::read_dir(d) else { continue };
        for e in rd.flatten() {
            let file = e.file_name().to_string_lossy().to_string();
            let Some(id) = file.strip_suffix(".desktop") else { continue };
            let Ok(text) = std::fs::read_to_string(e.path()) else { continue };
            let (mut in_entry, mut name, mut app, mut hidden) = (false, None, false, false);
            for l in text.lines().map(str::trim) {
                if l.starts_with('[') { in_entry = l == "[Desktop Entry]"; continue; }
                if !in_entry { continue; }
                if let Some(v) = l.strip_prefix("Name=") { name.get_or_insert(v.to_string()); }
                if l == "Type=Application" { app = true; }
                if l == "NoDisplay=true" || l == "Hidden=true" { hidden = true; }
            }
            if let (Some(name), true, false) = (name, app, hidden) {
                if !apps.iter().any(|a| a.id == id) { apps.push(App { id: id.to_string(), name }); }
            }
        }
    }
    apps.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    apps
}

/// The tools of `TOOLS` found in a `PATH`-style list (`/snap/bin` always looked in too).
pub fn tools_on(path: &str) -> Vec<String> {
    let dirs: Vec<&str> = path.split(':').chain(["/snap/bin"]).filter(|d| !d.is_empty()).collect();
    TOOLS.iter().filter(|t| dirs.iter().any(|d| std::path::Path::new(d).join(t).is_file())).map(|t| t.to_string()).collect()
}

/// One line from what the system files and commands said; a part that said nothing is left out.
pub fn system_line_from(os_release: &str, meminfo: &str, lspci: &str, df: &str) -> String {
    let mut parts = vec![];
    if let Some(n) = os_release.lines().find_map(|l| l.strip_prefix("PRETTY_NAME=")) { parts.push(n.trim_matches('"').to_string()); }
    if let Some(kb) = meminfo.lines().find_map(|l| l.strip_prefix("MemTotal:")).and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok()) {
        parts.push(format!("{} GB memory", (kb + 512 * 1024) / (1024 * 1024)));
    }
    if let Some(g) = lspci.lines().find(|l| l.contains("VGA") || l.contains("3D controller")).and_then(|l| l.splitn(3, ": ").nth(1).or(l.split(": ").nth(1))) {
        parts.push(format!("graphics: {}", g.trim()));
    }
    if let Some(free) = df.lines().nth(1).map(str::trim).filter(|s| !s.is_empty()) { parts.push(format!("{free} free for projects")); }
    parts.push("root through sudo".into());
    parts.join(", ")
}

/// The system line of the machine this runs on.
pub fn system_line() -> String {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    let run = |c: &str, a: &[&str]| std::process::Command::new(c).args(a).output().map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    system_line_from(&read("/etc/os-release"), &read("/proc/meminfo"), &run("lspci", &[]), &run("df", &["-h", "--output=avail", "/data"]))
}

/// What an install or remove command does to the machine's programs: (package, via, added).
/// A `bash -c` line is split on `&&`, `;`, `|` and read part by part.
pub fn installs(argv: &[String]) -> Vec<(String, &'static str, bool)> {
    if matches!(argv.first().map(String::as_str), Some("bash" | "sh")) && argv.get(1).map(String::as_str) == Some("-c") {
        return argv.get(2).map(|line| line.split(['&', ';', '|', '\n'])
            .flat_map(|part| one(&part.split_whitespace().map(String::from).collect::<Vec<_>>())).collect()).unwrap_or_default();
    }
    one(argv)
}

fn one(argv: &[String]) -> Vec<(String, &'static str, bool)> {
    let mut w = argv.iter().map(String::as_str).skip_while(|a| matches!(*a, "sudo" | "env" | "-E") || (a.contains('=') && !a.starts_with('-')));
    let (via, added) = match (w.next(), w.next()) {
        (Some("apt-get" | "apt"), Some("install")) => ("apt", true),
        (Some("apt-get" | "apt"), Some("remove" | "purge")) => ("apt", false),
        (Some("snap"), Some("install")) => ("snap", true),
        (Some("snap"), Some("remove")) => ("snap", false),
        (Some("flatpak"), Some("install")) => ("flatpak", true),
        (Some("flatpak"), Some("uninstall" | "remove")) => ("flatpak", false),
        _ => return vec![],
    };
    w.filter(|a| !a.starts_with('-') && *a != "flathub" && !a.contains('/') && !a.ends_with(".deb"))
        .map(|a| (a.split('=').next().unwrap_or(a).to_string(), via, added)).collect()
}

pub fn init(c: &Connection) -> Result<(), StoreError> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS installed(package TEXT PRIMARY KEY, via TEXT NOT NULL, at INTEGER NOT NULL);")?;
    Ok(())
}

/// Records what a command that succeeded installed or removed.
pub fn record(c: &Connection, argv: &[String]) -> Result<(), StoreError> {
    for (pkg, via, added) in installs(argv) {
        if added {
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0);
            c.execute("INSERT OR REPLACE INTO installed(package, via, at) VALUES (?1, ?2, ?3)", (&pkg, via, at))?;
        } else {
            c.execute("DELETE FROM installed WHERE package = ?1", [&pkg])?;
        }
    }
    Ok(())
}

pub fn installed(c: &Connection) -> Result<Vec<(String, String)>, StoreError> {
    let mut st = c.prepare("SELECT package, via FROM installed ORDER BY package")?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The block every prompt starts with.
pub fn block(system: &str, apps: &[App], tools: &[String], installed: &[(String, String)]) -> String {
    let none = |v: String| if v.is_empty() { "(none found)".to_string() } else { v };
    let apps = none(apps.iter().take(MAX_APPS).map(|a| format!("{} ({})", a.name, a.id)).collect::<Vec<_>>().join(", "));
    let mut b = format!("This machine: {system}.\nPrograms (open_app name): {apps}\nTools: {}", none(tools.join(", ")));
    if !installed.is_empty() {
        b.push_str(&format!("\nInstalled by you: {}", installed.iter().map(|(p, v)| format!("{p} ({v})")).collect::<Vec<_>>().join(", ")));
    }
    b.push_str("\nWhat is listed here is installed: never install it again; use it.");
    b
}

/// The block as the machine is now. An apt package dpkg no longer has was removed by hand: gone.
pub fn current_block(c: &Connection, system: &str) -> String {
    let mine = installed(c).unwrap_or_default().into_iter().filter(|(p, v)| v != "apt" || executor::worker::installed(p)).collect::<Vec<_>>();
    block(system, &apps_in(&executor::atspi::APP_DIRS), &tools_on(&std::env::var("PATH").unwrap_or_default()), &mine)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn argv(s: &str) -> Vec<String> { s.split_whitespace().map(String::from).collect() }

    #[test]
    fn apps_are_the_ones_a_person_can_open() {
        let d = crate::testing::temp_root("machine-apps");
        std::fs::write(d.join("blender.desktop"), "[Desktop Entry]\nType=Application\nName=Blender\nName[de]=Blender DE\nExec=blender %f\n[Desktop Action new]\nName=New window\n").unwrap();
        std::fs::write(d.join("firefox_firefox.desktop"), "[Desktop Entry]\nName=Firefox\nType=Application\n").unwrap();
        std::fs::write(d.join("hidden.desktop"), "[Desktop Entry]\nName=Hidden\nType=Application\nNoDisplay=true\n").unwrap();
        std::fs::write(d.join("link.desktop"), "[Desktop Entry]\nName=A link\nType=Link\n").unwrap();
        std::fs::write(d.join("notes.txt"), "Name=x").unwrap();
        let dir = d.to_string_lossy().to_string();
        let apps = apps_in(&[&dir, &dir, "/no/such/dir"]);
        assert_eq!(apps.iter().map(|a| (a.name.as_str(), a.id.as_str())).collect::<Vec<_>>(), vec![("Blender", "blender"), ("Firefox", "firefox_firefox")]);
    }

    #[test]
    fn tools_are_found_on_the_path() {
        let d = crate::testing::temp_root("machine-tools");
        std::fs::write(d.join("git"), "").unwrap();
        std::fs::write(d.join("ffmpeg"), "").unwrap();
        assert_eq!(tools_on(&format!("/no/such:{}", d.display())), vec!["git".to_string(), "ffmpeg".to_string()]);
    }

    #[test]
    fn the_system_line_says_what_the_files_say() {
        let l = system_line_from("NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 26.04 LTS\"\n", "MemTotal:       16318412 kB\n",
            "00:02.0 VGA compatible controller: NVIDIA Corporation GA106 [GeForce RTX 3060]\n", " Avail\n  120G\n");
        assert_eq!(l, "Ubuntu 26.04 LTS, 16 GB memory, graphics: NVIDIA Corporation GA106 [GeForce RTX 3060], 120G free for projects, root through sudo");
        assert_eq!(system_line_from("", "", "", ""), "root through sudo");
    }

    #[test]
    fn install_commands_are_read_as_runs_write_them() {
        assert_eq!(installs(&argv("sudo apt-get install -y blender")), vec![("blender".into(), "apt", true)]);
        assert_eq!(installs(&argv("sudo DEBIAN_FRONTEND=noninteractive apt-get install -y ffmpeg imagemagick")), vec![("ffmpeg".into(), "apt", true), ("imagemagick".into(), "apt", true)]);
        assert_eq!(installs(&argv("flatpak install -y flathub org.gimp.GIMP")), vec![("org.gimp.GIMP".into(), "flatpak", true)]);
        assert_eq!(installs(&argv("sudo snap install yt-dlp --classic")), vec![("yt-dlp".into(), "snap", true)]);
        assert_eq!(installs(&argv("sudo apt-get remove -y cowsay")), vec![("cowsay".into(), "apt", false)]);
        let line = vec!["bash".into(), "-c".into(), "sudo apt-get update && sudo apt-get install -y blender".into()];
        assert_eq!(installs(&line), vec![("blender".into(), "apt", true)]);
        for other in ["ls -la", "sudo apt-get update", "pip install numpy", "sudo apt-get install ./x.deb"] {
            assert!(installs(&argv(other)).is_empty(), "{other}");
        }
    }

    #[test]
    fn the_table_keeps_what_was_installed_until_it_is_removed() {
        let c = Connection::open_in_memory().unwrap();
        init(&c).unwrap();
        record(&c, &argv("sudo apt-get install -y cowsay ffmpeg")).unwrap();
        record(&c, &argv("sudo apt-get remove -y cowsay")).unwrap();
        assert_eq!(installed(&c).unwrap(), vec![("ffmpeg".to_string(), "apt".to_string())]);
    }

    #[test]
    fn the_block_lists_everything_and_says_not_to_install_it_again() {
        let apps = vec![App { id: "blender".into(), name: "Blender".into() }];
        let b = block("Ubuntu 26.04 LTS, root through sudo", &apps, &["git".into()], &[("ffmpeg".into(), "apt".into())]);
        assert!(b.starts_with("This machine: Ubuntu 26.04 LTS, root through sudo.\nPrograms (open_app name): Blender (blender)\nTools: git\nInstalled by you: ffmpeg (apt)"), "{b}");
        assert!(b.contains("never install it again"));
        assert!(block("x", &[], &[], &[]).contains("Programs (open_app name): (none found)"));
        let many: Vec<App> = (0..80).map(|i| App { id: format!("a{i}"), name: format!("A{i}") }).collect();
        assert_eq!(block("x", &many, &[], &[]).matches(" (a").count(), MAX_APPS);
    }
}
```

In `lib.rs` add `pub mod machine;` after `pub mod learn;`. In `store.rs` `init`, after `crate::notes::init(&conn)?;` add `crate::machine::init(&conn)?;`. Make `APP_DIRS` and `worker::installed` `pub`.

- [ ] **Step 2: Push the branch and run CI**

Run: `git -c credential.helper='!gh auth git-credential' push -u origin machine-map && gh workflow run build.yml --ref machine-map`, then `gh run watch <id> --exit-status`.
Expected: the six `machine::tests` pass (the system line test is the likeliest to need its `lspci` split fixed: the expected graphics text is everything after the class's `": "`).

- [ ] **Step 3: Commit**

```bash
git add runtime/core/src/machine.rs runtime/core/src/lib.rs runtime/core/src/store.rs runtime/executor/src/atspi.rs runtime/executor/src/worker.rs
git commit -m "core: machine.rs scans what this machine has"
```

### Task 2: The engine uses it

**Files:**
- Modify: `runtime/core/src/engine.rs` (field `system: std::cell::OnceCell<String>`, `fn machine_block`, front door, job turn, `perform`)
- Modify: `runtime/core/src/prompt.rs:18-19` (system text install rules)

**Interfaces:**
- Consumes: `machine::system_line()`, `machine::current_block(&Connection, &str)`, `machine::record(&Connection, &[String])`, `machine::installed(&Connection)`

- [ ] **Step 1: Failing tests (engine.rs tests module)**

```rust
    #[test]
    fn every_prompt_starts_with_the_machine() {
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "Hello!".into(), remember: None }], "machine-block");
        e.handle("hi").unwrap();
        assert!(e.model.prompts.borrow()[0].user.starts_with("This machine: "), "{}", e.model.prompts.borrow()[0].user);
    }

    #[test]
    fn what_the_ai_installs_is_recorded() {
        let root = Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "cowsay".into()] };
        let (mut e, log_path) = engine_with_log(vec![housekeep(), plan(), act(1, root), done(run("true"))], "records-install");
        e.handle_events("install cowsay").unwrap();
        assert_eq!(crate::machine::installed(e.store.conn()).unwrap(), vec![("cowsay".to_string(), "apt".to_string())]);
        let _ = std::fs::remove_file(&log_path);
    }
```

- [ ] **Step 2: Implement**

In `struct Engine` add `system: std::cell::OnceCell<String>,` and in `Engine::new` add `system: std::cell::OnceCell::new(),`. Add:

```rust
    /// What this machine has, ahead of every prompt (machine-map spec §2). The system line is read
    /// once; programs and installs are scanned each time, so a new install shows on the next turn.
    fn machine_block(&self) -> String {
        crate::machine::current_block(self.store.conn(), self.system.get_or_init(crate::machine::system_line))
    }
```

Front door, right after `let mut p = prompt::front_door(…);`: `p.user = format!("{}\n\n{}", self.machine_block(), p.user);`
Job turn, right after `let mut p = prompt::job_turn(…);`: the same line.
In `perform`, inside `if outcome.ok {`, first line:

```rust
            // Installs the scan cannot see (a tool with no window) are recorded as they happen.
            if let Action::RunCommand { argv } = &action {
                if let Err(e) = crate::machine::record(self.store.conn(), argv) { eprintln!("engine: install not recorded ({e})"); }
            }
```

In `prompt.rs` SYSTEM, replace the "Never install what is already there…" line with:

```
- Never install what is already there: *This machine* lists what is installed; use it. For anything it does not list, `run_command <program> --help` answers in a second; an install takes minutes.
```

and append to the "Install, remove and set up software…" line:

```
 Install a program so it can be found again: `sudo apt-get install -y`, else `sudo snap install`, else `flatpak install -y flathub`. Never leave a loose download (an AppImage, an unpacked archive): put it under /opt/<name> and write its launcher to /usr/local/share/applications/<name>.desktop.
```

- [ ] **Step 3: CI** — push, `gh workflow run build.yml --ref machine-map`, watch. Expected: the two new tests and all existing ones pass (front door tests check `ends_with`/`contains`, which the prefix keeps).

- [ ] **Step 4: Commit** — `git commit -am "engine: every prompt carries the machine; installs are recorded"`

### Task 3: Skills window and Help guide

**Files:**
- Modify: `runtime/core/src/service.rs:78-90` (`skills_event`)
- Modify: `runtime/rail/src/main.rs:476-489` (`show_skills`)
- Modify: `runtime/rail/src/guide.txt`
- Test: `runtime/core/tests/service.rs` (the existing skills test)

- [ ] **Step 1: Failing test** — in `core/tests/service.rs`, where the test first reads `Event::Skills { notebooks }` after a `skills` request, assert:

```rust
    assert_eq!(notebooks[0].name, "installed on this computer", "{notebooks:?}");
    assert!(notebooks[0].entries.iter().all(|n| n.kind == "program"));
```

and change the later `assert!(notebooks.is_empty(), …)` after the forget to `assert!(notebooks.iter().all(|n| n.name == "installed on this computer"), "{notebooks:?}");`.

- [ ] **Step 2: Implement** — in `skills_event`'s `read`, after `crate::notes::init(&c)?;` add `crate::machine::init(&c)?;`, and build:

```rust
        let program = |topic: String, text: String| aios_proto::NoteView { topic, kind: "program".into(), text, uses: 0, failed: false, needs_check: false };
        let mut entries: Vec<_> = crate::machine::apps_in(&executor::atspi::APP_DIRS).into_iter().map(|a| program(a.name, format!("open_app {}", a.id))).collect();
        entries.extend(crate::machine::installed(&c)?.into_iter().map(|(p, via)| program(p, format!("installed by the AI with {via}"))));
        let mut all = vec![aios_proto::Notebook { name: "installed on this computer".into(), entries }];
        all.extend(/* the existing notebooks map */);
        Ok(all)
```

In `show_skills`: only add the ✕ and the "used N times" line when `n.kind != "program"` (a program row is `"{topic}: {text}"`); title `"installed on this computer"` → `"Installed on this computer"`; the empty-state label shows when every notebook but that one is empty. Expand it by default only when it is the only notebook, as now.

Help guide (`guide.txt`), after the paragraph that starts "The AI remembers only the last few messages":

```
<b>What the AI knows about this computer.</b> Before every answer it looks at what is installed — the programs you can open, the usual tools, and whatever it installed itself — so it uses them rather than installing them again. When it does install something, it installs it the standard way so it shows up next time. The list is at the top of <b>Skills</b>, under <b>Installed on this computer</b>.
```

- [ ] **Step 3: CI**, then **Step 4: Commit** — `git commit -am "skills: installed on this computer; guide"`

### Task 4: Release v0.9.0

- [ ] Append a `2026-09-23` entry to `docs/superpowers/specs/2026-09-15-ai-os-design.md` pointing at the machine-map spec, ending `(v0.9.0)`.
- [ ] CI green on the branch → `git checkout master && git merge --no-ff machine-map` → `git tag -a v0.9.0 -m "v0.9.0: the machine map"` → push master and tag → check `https://github.com/gdoumou85/ai-os/releases/download/v0.9.0/ai-os-linux-amd64.tar.gz` answers 200.
