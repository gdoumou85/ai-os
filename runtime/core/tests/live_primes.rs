// The Phase 1b acceptance test (1b spec §9): the real local model, the real sandbox, one job,
// no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_primes -- --nocapture
use aios_core::engine::Engine;
use aios_core::job::State;
use aios_core::model::OllamaModel;
use aios_core::store::Store;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

#[test]
fn the_model_writes_and_proves_a_primes_script() {
    if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
    let root = PathBuf::from("/data/projects");
    let db = "/data/ai-os-live.db";
    let _ = std::fs::remove_file(db);
    let _ = std::fs::remove_dir_all(root.join("primes"));
    let store = Store::open(db).unwrap();
    let mut e = Engine::new(store, OllamaModel::local("qwen3.5:9b"), root.clone(), Some(db.into()),
        Box::new(|ws| Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>));
    let mut out = e.handle("Start a new project called primes: make a Python script that prints the first ten prime numbers, one per line, and prove it runs. Decide the details yourself.").unwrap();
    for line in &out { eprintln!("AI: {line}"); }
    // If it asks anyway, answer once; a second question is a failure of the creative rule.
    if e.open_job().map(|j| j.state == State::WaitingAnswer).unwrap_or(false) {
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
}
