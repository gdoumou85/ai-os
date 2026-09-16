use crate::action::Action;
use crate::rules::resolves_inside;
use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// The result of actually performing an action.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub ok: bool,
    pub detail: String,
}

/// A thing that can perform actions. The spine ships one real impl (SandboxWorker,
/// Task 7); tests use FakeWorker.
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
        Self { calls: RefCell::new(vec![]), outcome: Outcome { ok, detail: "fake".into() } }
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

/// Runs commands as an unprivileged user, scoped to one workspace, no network.
/// ponytail: shells out to `systemd-run`; a native cgroup/namespace impl only if this proves too slow.
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
    pub user: String,
    pub workspace: PathBuf,
}

impl SandboxWorker {
    /// Canonicalize the workspace itself once per call, as an `Outcome`-shaped error so call
    /// sites can just `?`-style propagate it with `match ... { Err(out) => return out }`.
    fn canonical_workspace(&self) -> Result<PathBuf, Outcome> {
        fs::canonicalize(&self.workspace)
            .map_err(|e| Outcome { ok: false, detail: format!("cannot resolve workspace: {e}") })
    }

    /// Resolve an existing in-workspace file to its canonical path, refusing anything that
    /// escapes (lexically or through a symlink). Same guard as ReadFile had in 1a; shared by
    /// ReadFile and EditFile since both only ever operate on a file that must already exist.
    fn existing_inside(&self, path: &str) -> Result<PathBuf, Outcome> {
        if !resolves_inside(path, &self.workspace) {
            return Err(Outcome { ok: false, detail: "path escapes workspace".into() });
        }
        let ws_canon = self.canonical_workspace()?;
        let target = fs::canonicalize(self.workspace.join(path))
            .map_err(|_| Outcome { ok: false, detail: "cannot resolve path".into() })?;
        if !target.starts_with(&ws_canon) {
            return Err(Outcome { ok: false, detail: "path escapes workspace".into() });
        }
        Ok(target)
    }
}

/// Last `n` chars of `s` — for failures the reason is at the end of the output, not the start.
/// `pub(crate)`: exercised directly by unit tests below (matches `rules::resolves_inside`'s
/// convention for a helper that's more than a private implementation detail).
pub(crate) fn tail(s: &str, n: usize) -> String {
    let count = s.chars().count();
    s.chars().skip(count.saturating_sub(n)).collect()
}
pub(crate) fn head(s: &str, n: usize) -> String { s.chars().take(n).collect() }

impl Worker for SandboxWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => {
                // Transient scope: unprivileged user, locked cwd, network cut off.
                // System `systemd-run` (not `--user`): the workshop's `systemd --user` manager
                // rejects `--uid=` and `PrivateNetwork=` (it runs as one uid and can't grant
                // either), so this goes through the system manager via passwordless sudo instead.
                let ws = self.workspace.display().to_string();
                let out = Command::new("sudo")
                    .args(["-n", "systemd-run", "--quiet", "--pipe", "--wait", "--collect"])
                    .arg(format!("--uid={}", self.user))
                    .arg(format!("--working-directory={ws}"))
                    // The jail (1b spec §7): system programs read-only, the project folder read-write,
                    // nothing else. /data becomes an empty read-only tmpfs with only this project bound
                    // into it, so other projects and the executor's database do not exist from inside.
                    // Home is hidden, /tmp is private, and files the sandbox creates are group-writable
                    // so the executor's own user can still edit them (shared group on the folder).
                    .args(["--property=PrivateNetwork=yes", "--property=ProtectHome=yes", "--property=PrivateTmp=yes"])
                    .args(["--property=ProtectSystem=strict", "--property=UMask=0002"])
                    .arg("--property=TemporaryFileSystem=/data:ro")
                    // ProtectSystem=strict/ProtectHome/TemporaryFileSystem still leave the host's
                    // other mounts readable (on the dev workshop, /mnt is the whole Windows
                    // profile) — spec §7 says "nothing else" is visible. Hide them outright; the
                    // leading `-` means "ignore if this path doesn't exist" (e.g. no /media on
                    // some hosts) rather than failing the whole unit.
                    .arg("--property=InaccessiblePaths=-/mnt -/media -/srv")
                    .arg(format!("--property=BindPaths={ws}"))
                    .arg(format!("--property=ReadWritePaths={ws}"))
                    .arg("--")
                    .args(argv)
                    .output();
                match out {
                    Ok(o) => {
                        let ok = o.status.success();
                        let stdout = String::from_utf8_lossy(&o.stdout);
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        // Success: the start of the output is what matters. Failure: the END is where the
                        // reason lives (1b spec §3), so cut from the tail.
                        let cut = |s: &str| if ok { head(s, 500) } else { tail(s, 500) };
                        Outcome {
                            ok,
                            detail: format!("exit {}; stdout: {} stderr: {}", o.status.code().unwrap_or(-1), cut(&stdout), cut(&stderr)),
                        }
                    }
                    Err(e) => Outcome { ok: false, detail: format!("spawn failed: {e}") },
                }
            }
            // ponytail: TOCTOU window between `existing_inside`'s canonicalize and the read below
            // — a swap of the (now-plain) target back into a symlink in between would slip
            // through. Acceptable for now since the interface runs one action at a time; revisit
            // if concurrent access to the same workspace is ever added.
            Action::ReadFile { path, from_line, lines } => {
                let target = match self.existing_inside(path) { Ok(p) => p, Err(out) => return out };
                let text = match fs::read_to_string(&target) {
                    Ok(s) => s,
                    Err(e) => return Outcome { ok: false, detail: e.to_string() },
                };
                match (from_line, lines) {
                    (None, None) => Outcome { ok: true, detail: head(&text, 2000) },
                    _ => {
                        // Numbered lines so the model can quote exact passages back in edit_file.
                        let start = from_line.unwrap_or(1).max(1);
                        let n = lines.unwrap_or(200).min(200);
                        let detail: String = text.lines().enumerate()
                            .skip(start - 1).take(n)
                            .map(|(i, l)| format!("{}: {l}\n", i + 1))
                            .collect();
                        Outcome { ok: true, detail }
                    }
                }
            }
            Action::WriteFile { path, contents } => {
                if !resolves_inside(path, &self.workspace) {
                    return Outcome { ok: false, detail: "path escapes workspace".into() };
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
                    None => return Outcome { ok: false, detail: "invalid file name".into() },
                };
                let parent = match target.parent() {
                    Some(p) => p,
                    None => return Outcome { ok: false, detail: "invalid file name".into() },
                };
                let parent_canon = match fs::canonicalize(parent) {
                    Ok(p) => p,
                    Err(_) => return Outcome { ok: false, detail: "cannot resolve path".into() },
                };
                if !parent_canon.starts_with(&ws_canon) {
                    return Outcome { ok: false, detail: "path escapes workspace".into() };
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
                        return Outcome { ok: false, detail: "refusing to write through a symlink".into() };
                    }
                    Ok(_) => {}                                    // exists, plain file: fine to overwrite
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {} // doesn't exist yet: fine to create
                    Err(e) => return Outcome { ok: false, detail: e.to_string() },
                }
                match fs::write(&leaf, contents) {
                    Ok(_) => Outcome { ok: true, detail: "written".into() },
                    Err(e) => Outcome { ok: false, detail: e.to_string() },
                }
            }
            // Rule 9: edit in place — the model never reads a whole file, holds it, and writes
            // it all back. It quotes an exact passage from a prior read_file and we swap it in.
            Action::EditFile { path, find, replace } => {
                let target = match self.existing_inside(path) { Ok(p) => p, Err(out) => return out };
                let text = match fs::read_to_string(&target) {
                    Ok(s) => s,
                    Err(e) => return Outcome { ok: false, detail: e.to_string() },
                };
                match text.matches(find.as_str()).count() {
                    0 => return Outcome { ok: false, detail: "find text not found — re-read the file and quote it exactly".into() },
                    1 => {}
                    n => return Outcome { ok: false, detail: format!("find text occurs in {n} places — include more surrounding lines so it is unique") },
                }
                // `target` is canonical (no symlink left in it), so this cannot write outside the workspace.
                match fs::write(&target, text.replacen(find.as_str(), replace, 1)) {
                    Ok(_) => Outcome { ok: true, detail: "edited".into() },
                    Err(e) => Outcome { ok: false, detail: e.to_string() },
                }
            }
            _ => Outcome { ok: false, detail: "sandbox has no hand for this action".into() },
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
    fn boxed_worker_delegates() {
        let b: Box<dyn Worker> = Box::new(FakeWorker::new(true));
        assert!(b.run(&Action::ReadFile { path: "x".into(), from_line: None, lines: None }).ok);
    }
}
