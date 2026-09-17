use executor::action::Action;
use executor::executor::{ExecOutcome, Executor};
use executor::log::ActionLog;
use executor::admin::AdminWorker;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

fn main() {
    let json = std::env::args().nth(1).expect("usage: executor '<action-json>'");
    let action: Action = serde_json::from_str(&json).expect("invalid action JSON");
    let ws = PathBuf::from("/data/jobs/demo");
    std::fs::create_dir_all(&ws).ok();
    let sandbox: Box<dyn Worker> = Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() });
    let admin: Box<dyn Worker> = Box::new(AdminWorker);
    // Task 7 swaps the 8192 for the model's own context size.
    let desktop: Box<dyn Worker> = Box::new(DesktopWorker(DesktopState::for_model(8192)));
    let log = ActionLog::open("/data/ai-os.db").expect("open log");
    let exec = Executor::new(sandbox, admin, desktop, log, ws);
    match exec.execute("demo", &action, false).expect("execute") {
        ExecOutcome::Ran(o) => println!("RAN ok={} detail={}", o.ok, o.detail),
        ExecOutcome::Blocked(reason) => println!("BLOCKED: {reason}"),
    }
}
