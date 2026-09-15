use crate::action::Action;
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
/// Path handling for ReadFile/WriteFile mirrors what `rules::classify` assumed when it let the
/// action through: `Executor::execute` is the only door (see executor.rs) and it calls the worker
/// only when classify() returned `Auto` or the caller passed `approved: true` for a `NeedsConfirm`.
/// A `..`-escaping WriteFile is classified `NeedsConfirm`, so `self.workspace.join(path)` here only
/// ever runs for such a path once a human (or orchestrator) has approved it — this impl does not
/// need to re-defend against `..` itself. ReadFile is unconditionally `Auto` in classify() today
/// (no path check at all), which is a pre-existing gap in rules.rs, not introduced here.
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
                            "exit {}; {}",
                            o.status.code().unwrap_or(-1),
                            String::from_utf8_lossy(&o.stdout).chars().take(500).collect::<String>()
                        ),
                    },
                    Err(e) => Outcome { ok: false, detail: format!("spawn failed: {e}") },
                }
            }
            Action::ReadFile { path } => match fs::read_to_string(self.workspace.join(path)) {
                Ok(s) => Outcome { ok: true, detail: s.chars().take(500).collect() },
                Err(e) => Outcome { ok: false, detail: e.to_string() },
            },
            Action::WriteFile { path, contents } => match fs::write(self.workspace.join(path), contents) {
                Ok(_) => Outcome { ok: true, detail: "written".into() },
                Err(e) => Outcome { ok: false, detail: e.to_string() },
            },
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
}
