use crate::action::Action;
use crate::log::{ActionLog, LogError};
use crate::rules::{classify, resolves_inside, under_any, wrong_hand, Risk};
use crate::undo::UndoEntry;
use crate::worker::{Outcome, Worker};
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum ExecOutcome {
    Ran(Outcome),
    Blocked(String),
}

/// Which hand performs an action (spec §6). `Engine` actions touch no worker at all —
/// the engine applies them itself and records them with `log_only`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lane {
    Sandbox,
    Admin,
    Engine,
}

/// Pure: the same action, workspace and approval always pick the same hand.
pub fn lane(action: &Action, workspace: &Path, approved: bool) -> Lane {
    match action {
        Action::Install { .. } | Action::Remove { .. } | Action::Service { .. } | Action::MakeDir { .. } => Lane::Admin,
        // A file action only leaves the sandbox once it is both outside the workspace and
        // approved. Reads under /etc stay in the sandbox: they are world-readable, so the
        // privileged hand buys nothing.
        Action::ReadFile { path, .. } | Action::WriteFile { path, .. } | Action::EditFile { path, .. } => {
            let etc_read = matches!(action, Action::ReadFile { .. }) && under_any(path, &["/etc"]);
            if !resolves_inside(path, workspace) && approved && !etc_read {
                Lane::Admin
            } else {
                Lane::Sandbox
            }
        }
        Action::SetSetting { .. } => Lane::Engine,
        // Listed, not `_`: a new action kind must fail to compile here rather than land
        // silently in the sandbox.
        Action::RunCommand { .. } | Action::HttpPost { .. } | Action::FetchPackages { .. } => Lane::Sandbox,
    }
}

pub struct Executor<W: Worker> {
    sandbox: W,
    admin: W,
    log: ActionLog,
    workspace: PathBuf,
}

impl<W: Worker> Executor<W> {
    pub fn new(sandbox: W, admin: W, log: ActionLog, workspace: PathBuf) -> Self {
        Self { sandbox, admin, log, workspace }
    }

    /// The one door. Classify, refuse unconfirmed risky actions, run the rest, log everything.
    ///
    /// Invariant: an executed action is never lost — a logging failure is reported to
    /// stderr but the outcome is still returned; only the blocked path may fail on
    /// logging since nothing ran.
    pub fn execute(&self, job_id: &str, action: &Action, approved: bool) -> Result<ExecOutcome, LogError> {
        // The wrong hand is refused with the right one named, never run and never put to the
        // user. A free command that belongs to another hand would fail in the sandbox in a way
        // that teaches the model the wrong thing about the machine (see `wrong_hand`); a
        // `make_dir` the privileged hand cannot carry out would go to the approval gate and, once
        // the user said yes, be refused by the wrapper anyway — it takes absolute paths outside
        // the workspace only, so a relative one means nothing to it whether it points into the
        // project or out of it. A yes that buys a failure is worse than a refusal that names the
        // hand, and the rule the prompt states ("make_dir is never used with a relative path") is
        // enforced here rather than merely asked for.
        let wrong = match action {
            Action::RunCommand { argv } => wrong_hand(argv),
            Action::MakeDir { path } if !Path::new(path).is_absolute() || resolves_inside(path, &self.workspace) => {
                Some(if resolves_inside(path, &self.workspace) {
                    format!("a folder inside the working directory is made with `run_command mkdir -p {path}`: make_dir is the hand for folders outside it and takes an absolute path")
                } else {
                    format!("make_dir takes an absolute path: `{path}` is relative, and the hand that would make it works from no working directory of yours")
                })
            }
            _ => None,
        };
        if let Some(reason) = wrong {
            if let Err(e) = self.log.append(job_id, action, &format!("error: {reason}")) {
                eprintln!("executor: failed to log a wrong-hand refusal: {e}");
            }
            return Ok(ExecOutcome::Ran(Outcome::err(reason)));
        }
        match classify(action, &self.workspace) {
            Risk::NeedsConfirm(reason) if !approved => {
                self.log.append(job_id, action, &format!("blocked: {reason}"))?;
                Ok(ExecOutcome::Blocked(reason))
            }
            _ => {
                let worker = match lane(action, &self.workspace, approved) {
                    Lane::Sandbox => &self.sandbox,
                    Lane::Admin => &self.admin,
                    // A programming error: the engine applies its own actions and records them
                    // with `log_only`. Nothing ran, so the log failing here is worth reporting.
                    Lane::Engine => {
                        let reason = "engine action reached the executor";
                        self.log.append(job_id, action, &format!("blocked: {reason}"))?;
                        return Ok(ExecOutcome::Blocked(reason.into()));
                    }
                };
                let outcome = worker.run(action);
                let tag = if outcome.ok { "ok" } else { "error" };
                // ponytail: log-failure branch is not unit-tested (needs failure-injection infra; out of scope this wave).
                if let Err(e) = self.log.append(job_id, action, &format!("{tag}: {}", outcome.detail)) {
                    eprintln!("executor: failed to log action outcome: {e}");
                }
                Ok(ExecOutcome::Ran(outcome))
            }
        }
    }

    /// Record an action no worker performed — the engine's own lane (`SetSetting`).
    pub fn log_only(&self, job_id: &str, action: &Action, detail: &str) -> Result<(), LogError> {
        self.log.append(job_id, action, detail)?;
        Ok(())
    }

    /// Record a line no action and no worker stands behind — the engine's own reversals (a
    /// setting, a project snapshot). `reverse` cannot log those: they never reach a worker,
    /// and a snapshot has no `Action` to log against at all.
    pub fn log_text(&self, job_id: &str, text: &str) -> Result<(), LogError> {
        self.log.append_text(job_id, text)?;
        Ok(())
    }

    /// Put one recorded change back. Only the privileged hand can undo.
    ///
    /// Same invariant as `execute`: a logging failure never loses the outcome.
    pub fn reverse(&self, job_id: &str, entry: &UndoEntry) -> Result<Outcome, LogError> {
        let outcome = self.admin.reverse(entry);
        let tag = if outcome.ok { "ok" } else { "error" };
        // The entry goes in the line: there is no action column behind an undo, so this is the
        // only record of WHAT was put back.
        if let Err(e) = self.log.append_text(job_id, &format!("undo: {entry:?}: {tag}: {}", outcome.detail)) {
            eprintln!("executor: failed to log undo outcome: {e}");
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Manager, ServiceDo};
    use crate::undo::UndoEntry;
    use crate::worker::FakeWorker;

    fn exec(ok: bool) -> Executor<FakeWorker> {
        Executor::new(
            FakeWorker::new(ok),
            FakeWorker::new(ok),
            ActionLog::open_in_memory().unwrap(),
            PathBuf::from("/data/jobs/j1"),
        )
    }

    #[test]
    fn auto_action_runs_and_logs() {
        let e = exec(true);
        let a = Action::RunCommand { argv: vec!["ls".into()] };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.sandbox.calls.borrow().len(), 1);
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1);
    }

    #[test]
    fn a_command_that_belongs_to_another_hand_never_reaches_the_sandbox() {
        let e = exec(true);
        let a = Action::RunCommand { argv: vec!["apt-get".into(), "install".into(), "-y".into(), "cowsay".into()] };
        match e.execute("j1", &a, false).unwrap() {
            ExecOutcome::Ran(o) => assert!(!o.ok && o.detail.contains("`install` action"), "{o:?}"),
            other => panic!("a refusal is a failed step, not {other:?}"),
        }
        assert!(e.sandbox.calls.borrow().is_empty() && e.admin.calls.borrow().is_empty());
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1, "the refusal is logged like any outcome");
    }

    #[test]
    fn risky_action_without_approval_is_blocked_and_not_run() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Blocked(_)));
        assert_eq!(e.sandbox.calls.borrow().len(), 0, "blocked action must never reach the worker");
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1, "the block itself is logged");
    }

    #[test]
    fn risky_action_with_approval_runs() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, true).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.sandbox.calls.borrow().len(), 1);
    }

    #[test]
    fn admin_kinds_go_to_the_admin_lane() {
        let e = exec(true);
        e.execute("j", &Action::Install { packages: vec!["cowsay".into()] }, false).unwrap();
        e.execute("j", &Action::Service { name: "nginx".into(), action: ServiceDo::Restart }, false).unwrap();
        e.execute("j", &Action::MakeDir { path: "/data/x".into() }, false).unwrap();
        assert_eq!(e.admin.calls.borrow().len(), 3);
        assert!(e.sandbox.calls.borrow().is_empty());
    }

    #[test]
    fn a_make_dir_inside_the_working_directory_is_refused_with_the_right_hand_named() {
        // The live 1d run had the 9B plan `make_dir src` for a folder in its own project. The
        // wrapper takes absolute paths outside the workspace only, so the approval gate would
        // have spent the user's yes on an action that then fails.
        let e = exec(true);
        for path in ["src", "/data/jobs/j1/src", "./a/b"] {
            let out = e.execute("j", &Action::MakeDir { path: path.into() }, false).unwrap();
            let ExecOutcome::Ran(o) = out else { panic!("{path} was not refused outright") };
            assert!(!o.ok && o.detail.contains("mkdir -p"), "{path}: {}", o.detail);
        }
        // A relative path that points *out* of the workspace is no better: the hand works from
        // no working directory at all, so it is refused for being relative rather than put to
        // the user as a yes the wrapper would then throw away.
        for path in ["../x", "../../../opt/x"] {
            let out = e.execute("j", &Action::MakeDir { path: path.into() }, false).unwrap();
            let ExecOutcome::Ran(o) = out else { panic!("{path} was not refused outright") };
            assert!(!o.ok && o.detail.contains("absolute path"), "{path}: {}", o.detail);
        }
        assert!(e.admin.calls.borrow().is_empty() && e.sandbox.calls.borrow().is_empty());
        // Absolute and outside, `make_dir` is still the hand; absolute and outside the AI's own
        // areas still needs a yes.
        assert!(matches!(e.execute("j", &Action::MakeDir { path: "/data/x".into() }, false).unwrap(), ExecOutcome::Ran(_)));
        assert!(matches!(e.execute("j", &Action::MakeDir { path: "/opt/x".into() }, false).unwrap(), ExecOutcome::Blocked(_)));
    }

    #[test]
    fn approved_outside_write_goes_admin_inside_write_goes_sandbox() {
        let e = exec(true);
        e.execute("j", &Action::WriteFile { path: "a.py".into(), contents: "x".into() }, false).unwrap();
        assert!(matches!(
            e.execute("j", &Action::WriteFile { path: "/etc/x".into(), contents: "x".into() }, false).unwrap(),
            ExecOutcome::Blocked(_)
        ));
        e.execute("j", &Action::WriteFile { path: "/etc/x".into(), contents: "x".into() }, true).unwrap();
        // Approval alone never promotes a lane: inside the workspace stays in the sandbox.
        e.execute("j", &Action::WriteFile { path: "a.py".into(), contents: "x".into() }, true).unwrap();
        assert_eq!(e.sandbox.calls.borrow().len(), 2);
        assert_eq!(e.admin.calls.borrow().len(), 1);
    }

    #[test]
    fn etc_read_and_fetch_stay_in_the_sandbox() {
        let e = exec(true);
        e.execute("j", &Action::ReadFile { path: "/etc/fstab".into(), from_line: None, lines: None }, false).unwrap();
        e.execute("j", &Action::FetchPackages { manager: Manager::Pip, packages: vec!["x".into()] }, false).unwrap();
        assert_eq!(e.sandbox.calls.borrow().len(), 2);
        assert!(e.admin.calls.borrow().is_empty());
    }

    #[test]
    fn set_setting_never_reaches_a_worker_but_log_only_records_it() {
        let e = exec(true);
        let a = Action::SetSetting { key: "projects_root".into(), value: "/data/w".into() };
        assert_eq!(lane(&a, Path::new("/data/jobs/j1"), false), Lane::Engine);
        e.log_only("j", &a, "ok: set").unwrap();
        assert!(e.sandbox.calls.borrow().is_empty() && e.admin.calls.borrow().is_empty());
        assert_eq!(e.log.count_for_job("j").unwrap(), 1);
    }

    #[test]
    fn an_engine_action_that_reaches_execute_is_blocked_not_run() {
        let e = exec(true);
        let a = Action::SetSetting { key: "k".into(), value: "v".into() };
        assert!(matches!(e.execute("j", &a, true).unwrap(), ExecOutcome::Blocked(_)));
        assert!(e.sandbox.calls.borrow().is_empty() && e.admin.calls.borrow().is_empty());
        assert_eq!(e.log.count_for_job("j").unwrap(), 1);
    }

    #[test]
    fn reverse_goes_through_admin_and_is_logged() {
        let e = exec(true);
        e.reverse("j", &UndoEntry::DirCreated { path: "/data/x".into() }).unwrap();
        assert_eq!(e.admin.reversed.borrow().len(), 1);
        assert!(e.sandbox.reversed.borrow().is_empty());
        assert_eq!(e.log.count_for_job("j").unwrap(), 1);
    }

    #[test]
    fn log_text_records_a_reversal_no_worker_performed() {
        // The engine puts settings and project snapshots back itself, so nothing else would
        // leave a trace of them in the action log.
        let e = exec(true);
        e.log_text("j", "undo: Setting { .. }: ok: put back").unwrap();
        assert_eq!(e.log.count_for_job("j").unwrap(), 1);
        assert!(e.admin.reversed.borrow().is_empty() && e.sandbox.calls.borrow().is_empty());
    }
}
