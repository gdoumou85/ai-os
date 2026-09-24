//! `ai-os-alert <watcher> <what happened>`: a live watcher's program wakes the AI (watchers
//! design §1). Exit 0 once the service took it, 1 with the reason when it did not, 2 on bad usage.
use std::io::{BufRead, BufReader, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [watcher, text @ ..] = &args[..] else { eprintln!("usage: ai-os-alert <watcher> <what happened>"); std::process::exit(2) };
    let text = text.join(" ");
    if text.trim().is_empty() { eprintln!("usage: ai-os-alert <watcher> <what happened>"); std::process::exit(2) }
    let sock = aios_core::service::socket_path();
    let mut s = match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(s) => s,
        Err(e) => { eprintln!("the AI OS service is not running ({}: {e})", sock.display()); std::process::exit(1) }
    };
    let line = serde_json::to_string(&aios_proto::Request::Alert { watcher: watcher.clone(), text }).expect("a request serialises");
    if s.write_all(format!("{line}\n").as_bytes()).is_err() { eprintln!("could not reach the AI OS service"); std::process::exit(1) }
    let _ = s.set_read_timeout(Some(std::time::Duration::from_secs(30)));
    // Everything broadcast carries a `seq`; the answer to this request alone does not.
    for l in BufReader::new(s).lines().map_while(Result::ok) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&l) else { continue };
        if v.get("seq").is_some() { continue }
        let said = v["text"].as_str().unwrap_or_default();
        if v["kind"] == "error" { eprintln!("{said}"); std::process::exit(1) }
        println!("{said}");
        std::process::exit(0)
    }
    eprintln!("the AI OS service did not answer");
    std::process::exit(1)
}
