use executor::action::Action;
use executor::executor::{ExecOutcome, Executor};
use executor::log::ActionLog;
use executor::worker::SandboxWorker;
use std::path::PathBuf;

fn main() {
    let json = std::env::args().nth(1).expect("usage: executor '<action-json>'");
    let action: Action = serde_json::from_str(&json).expect("invalid action JSON");
    let ws = PathBuf::from("/data/jobs/demo");
    std::fs::create_dir_all(&ws).ok();
    let worker = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let log = ActionLog::open("/data/ai-os.db").expect("open log");
    let exec = Executor::new(worker, log, ws);
    match exec.execute("demo", &action, false).expect("execute") {
        ExecOutcome::Ran(o) => println!("RAN ok={} detail={}", o.ok, o.detail),
        ExecOutcome::Blocked(reason) => println!("BLOCKED: {reason}"),
    }
}
