use crate::action::Action;
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The result of actually performing an action.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub detail: String,
    /// A picture for the model's next turn: the screen hand's look (2b).
    pub image: Option<Vec<u8>>,
}

impl Outcome {
    pub fn ok(detail: impl Into<String>) -> Self { Self { ok: true, detail: detail.into(), image: None } }
    pub fn err(detail: impl Into<String>) -> Self { Self { ok: false, detail: detail.into(), image: None } }
}

/// A thing that can perform actions: `MachineWorker` for commands and files, the desktop hand
/// for windows and the screen; tests use `FakeWorker`.
pub trait Worker {
    fn run(&self, action: &Action) -> Outcome;
}

/// Records every action it was asked to run; returns a preset outcome.
pub struct FakeWorker {
    pub calls: RefCell<Vec<Action>>,
    pub outcome: Outcome,
}

impl FakeWorker {
    pub fn new(ok: bool) -> Self {
        Self { calls: RefCell::new(vec![]), outcome: if ok { Outcome::ok("fake") } else { Outcome::err("fake") } }
    }
}

impl Worker for FakeWorker {
    fn run(&self, action: &Action) -> Outcome {
        self.calls.borrow_mut().push(action.clone());
        self.outcome.clone()
    }
}

/// Lets the engine hold a `Executor<Box<dyn Worker>>` without knowing the concrete impl.
impl Worker for Box<dyn Worker> {
    fn run(&self, action: &Action) -> Outcome { (**self).run(action) }
}

/// Runs commands and file actions as the owner, anywhere, with the network on. Root is
/// `sudo -n`, granted without a password at install (full-access spec): the VM is the net.
pub struct MachineWorker {
    /// The job's folder: where a command runs and what a relative path is relative to.
    pub workspace: PathBuf,
}

/// A hung command (a program with a window started from a shell, a prompt nobody answers) must
/// not hold the job forever.
const COMMAND_SECS: &str = "1800";

impl MachineWorker {
    /// An absolute `p` replaces the base, so this is the path the model meant either way.
    fn path(&self, p: &str) -> PathBuf { self.workspace.join(p) }

    fn command(&self, argv: &[String]) -> Outcome {
        // The owner, 2026-09-20: he said "open blender", and the AI — which had installed Blender
        // itself an hour before and kept no note of it — set out to install it again. Nothing
        // tells a model what this machine already has, so the hand answers for it: a check costs
        // a moment, and apt on his VM costs minutes.
        if let Some(pkgs) = packages_to_install(argv) {
            if pkgs.iter().all(|p| installed(p)) {
                return Outcome::ok(format!("nothing to install: {} already installed. Open a program with open_app; run_command is for a command that finishes, like `{} --help`.", pkgs.join(", "), pkgs[0]));
            }
        }
        // The owner, 2026-09-20: told to open Blender, the AI ran the name with run_command.
        // Blender opened — and the command never came back, so the job sat on it in silence with
        // the model idle, all the way to the 30-minute bound. A program with a desktop entry is a
        // program with a window, and windows are `open_app`'s: the hand answers for that here,
        // because a rule in the prompt did not hold.
        if let Some(name) = opens_a_window(argv) {
            if let Some(id) = crate::atspi::desktop_entry(name) {
                return Outcome::err(format!("{name} opens a window, and a command that opens a window never comes back. Open it with open_app {id} (visible if the user is to see it). To use {name} without a window, give it arguments, like `{name} --help`."));
            }
        }
        let out = Command::new("timeout").arg(COMMAND_SECS).args(argv).current_dir(&self.workspace)
            .env("DEBIAN_FRONTEND", "noninteractive").stdin(Stdio::null()).output();
        match out {
            Ok(o) => {
                let ok = o.status.success();
                let (stdout, stderr) = (String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr));
                // Success: the start of the output is what matters. Failure: the END is where the
                // reason lives (1b spec §3), so cut from the tail.
                let cut = |s: &str| if ok { head(s, 500) } else { tail(s, 500) };
                let code = o.status.code().unwrap_or(-1);
                let timed = if code == 124 { " (stopped after 30 min)" } else { "" };
                let detail = format!("exit {code}{timed}; stdout: {} stderr: {}", cut(&stdout), cut(&stderr));
                if ok { Outcome::ok(detail) } else { Outcome::err(detail) }
            }
            Err(e) => Outcome::err(format!("could not start {}: {e}", argv[0])),
        }
    }

    /// A file only root may read is read through `sudo`.
    fn read(&self, p: &Path) -> Result<String, String> {
        match fs::read_to_string(p) {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                let o = Command::new("sudo").args(["-n", "cat", "--"]).arg(p).output().map_err(|e| e.to_string())?;
                if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).into_owned()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            }
            r => r.map_err(|e| e.to_string()),
        }
    }

    /// Makes the parent folders; a file only root may write is written through `sudo tee`.
    fn write(&self, p: &Path, contents: &str) -> Result<(), String> {
        let direct = p.parent().map_or(Ok(()), fs::create_dir_all).and_then(|_| fs::write(p, contents));
        match direct {
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                use std::io::Write;
                if let Some(parent) = p.parent() {
                    let _ = Command::new("sudo").args(["-n", "mkdir", "-p", "--"]).arg(parent).status();
                }
                let mut c = Command::new("sudo").args(["-n", "tee", "--"]).arg(p)
                    .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::piped())
                    .spawn().map_err(|e| e.to_string())?;
                if let Some(mut stdin) = c.stdin.take() { stdin.write_all(contents.as_bytes()).map_err(|e| e.to_string())?; }
                let o = c.wait_with_output().map_err(|e| e.to_string())?;
                if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
            }
            r => r.map_err(|e| e.to_string()),
        }
    }
}

impl Worker for MachineWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => self.command(argv),
            Action::ReadFile { path, from_line, lines } => match self.read(&self.path(path)) {
                Ok(t) => Outcome::ok(window(&t, *from_line, *lines)),
                Err(e) => Outcome::err(e),
            },
            // What was made, in so many words: "written" let a 9B take a file it wrote where a
            // folder should be for a listed and emptied folder (the owner's run, 2026-09-23).
            Action::WriteFile { path, contents } => match self.write(&self.path(path), contents) {
                Ok(()) => Outcome::ok(format!("wrote a file of {} bytes at {}", contents.len(), self.path(path).display())),
                Err(e) => Outcome::err(e),
            },
            // Rule 9: edit in place — the model never reads a whole file, holds it, and writes
            // it all back. It quotes an exact passage from a prior read_file and we swap it in.
            Action::EditFile { path, find, replace } => {
                let p = self.path(path);
                let text = match self.read(&p) { Ok(t) => t, Err(e) => return Outcome::err(e) };
                match text.matches(find.as_str()).count() {
                    0 => return Outcome::err("find text not found — re-read the file and quote it exactly"),
                    1 => {}
                    n => return Outcome::err(format!("find text occurs in {n} places — include more surrounding lines so it is unique")),
                }
                match self.write(&p, &text.replacen(find.as_str(), replace, 1)) {
                    Ok(()) => Outcome::ok("edited"),
                    Err(e) => Outcome::err(e),
                }
            }
            _ => Outcome::err("commands and files have no hand for this action"),
        }
    }
}

/// The packages a plain `apt-get install` would install, or `None` when the command is anything
/// else — a reinstall, a fix, a local .deb, a pinned version, a shell line. Only the plain form is
/// answered from the machine's own records; everything else runs as written.
pub(crate) fn packages_to_install(argv: &[String]) -> Option<Vec<String>> {
    let mut it = argv.iter().map(String::as_str);
    let mut head = it.next()?;
    if head == "sudo" { head = it.next()?; }
    if !matches!(head, "apt" | "apt-get") || it.next()? != "install" { return None; }
    let mut pkgs = vec![];
    for a in it {
        if a.starts_with('-') {
            if !matches!(a, "-y" | "--yes" | "-q" | "-qq" | "--quiet" | "--no-install-recommends") { return None; }
        } else if a.contains('/') || a.contains('=') || a.ends_with(".deb") {
            return None;
        } else {
            pkgs.push(a.to_string());
        }
    }
    (!pkgs.is_empty()).then_some(pkgs)
}

/// The program an argv just starts: a plain name with no options after it (`blender`,
/// `blender scene.blend`). An option means a command-line use (`blender --background x.py`),
/// which finishes on its own and runs as written.
pub(crate) fn opens_a_window(argv: &[String]) -> Option<&str> {
    let (first, rest) = argv.split_first()?;
    (crate::action::valid_app_name(first) && !rest.iter().any(|a| a.starts_with('-'))).then_some(first.as_str())
}

/// Whether dpkg holds this package as installed. Anything it cannot answer is "not installed",
/// so the command runs as written.
pub fn installed(pkg: &str) -> bool {
    Command::new("dpkg-query").args(["-W", "-f=${db:Status-Status}", pkg]).stdin(Stdio::null()).output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "installed")
        .unwrap_or(false)
}

/// Last `n` chars of `s` — for failures the reason is at the end of the output, not the start.
pub(crate) fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}
pub(crate) fn head(s: &str, n: usize) -> String { s.chars().take(n).collect() }

/// What a `read_file` gives back: the whole file (capped), or the asked-for window with
/// numbered lines so the model can quote exact passages back in `edit_file`.
pub(crate) fn window(text: &str, from_line: Option<usize>, lines: Option<usize>) -> String {
    match (from_line, lines) {
        (None, None) => head(text, 2000),
        _ => {
            let start = from_line.unwrap_or(1).max(1);
            let n = lines.unwrap_or(200).min(200);
            text.lines().enumerate()
                .skip(start - 1).take(n)
                .map(|(i, l)| format!("{}: {l}\n", i + 1))
                .collect()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The owner's hang, 2026-09-20: `run_command blender` opened Blender and never returned,
    /// so the job held for half an hour with nothing on screen but a spinner.
    #[test]
    fn a_bare_program_name_starts_a_window_but_a_command_line_use_does_not() {
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        assert_eq!(opens_a_window(&argv("blender")), Some("blender"));
        assert_eq!(opens_a_window(&argv("blender scene.blend")), Some("blender"), "a file to open is still opening the program");
        assert_eq!(opens_a_window(&argv("blender --background x.py")), None, "a command line use finishes on its own");
        assert_eq!(opens_a_window(&argv("ls -la")), None);
        assert_eq!(opens_a_window(&argv("./run")), None, "a path is not a desktop name");
        assert_eq!(opens_a_window(&[]), None);
        // The entry is the gate: a name with no desktop file runs as written.
        assert!(crate::atspi::desktop_entry("no-such-program-anywhere").is_none());
    }

    #[test]
    fn tail_cuts_from_the_end_and_never_splits_a_char() {
        assert_eq!(tail("hi", 500), "hi");
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("héllo", 3), "llo");
        assert_eq!(tail("😀🎉✨", 2), "🎉✨");
    }

    #[test]
    fn head_cuts_from_the_start_and_never_splits_a_char() {
        assert_eq!(head("hi", 500), "hi");
        assert_eq!(head("abcdef", 3), "abc");
        assert_eq!(head("héllo", 3), "hél");
        assert_eq!(head("😀🎉✨", 2), "😀🎉");
    }

    #[test]
    fn fake_records_calls_and_a_boxed_worker_delegates() {
        let w = FakeWorker::new(true);
        assert!(w.run(&Action::ReadFile { path: "x".into(), from_line: None, lines: None }).ok);
        assert_eq!(w.calls.borrow().len(), 1);
        let b: Box<dyn Worker> = Box::new(FakeWorker::new(true));
        assert!(b.run(&Action::ReadFile { path: "x".into(), from_line: None, lines: None }).ok);
    }

    fn temp_ws(tag: &str) -> PathBuf {
        let ws = std::env::temp_dir().join(format!("ai-os-exec-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&ws);
        fs::create_dir_all(&ws).unwrap();
        ws
    }

    #[test]
    fn files_anywhere_relative_to_the_job_folder_or_absolute() {
        let ws = temp_ws("files");
        let w = MachineWorker { workspace: ws.clone() };
        assert!(w.run(&Action::WriteFile { path: "a/b.txt".into(), contents: "x = 1\n".into() }).ok, "parent folders are made");
        let abs = ws.join("abs.txt").display().to_string();
        assert!(w.run(&Action::WriteFile { path: abs.clone(), contents: "hi".into() }).ok);
        assert_eq!(w.run(&Action::ReadFile { path: abs, from_line: None, lines: None }).detail, "hi");
        assert!(w.run(&Action::EditFile { path: "a/b.txt".into(), find: "x = 1".into(), replace: "x = 2".into() }).ok);
        assert_eq!(fs::read_to_string(ws.join("a/b.txt")).unwrap(), "x = 2\n");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn a_command_runs_in_the_job_folder_and_its_failure_keeps_the_tail() {
        let ws = temp_ws("cmd");
        let w = MachineWorker { workspace: ws.clone() };
        let o = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "pwd".into()] });
        assert!(o.ok && o.detail.contains(ws.to_str().unwrap()), "{o:?}");
        let f = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "echo boom >&2; exit 3".into()] });
        assert!(!f.ok && f.detail.contains("exit 3") && f.detail.contains("boom"), "{f:?}");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn edit_file_refuses_zero_and_many_matches() {
        let ws = temp_ws("edit2");
        fs::write(ws.join("a.py"), "x = 1\nx = 1\n").unwrap();
        let w = MachineWorker { workspace: ws.clone() };
        let none = w.run(&Action::EditFile { path: "a.py".into(), find: "z".into(), replace: "q".into() });
        assert!(!none.ok && none.detail.contains("not found"), "{}", none.detail);
        let many = w.run(&Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "q".into() });
        assert!(!many.ok && many.detail.contains("2 places"), "{}", many.detail);
        assert_eq!(fs::read_to_string(ws.join("a.py")).unwrap(), "x = 1\nx = 1\n", "file untouched");
        let _ = fs::remove_dir_all(&ws);
    }

    #[test]
    fn only_a_plain_install_is_answered_from_what_is_already_there() {
        let argv = |s: &str| s.split(' ').map(str::to_string).collect::<Vec<_>>();
        assert_eq!(packages_to_install(&argv("sudo apt-get install -y blender")), Some(vec!["blender".to_string()]));
        assert_eq!(packages_to_install(&argv("apt install blender cowsay")), Some(vec!["blender".to_string(), "cowsay".to_string()]));
        for other in ["sudo apt-get install --reinstall blender", "sudo apt-get remove -y blender",
                      "sudo apt-get install ./local.deb", "sudo apt-get install blender=1.2",
                      "pip install requests", "sh -c apt-get install blender", "sudo apt-get install -y"] {
            assert_eq!(packages_to_install(&argv(other)), None, "{other}");
        }
    }

    #[test]
    fn dpkg_says_what_is_installed() {
        assert!(installed("dpkg"), "dpkg itself is installed wherever dpkg-query answers");
        assert!(!installed("no-such-package-anywhere-xyz"));
    }

    #[test]
    fn read_file_window_returns_only_those_lines() {
        let ws = temp_ws("read1");
        fs::write(ws.join("a.txt"), "l1\nl2\nl3\nl4\n").unwrap();
        let w = MachineWorker { workspace: ws.clone() };
        let out = w.run(&Action::ReadFile { path: "a.txt".into(), from_line: Some(2), lines: Some(2) });
        assert!(out.ok);
        assert_eq!(out.detail, "2: l2\n3: l3\n");
        let _ = fs::remove_dir_all(&ws);
    }
}
