use executor::action::Action;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::executor::Executor;
use executor::log::ActionLog;
use executor::worker::{MachineWorker, Worker};
use std::path::PathBuf;

fn main() {
    let json = std::env::args().nth(1).expect("usage: executor '<action-json>'");
    let action: Action = serde_json::from_str(&json).expect("invalid action JSON");
    let ws = PathBuf::from("/data/jobs/demo");
    std::fs::create_dir_all(&ws).ok();
    let machine: Box<dyn Worker> = Box::new(MachineWorker { workspace: ws.clone() });
    // The demo binary has no model to ask for a context size; 8192 is the workshop's.
    let desktop: Box<dyn Worker> = Box::new(DesktopWorker(DesktopState::for_model(8192)));
    let log = ActionLog::open("/data/ai-os.db").expect("open log");
    let exec = Executor::new(machine, desktop, log, ws);
    let o = exec.execute("demo", &action).expect("execute");
    println!("RAN ok={} detail={}", o.ok, o.detail);
}
