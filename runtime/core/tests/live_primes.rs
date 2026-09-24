// The Phase 1b acceptance test (1b spec §9): the real local model, the real machine, one job,
// no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_primes -- --nocapture
use aios_core::engine::Engine;
use aios_core::model::{Model, RemoteModel};
use aios_core::store::Store;
use aios_proto::Event;
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
    let mut ev = e.handle_events("Start a new project called primes: make a Python script that prints the first ten prime numbers, one per line, and prove it runs. Decide the details yourself.").unwrap();
    for line in ev.iter().flat_map(aios_core::event::lines) { eprintln!("AI: {line}"); }
    // If it asks anyway, answer once; a second question is a failure of "decide yourself".
    if matches!(ev.last(), Some(Event::NeedsAnswer { .. })) {
        ev = e.handle_events("You decide.").unwrap();
        for line in ev.iter().flat_map(aios_core::event::lines) { eprintln!("AI: {line}"); }
    }
    assert!(ev.iter().any(|v| matches!(v, Event::Done { .. })), "the turn must end Finished: {ev:?}");
    let folder = root.join("primes");
    assert!(folder.join("BLUEPRINT.md").exists(), "blueprint must exist in {folder:?}");
    let py = std::fs::read_dir(&folder).unwrap().filter_map(|d| d.ok()).any(|d| d.path().extension().map(|x| x == "py").unwrap_or(false));
    assert!(py, "a .py file must exist in {folder:?}");
    // The proof: a run of the script whose real output holds the tenth prime, from the
    // conversation's own result rows — a lazy `true` would not pass this.
    let rows = e.store.all_messages().unwrap();
    assert!(rows.iter().any(|(r, t)| r == "result" && t.contains(".py") && t.contains("-> ok") && t.contains("29")), "no run of the script printed 29: {rows:?}");
}
