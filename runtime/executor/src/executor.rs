use crate::action::Action;
use crate::log::{ActionLog, LogError};
use crate::worker::{Outcome, Worker};
use std::path::PathBuf;

/// Which hand performs an action. `Engine` actions touch no worker at all — the engine applies
/// them itself and records them with `log_only`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Lane {
    Machine,
    Desktop,
    Engine,
}

/// Pure: the same action always picks the same hand.
pub fn lane(action: &Action) -> Lane {
    match action {
        Action::RunCommand { .. } | Action::ReadFile { .. } | Action::WriteFile { .. } | Action::EditFile { .. }
        | Action::AppendFile { .. }
        | Action::WebRead { .. } | Action::WebSearch { .. }
        | Action::StartProgram { .. } | Action::ProgramOutput { .. } | Action::StopProgram { .. } => Lane::Machine,
        Action::SetSetting { .. } | Action::Wait { .. } | Action::Watch { .. } | Action::Unwatch { .. } | Action::Delegate { .. } => Lane::Engine,
        // Listed, not `_`: a new action kind must fail to compile here rather than land
        // silently on the wrong hand.
        Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. }
        | Action::ScreenLook { .. } | Action::ScreenClick { .. } | Action::ScreenType { .. }
        | Action::Key { .. } | Action::Scroll { .. } | Action::Drag { .. } => Lane::Desktop,
    }
}

pub struct Executor<W: Worker> {
    machine: W,
    desktop: W,
    log: ActionLog,
    /// The job's folder, kept for the demo binary and the log; the machine hand holds its own.
    #[allow(dead_code)]
    workspace: PathBuf,
}

impl<W: Worker> Executor<W> {
    pub fn new(machine: W, desktop: W, log: ActionLog, workspace: PathBuf) -> Self {
        Self { machine, desktop, log, workspace }
    }

    /// The one door: run the action on its hand and log it. Nothing is ever held for a yes
    /// (full-access spec).
    ///
    /// Invariant: an executed action is never lost — a logging failure is reported to stderr
    /// but the outcome is still returned.
    pub fn execute(&self, job_id: &str, action: &Action) -> Result<Outcome, LogError> {
        let worker = match lane(action) {
            Lane::Machine => &self.machine,
            Lane::Desktop => &self.desktop,
            // A programming error: the engine applies its own actions and records them with
            // `log_only`. Nothing ran, so the log failing here is worth reporting.
            Lane::Engine => {
                let reason = "engine action reached the executor";
                self.log.append(job_id, action, &format!("error: {reason}"))?;
                return Ok(Outcome::err(reason));
            }
        };
        let outcome = worker.run(action);
        let tag = if outcome.ok { "ok" } else { "error" };
        if let Err(e) = self.log.append(job_id, action, &format!("{tag}: {}", outcome.detail)) {
            eprintln!("executor: failed to log action outcome: {e}");
        }
        Ok(outcome)
    }

    /// Record an action no worker performed — the engine's own lane (`SetSetting`).
    pub fn log_only(&self, job_id: &str, action: &Action, detail: &str) -> Result<(), LogError> {
        self.log.append(job_id, action, detail)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::FakeWorker;

    fn exec(ok: bool) -> Executor<FakeWorker> {
        Executor::new(FakeWorker::new(ok), FakeWorker::new(ok), ActionLog::open_in_memory().unwrap(), PathBuf::from("/data/jobs/j1"))
    }

    #[test]
    fn a_command_runs_on_the_machine_hand_and_is_logged() {
        let e = exec(true);
        let a = Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "cowsay".into()] };
        assert!(e.execute("j1", &a).unwrap().ok);
        assert_eq!(e.machine.calls.borrow().len(), 1);
        assert_eq!(e.log.count_for_job("j1").unwrap(), 1);
    }

    /// Full access: an outside write, a Delete press and a Send click all just run.
    #[test]
    fn nothing_is_held_for_a_yes() {
        let e = exec(true);
        for a in [Action::WriteFile { path: "/etc/x".into(), contents: "x".into() },
                  Action::Press { control: 2, name: "Delete".into() },
                  Action::ScreenClick { cell: 1, spot: 1, name: "Send".into(), double: false }] {
            assert!(e.execute("j", &a).unwrap().ok, "{a:?}");
        }
        assert_eq!(e.machine.calls.borrow().len(), 1);
        assert_eq!(e.desktop.calls.borrow().len(), 2);
    }

    #[test]
    fn set_setting_never_reaches_a_worker_but_log_only_records_it() {
        let e = exec(true);
        let a = Action::SetSetting { key: "projects_root".into(), value: "/data/w".into() };
        assert_eq!(lane(&a), Lane::Engine);
        assert!(!e.execute("j", &a).unwrap().ok, "an engine action that reaches execute is refused");
        e.log_only("j", &a, "ok: set").unwrap();
        assert!(e.machine.calls.borrow().is_empty() && e.desktop.calls.borrow().is_empty());
        assert_eq!(e.log.count_for_job("j").unwrap(), 2);
    }

    #[test]
    fn desktop_actions_take_the_desktop_lane() {
        for a in [Action::Look { window: None, find: None }, Action::Press { control: 1, name: "Bold".into() },
                  Action::Type { control: 1, text: "x".into(), replace: false }, Action::Read { control: 1, from_line: None, lines: None },
                  Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false },
                  Action::Key { keys: "ctrl+s".into() },
                  Action::Scroll { cell: 1, spot: 1, direction: "down".into(), amount: 3 },
                  Action::Drag { from_cell: 1, from_spot: 1, to_cell: 2, to_spot: 1 }] {
            assert_eq!(lane(&a), Lane::Desktop, "{a:?}");
        }
    }
}
