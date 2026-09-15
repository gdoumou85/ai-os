use crate::action::Action;
use crate::log::{ActionLog, LogError};
use crate::rules::{classify, Risk};
use crate::worker::{Outcome, Worker};
use std::path::PathBuf;

#[derive(Debug, PartialEq)]
pub enum ExecOutcome {
    Ran(Outcome),
    Blocked(String),
}

pub struct Executor<W: Worker> {
    worker: W,
    log: ActionLog,
    workspace: PathBuf,
}

impl<W: Worker> Executor<W> {
    pub fn new(worker: W, log: ActionLog, workspace: PathBuf) -> Self {
        Self { worker, log, workspace }
    }

    /// The one door. Classify, refuse unconfirmed risky actions, run the rest, log everything.
    ///
    /// Invariant: an executed action is never lost — a logging failure is reported to
    /// stderr but the outcome is still returned; only the blocked path may fail on
    /// logging since nothing ran.
    pub fn execute(&self, job_id: &str, action: &Action, approved: bool) -> Result<ExecOutcome, LogError> {
        match classify(action, &self.workspace) {
            Risk::NeedsConfirm(reason) if !approved => {
                self.log.append(job_id, action, &format!("blocked: {reason}"))?;
                Ok(ExecOutcome::Blocked(reason))
            }
            _ => {
                let outcome = self.worker.run(action);
                let tag = if outcome.ok { "ok" } else { "error" };
                // ponytail: log-failure branch is not unit-tested (needs failure-injection infra; out of scope this wave).
                if let Err(e) = self.log.append(job_id, action, &format!("{tag}: {}", outcome.detail)) {
                    eprintln!("executor: failed to log action outcome: {e}");
                }
                Ok(ExecOutcome::Ran(outcome))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::FakeWorker;

    fn exec(ok: bool) -> Executor<FakeWorker> {
        Executor::new(FakeWorker::new(ok), ActionLog::open_in_memory().unwrap(), PathBuf::from("/data/jobs/j1"))
    }

    #[test]
    fn auto_action_runs_and_logs() {
        let e = exec(true);
        let a = Action::RunCommand { argv: vec!["ls".into()] };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.worker.calls.borrow().len(), 1);
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1);
    }

    #[test]
    fn risky_action_without_approval_is_blocked_and_not_run() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, false).unwrap();
        assert!(matches!(out, ExecOutcome::Blocked(_)));
        assert_eq!(e.worker.calls.borrow().len(), 0, "blocked action must never reach the worker");
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1, "the block itself is logged");
    }

    #[test]
    fn risky_action_with_approval_runs() {
        let e = exec(true);
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let out = e.execute("j1", &a, true).unwrap();
        assert!(matches!(out, ExecOutcome::Ran(_)));
        assert_eq!(e.worker.calls.borrow().len(), 1);
    }
}
