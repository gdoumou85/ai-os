// Phase 2a acceptance (2a design §12 item 4): the real model, three hands, the service on a temp
// socket, no human. Inside the distro with the desktop unit up:
//   trial/run-live-2a.sh   (sets the displays and AI_OS_LIVE=1)
use aios_core::engine::Engine;
use aios_core::model::{Model, OllamaModel};
use aios_core::service;
use aios_core::store::Store;
use aios_proto::{Client, Event};
use executor::admin::AdminWorker;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DB: &str = "/data/ai-os-live-2a.db";
const NOTES: &str = "/data/housekeeping/live-2a-notes.txt";

fn start_service() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ai-os-live-2a-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    std::thread::spawn(move || service::run(listener, Box::new(|sink| {
        let model = OllamaModel::local("qwen3.5:9b");
        let desktop = DesktopState::for_model(model.context_tokens());
        Engine::new(Store::open(DB).unwrap(), model, PathBuf::from("/data/projects"), Some(DB.into()),
            Box::new(move |ws| (
                Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
                Box::new(AdminWorker) as Box<dyn Worker>,
                Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
            )),
            PathBuf::from("/data/housekeeping"), PathBuf::from("/data/snapshots")).with_sink(sink)
    })));
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

/// Say it, print every event, return once `stop` matches. Panics after `limit` seconds.
fn say_until(c: &mut Client, text: &str, limit: u64, stop: impl Fn(&Event) -> bool) -> Vec<Event> {
    println!("you> {text}");
    c.set_read_timeout(Some(Duration::from_secs(limit))).unwrap();
    c.say(text).unwrap();
    let t = Instant::now();
    let mut got = vec![];
    while let Some(e) = c.next_event() {
        println!("ev> {}", serde_json::to_string(&e).unwrap());
        let done = stop(&e);
        got.push(e);
        if done { return got; }
        assert!(t.elapsed().as_secs() < limit, "no end after {limit}s: {got:?}");
    }
    panic!("connection closed or nothing said for {limit}s: {got:?}");
}

fn ended(e: &Event) -> bool { matches!(e, Event::Done { .. } | Event::Failed { .. } | Event::Stopped { .. }) }

fn editor_running() -> bool {
    std::process::Command::new("pgrep").args(["-u", "ai", "-f", "gnome-text-editor"]).output().map(|o| !o.stdout.is_empty()).unwrap_or(false)
}

#[test]
fn a_window_of_the_users_a_window_of_its_own_and_a_risky_press() {
    if std::env::var("AI_OS_LIVE").is_err() { eprintln!("skipped: AI_OS_LIVE not set"); return; }
    let _ = std::fs::remove_file(DB);
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-text-editor|gnome-calculator"]).status();
    // Text Editor keeps a draft of every buffer it did not save and restores it on the next start,
    // banner and all; a run must open on the file, not on the last run's leavings.
    let _ = std::fs::remove_dir_all("/home/ai/.local/share/org.gnome.TextEditor");
    std::fs::write(NOTES, "first line\n").unwrap();
    // The user's window: on the visible display, opened by the user (here: the test), unsaved nothing yet.
    std::process::Command::new("gnome-text-editor").arg(NOTES)
        .env("WAYLAND_DISPLAY", "wayland-0").env("GNOME_ACCESSIBILITY", "1").env_remove("DISPLAY")
        .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(6));
    let sock = start_service();
    let mut c = Client::connect(&sock).unwrap();
    c.hello().unwrap();
    assert_eq!(c.next_event(), Some(Event::State { job: None }));

    // Script 1: the user's window.
    let ev = say_until(&mut c, "take my Text Editor window, add a line saying reviewed at the end and save it; decide everything yourself and do not ask", 500, ended);
    assert!(matches!(ev.last().unwrap(), Event::Done { .. }), "script 1: {ev:?}");
    let saved = std::fs::read_to_string(NOTES).unwrap();
    assert!(saved.to_lowercase().contains("reviewed"), "the file on disk has the line: {saved:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Done { windows, .. } if !windows.is_empty())), "the Done card names the window");

    // Script 2: the AI's own window, invisible.
    let ev = say_until(&mut c, "open the calculator and tell me what 12 times 34 is; decide everything yourself and do not ask", 500, ended);
    let Event::Done { text, .. } = ev.last().unwrap() else { panic!("script 2: {ev:?}") };
    assert!(text.contains("408") || ev.iter().any(|e| matches!(e, Event::Said { text } if text.contains("408"))), "the answer is said: {ev:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Step { text, .. } if text.starts_with("opened "))), "it opened the app itself");

    // Script 3: the risky press.
    let ev = say_until(&mut c, "take my Text Editor window and close it without saving; decide everything yourself and do not ask", 300,
        |e| matches!(e, Event::NeedsOk { .. }) || ended(e));
    let Event::NeedsOk { what, why, .. } = ev.last().unwrap() else { panic!("script 3 stopped at an OK: {ev:?}") };
    assert!(what.starts_with("pressed ") && why.contains("unsaved work"), "{what} / {why}");
    let ev = say_until(&mut c, "yes", 300, ended);
    let t = Instant::now();
    while editor_running() && t.elapsed() < Duration::from_secs(15) { std::thread::sleep(Duration::from_millis(500)); }
    assert!(!editor_running(), "the editor is gone after the yes: {ev:?}");
    let _ = std::process::Command::new("pkill").args(["-u", "ai", "-f", "gnome-calculator"]).status();
}
