//! The engine as a long-running service on a private socket; front doors connect to it.
use aios_core::engine::{Engine, HOUSEKEEPING_DIR};
use aios_core::model::{Model, RemoteModel};
use aios_core::service;
use aios_core::store::Store;
use executor::admin::AdminWorker;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

fn main() {
    let db = std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into());
    let model = std::env::var("AI_OS_MODEL").unwrap_or_else(|_| "qwen3.5:9b".into());
    let root = PathBuf::from(std::env::var("AI_OS_PROJECTS").unwrap_or_else(|_| "/data/projects".into()));
    let sock = service::socket_path();
    let listener = service::bind(&sock).unwrap_or_else(|e| { eprintln!("cannot listen on {}: {e}", sock.display()); std::process::exit(1) });
    eprintln!("ai-os-engine listening on {}", sock.display());
    service::run(listener, Box::new(move |sink| {
        let store = Store::open(&db).expect("open store");
        // One state for the whole process: the accessibility bus connection and the id table
        // outlive any one job. The look cap follows the model's own context (2a §4).
        let llm = RemoteModel::from_env(&model);
        let desktop = DesktopState::for_model(llm.context_tokens());
        Engine::new(store, llm, root, Some(db),
            Box::new(move |ws| (
                Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
                Box::new(AdminWorker) as Box<dyn Worker>,
                Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
            )),
            PathBuf::from(HOUSEKEEPING_DIR),
            PathBuf::from("/data/snapshots")).with_sink(sink)
    }))
}
