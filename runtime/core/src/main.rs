//! The builder's terminal front door: a thin client of ai-os-engine (1d design §3.5).
use aios_core::event::lines;
use aios_proto::Client;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn main() {
    let sock = aios_core::service::socket_path();
    let client = match Client::connect(&sock) {
        Ok(c) => c,
        Err(e) => { eprintln!("The AI OS service is not running ({}: {e}). Start it: systemctl --user start ai-os-engine", sock.display()); std::process::exit(1) }
    };
    // One connection, two halves: the reader prints on its own thread so it never blocks the prompt.
    let (mut reader, mut client) = client.split();
    client.hello().ok();
    // Shared with the reader thread: once stdin closes (a pipe, or Ctrl-D) the main thread stops
    // reading input, but a reply can still be in flight (`printf … | ai-os-chat` closes stdin the
    // instant it has written its line, long before the model answers). Track the last event so
    // the process can wait for that reply instead of exiting the moment stdin runs dry.
    let last_event = Arc::new(Mutex::new(Instant::now()));
    let last_event_reader = last_event.clone();
    std::thread::spawn(move || {
        while let Some(ev) = reader.next_event() {
            *last_event_reader.lock().unwrap() = Instant::now();
            for l in lines(&ev) { println!("ai> {l}"); }
            print!("you> "); std::io::stdout().flush().ok();
        }
        println!("(the service closed the connection)");
        std::process::exit(0);
    });
    let stdin = std::io::stdin();
    print!("you> "); std::io::stdout().flush().ok();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let text = line.trim();
        if text.is_empty() { print!("you> "); std::io::stdout().flush().ok(); continue; }
        if client.say(text).is_err() { break; }
    }
    // Stdin is closed. The reader thread is still live and will exit(0) itself the moment the
    // service actually closes the connection; until then, stay up so a reply already in flight
    // gets printed, but give up after 60s of silence.
    loop {
        std::thread::sleep(Duration::from_millis(200));
        if last_event.lock().unwrap().elapsed() > Duration::from_secs(60) { std::process::exit(0); }
    }
}
