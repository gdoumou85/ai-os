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

/// Runs commands as an unprivileged user, scoped to one workspace, no network.
/// ponytail: shells out to `systemd-run`; a native cgroup/namespace impl only if this proves too slow.
///
/// Path handling for ReadFile/WriteFile: `rules::classify` gates both the same way — outside the
/// workspace needs approval from `Executor::execute` (the only door, see executor.rs) before the
/// worker is ever called. On top of that, this impl re-checks with `resolves_inside` itself
/// (defense-in-depth): even a caller that passes `approved: true` for an outside-workspace read/write
/// gets refused here rather than trusted blindly — the sandbox never touches a path outside its
/// own workspace, full stop.
pub struct SandboxWorker {
    pub user: String,
    pub workspace: PathBuf,
}

impl Worker for SandboxWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::RunCommand { argv } if !argv.is_empty() => {
                // Transient scope: unprivileged user, locked cwd, network cut off.
                // System `systemd-run` (not `--user`): the workshop's `systemd --user` manager
                // rejects `--uid=` and `PrivateNetwork=` (it runs as one uid and can't grant
                // either), so this goes through the system manager via passwordless sudo instead.
                let out = Command::new("sudo")
                    .args(["-n", "systemd-run", "--quiet", "--pipe", "--wait", "--collect"])
                    .arg(format!("--uid={}", self.user))
                    .arg(format!("--working-directory={}", self.workspace.display()))
                    // No network; personal files hidden. System dirs are already unwritable
                    // to the unprivileged sandbox user, and the workspace stays writable
                    // (do NOT use ProtectSystem=strict — it would make the workspace read-only).
                    .args(["--property=PrivateNetwork=yes", "--property=ProtectHome=yes"])
                    .arg("--")
                    .args(argv)
                    .output();
                match out {
                    Ok(o) => Outcome {
                        ok: o.status.success(),
                        detail: format!(
                            "exit {}; stdout: {} stderr: {}",
                            o.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&o.stdout).chars().take(500).collect::<String>(),
                            String::from_utf8_lossy(&o.stderr).chars().take(500).collect::<String>()
                        ),
                    },
                    Err(e) => Outcome { ok: false, detail: format!("spawn failed: {e}") },
                }
            }
            Action::ReadFile { path } => {
                if !resolves_inside(path, &self.workspace) {
                    return Outcome { ok: false, detail: "path escapes workspace".into() };
                }
                match fs::read_to_string(self.workspace.join(path)) {
                    Ok(s) => Outcome { ok: true, detail: s.chars().take(500).collect() },
                    Err(e) => Outcome { ok: false, detail: e.to_string() },
                }
            }
            Action::WriteFile { path, contents } => {
                if !resolves_inside(path, &self.workspace) {
                    return Outcome { ok: false, detail: "path escapes workspace".into() };
                }
                match fs::write(self.workspace.join(path), contents) {
                    Ok(_) => Outcome { ok: true, detail: "written".into() },
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
    fn fake_records_calls() {
        let w = FakeWorker::new(true);
        let a = Action::ReadFile { path: "x".into() };
        let out = w.run(&a);
        assert!(out.ok);
        assert_eq!(w.calls.borrow().len(), 1);
    }

    fn sandbox() -> SandboxWorker {
        SandboxWorker { user: "ai-sandbox".into(), workspace: PathBuf::from("/data/jobs/j1") }
    }

    #[test]
    fn read_file_escaping_workspace_is_refused() {
        let out = sandbox().run(&Action::ReadFile { path: "../../etc/passwd".into() });
        assert!(!out.ok);
        assert_eq!(out.detail, "path escapes workspace");
    }

    #[test]
    fn write_file_escaping_workspace_is_refused() {
        let out = sandbox().run(&Action::WriteFile { path: "/etc/passwd".into(), contents: "x".into() });
        assert!(!out.ok);
        assert_eq!(out.detail, "path escapes workspace");
    }
}
