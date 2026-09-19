// The Phase 1b acceptance test (1b spec §9): the real local model, the real machine, one job,
// no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_primes -- --nocapture
use aios_core::engine::Engine;
use aios_core::job::{Job, State};
use aios_core::model::{Model, RemoteModel};
use aios_core::store::Store;
use executor::action::Action;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::worker::{MachineWorker, Worker};
use std::path::PathBuf;

#[test]
fn the_model_writes_and_proves_a_primes_script() {
    if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
    let root = PathBuf::from("/data/projects");
    let db = "/data/ai-os-live.db";
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root.join("primes"));
    let store = Store::open(db).unwrap();
    let llm = RemoteModel::local("qwen3.5:9b");
    let desktop = DesktopState::for_model(llm.context_tokens());
    let mut e = Engine::new(store, llm, root.clone(), Some(db.into()),
        Box::new(move |ws| (
            Box::new(MachineWorker { workspace: ws.to_path_buf() }) as Box<dyn Worker>,
            Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
        )),
        PathBuf::from("/data/housekeeping"));
    let mut out = e.handle("Start a new project called primes: make a Python script that prints the first ten prime numbers, one per line, and prove it runs. Decide the details yourself.").unwrap();
    for line in &out { eprintln!("AI: {line}"); }
    // If it asks anyway, answer once; a second question is a failure of the creative rule.
    if e.open_job().unwrap().map(|j| j.state == State::WaitingAnswer).unwrap_or(false) {
        out = e.handle("You decide.").unwrap();
        for line in &out { eprintln!("AI: {line}"); }
    }
    let job = e.store.open_job().unwrap();
    assert!(job.is_none(), "job should be finished, still open: {job:?}");
    let projects = e.store.list_projects().unwrap();
    assert!(!projects.is_empty(), "the model never started a job — replies: {out:?}");
    let name = projects[0].name.clone();
    let folder = root.join(&name);
    assert!(folder.join("BLUEPRINT.md").exists(), "blueprint must exist");
    let py = std::fs::read_dir(&folder).unwrap().filter_map(|d| d.ok()).any(|d| d.path().extension().map(|x| x == "py").unwrap_or(false));
    assert!(py, "a .py file must exist in {folder:?}");
    assert!(out.last().unwrap().to_lowercase().contains("gave up") == false, "{out:?}");
    // The proof: the check's real output holds the tenth prime.
    let last_line = out.last().unwrap().clone();
    eprintln!("RESULT: {last_line}");

    // Structural checks above (BLUEPRINT.md + a .py file exist) would pass a lazy check like
    // `run_command ["true"]`. Open the live DB directly and inspect the last step's actual action
    // and outcome: it must be a python run whose real stdout proves the tenth prime, not a stub.
    let conn = rusqlite::Connection::open(db).unwrap();
    let json: String = conn.query_row("SELECT json FROM core_jobs ORDER BY updated_at DESC LIMIT 1", [], |r| r.get(0)).unwrap();
    let job: Job = serde_json::from_str(&json).unwrap();
    assert_eq!(job.state, State::Done, "{job:?}");
    let last_step = job.steps.last().expect("a finished job must have at least one step");
    match &last_step.action {
        Action::RunCommand { argv } => {
            assert!(argv.iter().any(|a| a.ends_with(".py")), "the proof must run the .py file: {argv:?}");
        }
        other => panic!("the last step must be the check running the script, not {other:?}"),
    }
    assert!(last_step.ok, "the check must have actually passed: {last_step:?}");
    assert!(last_step.detail.contains("29"), "the check's real output must hold the tenth prime: {last_step:?}");
}
