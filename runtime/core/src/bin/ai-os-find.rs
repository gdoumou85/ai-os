//! Lists the AI models on this machine's home network, one per line (network-models spec §1):
//!   ai-os-find              scan the network
//!   ai-os-find --url URL    ask that one runner
//! Lines are `kind<TAB>url<TAB>model`, or `kind<TAB>url<TAB>-<TAB>needs-key`. A key for LM Studio
//! comes only from AI_OS_MODEL_KEY: a command line is readable by every user through `ps`.
use aios_core::find::{self, Kind};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let url = match args.as_slice() {
        [] => None,
        [flag, u] if flag == "--url" => Some(u.trim_end_matches('/').to_string()),
        _ => { eprintln!("usage: ai-os-find [--url URL]"); std::process::exit(2) }
    };
    let key = std::env::var("AI_OS_MODEL_KEY").ok().filter(|k| !k.is_empty());
    let found = match url {
        Some(u) => {
            let ollama = find::ask(Kind::Ollama, &u, None);
            if ollama.is_empty() { find::ask(Kind::OpenAi, &u, key.as_deref()) } else { ollama }
        }
        None => find::scan(&find::home_hosts(), find::OLLAMA_PORT, find::LMSTUDIO_PORT, key.as_deref()),
    };
    for f in &found {
        match &f.model {
            Some(m) => println!("{}\t{}\t{m}", f.kind.as_str(), f.url),
            None => println!("{}\t{}\t-\tneeds-key", f.kind.as_str(), f.url),
        }
    }
    if found.is_empty() { std::process::exit(1) }
}
