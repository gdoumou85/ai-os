use aios_core::engine::Engine;
use aios_core::model::OllamaModel;
use aios_core::store::Store;
use executor::worker::{SandboxWorker, Worker};
use std::io::{BufRead, Write};
use std::path::PathBuf;

fn main() {
    let db = std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into());
    let model = std::env::var("AI_OS_MODEL").unwrap_or_else(|_| "qwen3.5:9b".into());
    let root = PathBuf::from(std::env::var("AI_OS_PROJECTS").unwrap_or_else(|_| "/data/projects".into()));
    let store = Store::open(&db).expect("open store");
    let mut engine = Engine::new(store, OllamaModel::local(&model), root, Some(db),
        Box::new(|ws| Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>));
    // Builder's front (1b spec §9): the 1d rail draws this same conversation as cards.
    let stdin = std::io::stdin();
    loop {
        print!("you> "); std::io::stdout().flush().ok();
        let mut line = String::new();
        if stdin.lock().read_line(&mut line).unwrap_or(0) == 0 { break; }
        let text = line.trim();
        if text.is_empty() { continue; }
        match engine.handle(text) {
            Ok(lines) => for l in lines { println!("ai> {l}"); },
            Err(e) => println!("ai> (something went wrong: {e})"),
        }
    }
}
