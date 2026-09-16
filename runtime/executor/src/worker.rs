use crate::action::{Action, Manager};
use crate::admin;
use crate::rules::{resolves_inside, under_any, AI_ROOTS};
use crate::undo::UndoEntry;
use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// The result of actually performing an action, plus what it takes to put it back.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub detail: String,
    /// `Some` only when the action actually changed something reversible.
    pub undo: Option<UndoEntry>,
}

impl Outcome {
    pub fn ok(detail: impl Into<String>) -> Self { Self { ok: true, detail: detail.into(), undo: None } }
    pub fn err(detail: impl Into<String>) -> Self { Self { ok: false, detail: detail.into(), undo: None } }
    pub fn with_undo(mut self, entry: UndoEntry) -> Self {
        self.undo = Some(entry);
        self
    }
}

/// A thing that can perform actions. The spine ships two real impls (SandboxWorker,
/// AdminWorker); tests use FakeWorker.
pub trait Worker {
    fn run(&self, action: &Action) -> Outcome;
    /// Put back what an earlier `run` recorded. Only workers with a privileged hand can.
    fn reverse(&self, _entry: &UndoEntry) -> Outcome { Outcome::err("this worker cannot undo") }
}

/// Records every action it was asked to run; returns a preset outcome.
pub struct FakeWorker {
    pub calls: RefCell<Vec<Action>>,
    pub reversed: RefCell<Vec<UndoEntry>>,
    pub outcome: Outcome,
}

impl FakeWorker {
    pub fn new(ok: bool) -> Self {
        Self {
            calls: RefCell::new(vec![]),
            reversed: RefCell::new(vec![]),
            outcome: if ok { Outcome::ok("fake") } else { Outcome::err("fake") },
        }
    }
}

impl Worker for FakeWorker {
    fn run(&self, action: &Action) -> Outcome {
        self.calls.borrow_mut().push(action.clone());
        self.outcome.clone()
    }
    fn reverse(&self, entry: &UndoEntry) -> Outcome {
        self.reversed.borrow_mut().push(entry.clone());
        self.outcome.clone()
    }
}

/// Lets the engine hold a `Executor<Box<dyn Worker>>` without knowing the concrete impl.
impl Worker for Box<dyn Worker> {
    fn run(&self, action: &Action) -> Outcome { (**self).run(action) }
    fn reverse(&self, entry: &UndoEntry) -> Outcome { (**self).reverse(entry) }
}

/// Runs commands as an unprivileged user, scoped to one workspace, no network.
///
/// Since Task 1, user `ai` may sudo exactly one program, so every sandboxed command goes
/// through the root wrapper's `sandbox-run` verb (`runtime/admin/ai-os-admin`). The wrapper
/// pins `--uid=ai-sandbox` and builds the whole jail property set itself — this side only
/// says which network the command may reach, which folder it runs in, and what to run.
/// ponytail: shells out to the wrapper, which shells out to `systemd-run`; a native
/// cgroup/namespace impl only if this proves too slow.
///
/// Path handling for ReadFile/WriteFile: `rules::classify` gates both the same way — outside the
/// workspace needs approval from `Executor::execute` (the only door, see executor.rs) before the
/// worker is ever called. On top of that, this impl re-checks with `resolves_inside` itself
/// (defense-in-depth): even a caller that passes `approved: true` for an outside-workspace read/write
/// gets refused here rather than trusted blindly.
///
/// `resolves_inside` is a cheap early reject, but it's purely lexical — it can't see that an
/// in-workspace path component is a symlink pointing outside (e.g. a sandboxed `RunCommand` plants
/// `ln -s /etc/shadow leak`, then `ReadFile{path:"leak"}` is lexically inside but would follow the
/// link). The real guard runs right before each fs op: `ReadFile` fully canonicalizes the target
/// (it must already exist, so this resolves every symlink in the path, leaf included) and checks
/// it against the canonicalized workspace. `WriteFile` can't canonicalize a target that may not
/// exist yet, so it canonicalizes the *parent* instead (catching a symlinked parent dir) and then,
/// separately, refuses outright if the leaf itself already exists as a symlink — otherwise
/// `fs::write` would still follow it out of the workspace even with a checked parent. With both,
/// the sandbox never touches a path outside its own workspace, full stop.
pub struct SandboxWorker {
    /// Always `ai-sandbox`. Documentation only: the wrapper pins the uid, so nothing this
    /// side puts here can change who the command runs as.
    pub user: String,
    pub workspace: PathBuf,
}

/// Where an `/etc` read is allowed to land once every symlink is resolved. `/etc` is full of
/// links out of itself — `/etc/os-release` → `/usr/lib/os-release`, `/etc/localtime` →
/// `/usr/share/zoneinfo/…`, `/etc/mtab` → `/proc/self/mounts`, and the whole alternatives
/// system — so pinning the target to `/etc` alone would refuse most of what makes `/etc`
/// worth reading. All four are world-readable system state; permissions still decide.
const SYSTEM_READ_ROOTS: [&str; 4] = ["/etc", "/usr", "/run", "/proc"];

impl SandboxWorker {
    /// Canonicalize the workspace itself once per call, as an `Outcome`-shaped error so call
    /// sites can just `?`-style propagate it with `match ... { Err(out) => return out }`.
    fn canonical_workspace(&self) -> Result<PathBuf, Outcome> {
        fs::canonicalize(&self.workspace)
            .map_err(|e| Outcome::err(format!("cannot resolve workspace: {e}")))
    }

    /// Resolve an existing in-workspace file to its canonical path, refusing anything that
    /// escapes (lexically or through a symlink). Same guard as ReadFile had in 1a; shared by
    /// ReadFile and EditFile since both only ever operate on a file that must already exist.
    ///
    /// `etc_ok` opens one extra door, and only for reads: `/etc`. System config is
    /// world-readable already and `rules::classify` rates reading it Auto, so refusing it here
    /// would only leave the AI unable to see the machine it runs on. The read runs in-process
    /// as the executor's own user (`ai`), so the file's own permissions are the real gate —
    /// `/etc/fstab` comes back, `/etc/shadow` does not. `EditFile` passes `false` and stays
    /// workspace-only: nothing may rewrite system config behind the admin wrapper's back (the
    /// wrapper has its own protected-file list for the writes it does allow).
    ///
    /// The door is keyed on the *asked-for* path, not just on where it lands, and the
    /// canonical target still has to stay in the roots that door opened. So `/etc/fstab` reads,
    /// a workspace file that is secretly a symlink into `/etc` does NOT (the request was for a
    /// workspace path, so only the workspace is open to it), and an `/etc` path that resolves
    /// outside the system roots does not either. Both halves have to agree or it is refused.
    fn existing_inside(&self, path: &str, etc_ok: bool) -> Result<PathBuf, Outcome> {
        // `join` on an absolute path just yields that path, so this needs no workspace for /etc.
        let canon = || fs::canonicalize(self.workspace.join(path)).map_err(|_| Outcome::err("cannot resolve path"));
        // An `/etc` read must not need the workspace to exist — it is system state, nothing to
        // do with any project — so that branch never canonicalizes the workspace at all.
        if etc_ok && under_any(path, &["/etc"]) {
            let t = canon()?;
            return if under_any(&t.to_string_lossy(), &SYSTEM_READ_ROOTS) {
                Ok(t)
            } else {
                Err(Outcome::err("path escapes workspace"))
            };
        }
        if !resolves_inside(path, &self.workspace) {
            return Err(Outcome::err("path escapes workspace"));
        }
        let ws_canon = self.canonical_workspace()?;
        let t = canon()?;
        if !t.starts_with(&ws_canon) {
            return Err(Outcome::err("path escapes workspace"));
        }
        Ok(t)
    }

    /// Run `argv` in the jail through the wrapper. `net` is `none` (PrivateNetwork) or a
    /// comma-separated address allowlist; `envs` are plain `K=V`.
    ///
    /// argv goes across RAW. systemd expands `${VAR}` in a unit's argv, and the wrapper
    /// doubles every `$` itself right before it execs systemd-run — escaping here as well
    /// would double it twice and leave `$$` in the command's own output.
    pub(crate) fn run_in_sandbox(&self, net: &str, envs: &[String], argv: &[String]) -> Outcome {
        let args = sandbox_args(&self.workspace.display().to_string(), net, envs, argv);
        // The wrapper `exec`s systemd-run, so this status/stdout/stderr is the command's own.
        // A wrapper refusal is exit 3 with a `refused: …` line on stderr, which reads the same.
        match Command::new(admin::SUDO).args(["-n", admin::WRAPPER]).args(&args).output() {
            Ok(o) => {
                let ok = o.status.success();
                let stdout = String::from_utf8_lossy(&o.stdout);
                let stderr = String::from_utf8_lossy(&o.stderr);
                // Success: the start of the output is what matters. Failure: the END is where the
                // reason lives (1b spec §3), so cut from the tail.
                let cut = |s: &str| if ok { head(s, 500) } else { tail(s, 500) };
                let detail = format!("exit {}; stdout: {} stderr: {}", o.status.code().unwrap_or(-1), cut(&stdout), cut(&stderr));
                if ok { Outcome::ok(detail) } else { Outcome::err(detail) }
            }
            Err(e) => Outcome::err(format!("spawn failed: {e}")),
        }
    }

    /// Fetch language packages into the workspace: the same jail as any other command, plus a
    /// network allowlist holding nothing but the resolver and the manager's own registry.
    /// Steps run in order and stop at the first failure; the detail is that step's output.
    fn fetch(&self, manager: Manager, packages: &[String]) -> Outcome {
        if packages.is_empty() {
            return Outcome::err("no packages named");
        }
        let hosts = admin::resolve_all(admin::registry_hosts(manager));
        if hosts.is_empty() {
            return Outcome::err("could not resolve the package registry");
        }
        let net = admin::resolver_ips().into_iter().chain(hosts).collect::<Vec<_>>().join(",");
        let ws = self.workspace.display().to_string();
        // HOME: pip/npm/cargo all want caches and config under it, and the jail hides the real
        // one (ProtectHome). CARGO_HOME likewise, so `cargo add`/`fetch` land inside the project.
        // The three config vars point the tools at nothing: HOME is the workspace, so a config
        // file an earlier sandboxed command planted there could otherwise re-point this fixed
        // argv at another host that happens to share the allowlisted CDN addresses.
        // Accepted ceilings: during a fetch the resolver address is open on every port, and
        // cargo still reads `.cargo/config.toml` under the workspace (CARGO_HOME lives there
        // by design, so a planted registry override is possible for cargo alone).
        let envs = [
            format!("HOME={ws}"),
            format!("CARGO_HOME={ws}/.cargo"),
            "PIP_CONFIG_FILE=/dev/null".to_string(),
            "NPM_CONFIG_USERCONFIG=/dev/null".to_string(),
            "NPM_CONFIG_GLOBALCONFIG=/dev/null".to_string(),
        ];
        let mut done: Option<Outcome> = None;
        for mut argv in admin::fetch_argv(manager, packages) {
            absolute_program(&ws, &mut argv);
            let step = self.run_in_sandbox(&net, &envs, &argv);
            if !step.ok {
                return step;
            }
            done = Some(step);
        }
        // `fetch_argv` yields at least one step for every manager, and `packages` is non-empty.
        done.expect("fetch_argv yields at least one step")
    }
}

/// What a failed command was really asking about. The jail replaces `/data` and `/mnt` with
/// empty mounts, so `ls /data/work` answers "No such file or directory" about a folder that is
/// really there — the live 1c run watched the model read that as proof its own `make_dir` had
/// failed, and give up on a job it had already done. Never let the sandbox's blindness be
/// reported as the machine's truth.
///
/// Only paths under `roots` (the AI-writable roots) are probed: that is where the folders it
/// makes live, and it keeps this from stat-ing whatever else an argument list happens to hold.
/// A path inside the workspace is visible to the sandbox and needs no note; one that does not
/// exist gets none either — the failure was the truth.
pub(crate) fn hidden_note(argv: &[String], workspace: &Path, roots: &[&str]) -> Option<String> {
    let hidden: Vec<&str> = argv.iter().skip(1)
        .filter(|a| under_any(a, roots) && !resolves_inside(a, workspace) && Path::new(a).exists())
        .map(String::as_str)
        .collect();
    if hidden.is_empty() {
        return None;
    }
    Some(format!(
        "note: {} exists — the sandbox cannot see outside its working directory, so this command cannot check it. Use a hand that can.",
        hidden.join(", "),
    ))
}

/// systemd resolves a unit's program against `/`, not `--working-directory`, so a
/// workspace-relative program (`.venv/bin/pip`, `./run.sh`) has to be made absolute before it
/// goes out — the live 1c run watched the model try `.venv/bin/python3` and be told there is no
/// such file. A bare name (`python3`, `npm`, `cargo`) is left alone and looked up on PATH, which
/// is exactly how a shell treats a name with a slash in it against one without.
pub(crate) fn absolute_program(workspace: &str, argv: &mut [String]) {
    if let Some(p) = argv.first_mut() {
        if p.contains('/') && !p.starts_with('/') {
            *p = format!("{workspace}/{}", p.trim_start_matches("./"));
        }
    }
}

/// The wrapper argv for one sandboxed run, from the verb on. Pure, so the shape can be checked
/// without a machine: every option comes before the `--`, and everything after it is the
/// command exactly as given — a package or filename that reads like an option (`--net=…`) lands
/// on the far side of the `--` and can never be taken for one.
pub(crate) fn sandbox_args(cwd: &str, net: &str, envs: &[String], argv: &[String]) -> Vec<String> {
    let mut args = vec!["sandbox-run".to_string(), format!("--net={net}"), format!("--cwd={cwd}")];
    args.extend(envs.iter().map(|e| format!("--env={e}")));
    args.push("--".to_string());
    args.extend(argv.iter().cloned());
    args
}

/// Last `n` chars of `s` — for failures the reason is at the end of the output, not the start.
/// `pub(crate)`: exercised directly by unit tests below (matches `rules::resolves_inside`'s
/// convention for a helper that's more than a private implementation detail).
pub(crate) fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}
pub(crate) fn head(s: &str, n: usize) -> String { s.chars().take(n).collect() }

/// What a `read_file` gives back: the whole file (capped), or the asked-for window with
/// numbered lines so the model can quote exact passages back in `edit_file`. Shared by
/// `SandboxWorker` and `AdminWorker` — a file reads the same whichever hand fetched it.
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

impl Worker for SandboxWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => {
                let mut argv = argv.clone();
                absolute_program(&self.workspace.display().to_string(), &mut argv);
                let out = self.run_in_sandbox("none", &[], &argv);
                if out.ok { return out; }
                match hidden_note(&argv, &self.workspace, &AI_ROOTS) {
                    Some(note) => Outcome::err(format!("{} ({note})", out.detail)),
                    None => out,
                }
            }
            Action::FetchPackages { manager, packages } => self.fetch(*manager, packages),
            // ponytail: TOCTOU window between `existing_inside`'s canonicalize and the read below
            // — a swap of the (now-plain) target back into a symlink in between would slip
            // through. Acceptable for now since the interface runs one action at a time; revisit
            // if concurrent access to the same workspace is ever added.
            Action::ReadFile { path, from_line, lines } => {
                let target = match self.existing_inside(path, true) { Ok(p) => p, Err(out) => return out };
                let text = match fs::read_to_string(&target) {
                    Ok(s) => s,
                    Err(e) => return Outcome::err(e.to_string()),
                };
                Outcome::ok(window(&text, *from_line, *lines))
            }
            Action::WriteFile { path, contents } => {
                if !resolves_inside(path, &self.workspace) {
                    return Outcome::err("path escapes workspace");
                }
                let ws_canon = match self.canonical_workspace() {
                    Ok(p) => p,
                    Err(out) => return out,
                };
                let target = self.workspace.join(path);
                // A file that doesn't exist yet can't be canonicalized, so canonicalize its
                // parent instead (catches a symlinked parent dir) and keep the file's own plain
                // name from the (already lexically-checked) target — file_name() rejects "..",
                // "." and anything that isn't a single normal component.
                let file_name = match target.file_name() {
                    Some(n) => n,
                    None => return Outcome::err("invalid file name"),
                };
                let parent = match target.parent() {
                    Some(p) => p,
                    None => return Outcome::err("invalid file name"),
                };
                let parent_canon = match fs::canonicalize(parent) {
                    Ok(p) => p,
                    Err(_) => return Outcome::err("cannot resolve path"),
                };
                if !parent_canon.starts_with(&ws_canon) {
                    return Outcome::err("path escapes workspace");
                }
                // The parent check alone isn't enough: if the leaf itself already exists as a
                // symlink, `fs::write` opens without O_NOFOLLOW and happily follows it out of the
                // workspace. `symlink_metadata` (unlike `metadata`/`canonicalize`) does not follow
                // the final component, so this sees the link itself rather than what it points to.
                // ponytail: TOCTOU window between this check and the write below — swapping a
                // plain file for a symlink in between would slip through. Acceptable for now since
                // the interface runs one action at a time; revisit if concurrent workspace access
                // is ever added.
                let leaf = parent_canon.join(file_name);
                match fs::symlink_metadata(&leaf) {
                    Ok(meta) if meta.file_type().is_symlink() => {
                        return Outcome::err("refusing to write through a symlink");
                    }
                    Ok(_) => {}                                    // exists, plain file: fine to overwrite
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // doesn't exist yet: fine to create
                    Err(e) => return Outcome::err(e.to_string()),
                }
                match fs::write(&leaf, contents) {
                    Ok(_) => Outcome::ok("written"),
                    Err(e) => Outcome::err(e.to_string()),
                }
            }
            // Rule 9: edit in place — the model never reads a whole file, holds it, and writes
            // it all back. It quotes an exact passage from a prior read_file and we swap it in.
            Action::EditFile { path, find, replace } => {
                let target = match self.existing_inside(path, false) { Ok(p) => p, Err(out) => return out };
                let text = match fs::read_to_string(&target) {
                    Ok(s) => s,
                    Err(e) => return Outcome::err(e.to_string()),
                };
                match text.matches(find.as_str()).count() {
                    0 => return Outcome::err("find text not found — re-read the file and quote it exactly"),
                    1 => {}
                    n => return Outcome::err(format!("find text occurs in {n} places — include more surrounding lines so it is unique")),
                }
                // `target` is canonical (no symlink left in it), so this cannot write outside the workspace.
                match fs::write(&target, text.replacen(find.as_str(), replace, 1)) {
                    Ok(_) => Outcome::ok("edited"),
                    Err(e) => Outcome::err(e.to_string()),
                }
            }
            _ => Outcome::err("sandbox has no hand for this action"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_shorter_than_n_returns_the_whole_string() {
        assert_eq!(tail("hi", 500), "hi");
    }

    #[test]
    fn tail_exactly_n_returns_the_whole_string() {
        assert_eq!(tail("hello", 5), "hello");
    }

    #[test]
    fn tail_cuts_from_the_end_not_the_start() {
        assert_eq!(tail("abcdef", 3), "def");
    }

    #[test]
    fn tail_never_splits_a_multibyte_char() {
        // 5 chars, each multi-byte in UTF-8; a byte-based cut would panic or corrupt.
        assert_eq!(tail("héllo", 3), "llo");
        assert_eq!(tail("😀🎉✨", 2), "🎉✨");
    }

    #[test]
    fn head_shorter_than_n_returns_the_whole_string() {
        assert_eq!(head("hi", 500), "hi");
    }

    #[test]
    fn head_exactly_n_returns_the_whole_string() {
        assert_eq!(head("hello", 5), "hello");
    }

    #[test]
    fn head_cuts_from_the_start_not_the_end() {
        assert_eq!(head("abcdef", 3), "abc");
    }

    #[test]
    fn head_never_splits_a_multibyte_char() {
        assert_eq!(head("héllo", 3), "hél");
        assert_eq!(head("😀🎉✨", 2), "😀🎉");
    }

    #[test]
    fn fake_records_calls() {
        let w = FakeWorker::new(true);
        let a = Action::ReadFile { path: "x".into(), from_line: None, lines: None };
        let out = w.run(&a);
        assert!(out.ok);
        assert_eq!(w.calls.borrow().len(), 1);
    }

    #[test]
    fn fake_records_reversals_and_the_default_worker_cannot_undo() {
        let e = UndoEntry::DirCreated { path: "/data/x".into() };
        let w = FakeWorker::new(true);
        assert!(w.reverse(&e).ok);
        assert_eq!(w.reversed.borrow().as_slice(), &[e.clone()]);
        // SandboxWorker never records undo, so it gets the trait's default refusal.
        assert_eq!(sandbox().reverse(&e).detail, "this worker cannot undo");
    }

    fn sandbox() -> SandboxWorker {
        SandboxWorker { user: "ai-sandbox".into(), workspace: PathBuf::from("/data/jobs/j1") }
    }

    #[test]
    fn read_file_escaping_workspace_is_refused() {
        let out = sandbox().run(&Action::ReadFile { path: "../../etc/passwd".into(), from_line: None, lines: None });
        assert!(!out.ok);
        assert_eq!(out.detail, "path escapes workspace");
    }

    #[test]
    fn write_file_escaping_workspace_is_refused() {
        let out = sandbox().run(&Action::WriteFile { path: "/etc/passwd".into(), contents: "x".into() });
        assert!(!out.ok);
        assert_eq!(out.detail, "path escapes workspace");
    }

    fn temp_ws(tag: &str) -> PathBuf {
        let ws = std::env::temp_dir().join(format!("ai-os-exec-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&ws);
        std::fs::create_dir_all(&ws).unwrap();
        ws
    }

    #[test]
    fn edit_file_replaces_exactly_one_match() {
        let ws = temp_ws("edit1");
        std::fs::write(ws.join("a.py"), "x = 1\ny = 2\n").unwrap();
        let w = SandboxWorker { user: "nobody".into(), workspace: ws.clone() };
        let out = w.run(&Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "x = 10".into() });
        assert!(out.ok, "{}", out.detail);
        assert_eq!(std::fs::read_to_string(ws.join("a.py")).unwrap(), "x = 10\ny = 2\n");
    }

    #[test]
    fn edit_file_refuses_zero_and_many_matches() {
        let ws = temp_ws("edit2");
        std::fs::write(ws.join("a.py"), "x = 1\nx = 1\n").unwrap();
        let w = SandboxWorker { user: "nobody".into(), workspace: ws.clone() };
        let none = w.run(&Action::EditFile { path: "a.py".into(), find: "z".into(), replace: "q".into() });
        assert!(!none.ok);
        assert!(none.detail.contains("not found"), "{}", none.detail);
        let many = w.run(&Action::EditFile { path: "a.py".into(), find: "x = 1".into(), replace: "q".into() });
        assert!(!many.ok);
        assert!(many.detail.contains("2 places"), "{}", many.detail);
        assert_eq!(std::fs::read_to_string(ws.join("a.py")).unwrap(), "x = 1\nx = 1\n", "file untouched");
    }

    #[test]
    fn read_file_window_returns_only_those_lines() {
        let ws = temp_ws("read1");
        std::fs::write(ws.join("a.txt"), "l1\nl2\nl3\nl4\n").unwrap();
        let w = SandboxWorker { user: "nobody".into(), workspace: ws };
        let out = w.run(&Action::ReadFile { path: "a.txt".into(), from_line: Some(2), lines: Some(2) });
        assert!(out.ok);
        assert_eq!(out.detail, "2: l2\n3: l3\n");
    }

    #[test]
    fn only_a_real_path_under_the_ai_roots_and_outside_the_workspace_earns_the_hidden_note() {
        // A temp root stands in for /data: the rule is the same, and the test needs a root it
        // may create in. `roots` is a parameter for exactly this reason.
        let root = std::env::temp_dir().join(format!("ai-os-hidden-{}", std::process::id()));
        let ws = root.join("ws");
        fs::create_dir_all(&ws).unwrap();
        fs::write(ws.join("inside.txt"), "x").unwrap();
        fs::write(root.join("outside.txt"), "x").unwrap();
        let roots = [root.to_str().unwrap()];
        let argv = |p: &Path| vec!["ls".to_string(), p.display().to_string()];

        let note = hidden_note(&argv(&root.join("outside.txt")), &ws, &roots).expect("outside and real");
        assert!(note.contains("outside.txt") && note.contains("cannot see outside"), "{note}");
        assert_eq!(hidden_note(&argv(&ws.join("inside.txt")), &ws, &roots), None, "the sandbox can see its own workspace");
        assert_eq!(hidden_note(&argv(&root.join("never.txt")), &ws, &roots), None, "a missing file's failure was the truth");
        // /etc/hostname exists on every Linux, and is not under the AI roots: no note.
        assert_eq!(hidden_note(&argv(Path::new("/etc/hostname")), &ws, &roots), None, "only the AI roots are probed");
        assert_eq!(hidden_note(&vec!["ls".to_string()], &ws, &roots), None);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_program_with_a_slash_is_resolved_against_the_workspace_a_bare_name_is_not() {
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        let mut a = argv(".venv/bin/python3 t.py");
        absolute_program("/data/p", &mut a);
        assert_eq!(a[0], "/data/p/.venv/bin/python3");
        assert_eq!(a[1], "t.py", "only the program is touched");
        let mut a = argv("./run.sh");
        absolute_program("/data/p", &mut a);
        assert_eq!(a[0], "/data/p/run.sh");
        for bare in ["python3 t.py", "/usr/bin/env python3"] {
            let mut a = argv(bare);
            let before = a.clone();
            absolute_program("/data/p", &mut a);
            assert_eq!(a, before, "{bare}");
        }
    }

    #[test]
    fn sandbox_args_keeps_every_option_before_the_dash_dash() {
        // A package literally named `--net=1.2.3.4`: if it were ever spliced in ahead of the
        // `--`, the wrapper would read it as the allowlist and open the jail to that address.
        let argv: Vec<String> = ["pip", "install", "--", "--net=1.2.3.4"].iter().map(|s| s.to_string()).collect();
        let envs = vec!["HOME=/data/p".to_string(), "PIP_CONFIG_FILE=/dev/null".to_string()];
        assert_eq!(
            sandbox_args("/data/p", "10.0.0.1,1.2.3.4", &envs, &argv),
            vec![
                "sandbox-run",
                "--net=10.0.0.1,1.2.3.4",
                "--cwd=/data/p",
                "--env=HOME=/data/p",
                "--env=PIP_CONFIG_FILE=/dev/null",
                "--",
                "pip",
                "install",
                "--",
                "--net=1.2.3.4",
            ]
        );
        // No envs: the `--` still separates, and argv still comes across untouched.
        assert_eq!(
            sandbox_args("/data/p", "none", &[], &["id".to_string()]),
            vec!["sandbox-run", "--net=none", "--cwd=/data/p", "--", "id"]
        );
    }

    /// The fetch allowlist really is an allowlist, not just "network on": a host that is not on
    /// it stays unreachable even from a run opened up for the registry. Gated like the machine
    /// tests — it needs the distro, the wrapper and a working resolver.
    #[test]
    fn fetch_allowlist_reaches_nothing_else() {
        if std::env::var("AI_OS_SANDBOX_IT").as_deref() != Ok("1") { return; }
        let hosts = crate::admin::resolve_all(crate::admin::registry_hosts(Manager::Pip));
        assert!(!hosts.is_empty(), "no registry addresses to build an allowlist from");
        let net = crate::admin::resolver_ips().into_iter().chain(hosts).collect::<Vec<_>>().join(",");
        let w = SandboxWorker { user: "ai-sandbox".into(), workspace: PathBuf::from("/data/housekeeping") };
        let argv: Vec<String> = ["curl", "-sS", "-m", "8", "https://example.com"].iter().map(|s| s.to_string()).collect();
        let out = w.run_in_sandbox(&net, &[], &argv);
        assert!(!out.ok, "an off-allowlist host must stay unreachable during a fetch: {}", out.detail);
    }

    #[test]
    fn boxed_worker_delegates() {
        let b: Box<dyn Worker> = Box::new(FakeWorker::new(true));
        assert!(b.run(&Action::ReadFile { path: "x".into(), from_line: None, lines: None }).ok);
    }
}
