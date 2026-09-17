// Phase 1d acceptance (1d design §7 item 4): the real model, both hands, through the service on
// a temp socket, no human. Inside the distro with Ollama and the wrapper installed:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_1d -- --nocapture
use aios_core::engine::Engine;
use aios_core::model::OllamaModel;
use aios_core::service;
use aios_core::store::Store;
use aios_proto::{Client, Event};
use executor::admin::AdminWorker;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;
use std::time::{Duration, Instant};

const DB: &str = "/data/ai-os-live-1d.db";
const PROJECT_DIR: &str = "/data/projects/rail-live";
const ETC_FILE: &str = "/etc/ai-os-live-1d.txt";

fn start_service() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("ai-os-live-1d-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    std::thread::spawn(move || service::run(listener, Box::new(|sink| {
        Engine::new(Store::open(DB).unwrap(), OllamaModel::local("qwen3.5:9b"), PathBuf::from("/data/projects"), Some(DB.into()),
            Box::new(|ws| (Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>, Box::new(AdminWorker) as Box<dyn Worker>)),
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

fn must_clear(path: &str, dir: bool) {
    let r = if dir { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = r { assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "could not clear {path}: {e}"); }
}

#[test]
fn live_1d() {
    if std::env::var("AI_OS_LIVE").is_err() { eprintln!("skipped: set AI_OS_LIVE=1"); return; }
    must_clear(DB, false);
    // The project folder is a btrfs subvolume: only the wrapper can remove it. A leftover fails
    // the run rather than silently reusing an old project.
    assert!(!std::path::Path::new(PROJECT_DIR).exists(), "clear {PROJECT_DIR} first (sudo /usr/local/libexec/ai-os-admin remove-dir {PROJECT_DIR})");
    let sock = start_service();
    let mut c = Client::connect(&sock).unwrap();
    c.hello().unwrap();
    assert_eq!(c.next_event(), Some(Event::State { job: None }), "a fresh db has no open job");

    // 1. A project job, decide-yourself, ends Done with files.
    let ev = say_until(&mut c, "make a new project called rail-live with a python script that prints the first five primes; decide everything yourself and do not ask", 400,
        |e| matches!(e, Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(&ev[0], Event::You { .. }));
    assert!(ev.iter().any(|e| matches!(e, Event::Understood { name, .. } if name == "rail-live")), "{ev:?}");
    assert!(ev.iter().any(|e| matches!(e, Event::Plan { .. })));
    assert!(ev.iter().filter(|e| matches!(e, Event::Step { .. })).count() >= 2);
    let Event::Done { files, .. } = ev.last().unwrap() else { panic!("the job did not finish: {:?}", ev.last()) };
    assert!(files.iter().any(|f| f.path.ends_with(".py")), "a python file among the changed files: {files:?}");
    assert!(files.iter().all(|f| f.path.starts_with(PROJECT_DIR)));

    // 2. A write under /etc needs the OK; a question there is answered and the OK asked again; yes runs it.
    must_clear(ETC_FILE, false);
    let ev = say_until(&mut c, &format!("in project rail-live, write the single word hello into the file {ETC_FILE}; decide everything yourself"), 400,
        |e| matches!(e, Event::NeedsOk { .. } | Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(ev.last().unwrap(), Event::NeedsOk { .. }), "expected an OK request for the /etc write: {:?}", ev.last());
    let ev = say_until(&mut c, "what exactly will you write there?", 120, |e| matches!(e, Event::NeedsOk { .. } | Event::Done { .. } | Event::Failed { .. }));
    assert!(ev.iter().any(|e| matches!(e, Event::Said { .. })), "the question was answered: {ev:?}");
    assert!(matches!(ev.last().unwrap(), Event::NeedsOk { .. }), "and the OK asked again: {ev:?}");
    let ev = say_until(&mut c, "yes", 400, |e| matches!(e, Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(ev.last().unwrap(), Event::Done { .. }), "{:?}", ev.last());
    assert_eq!(std::fs::read_to_string(ETC_FILE).unwrap().trim(), "hello");

    // 3. Undo puts the /etc file back (it did not exist) and reports it.
    let ev = say_until(&mut c, "undo", 120, |e| matches!(e, Event::Undone { .. } | Event::Said { .. }));
    let Event::Undone { lines, .. } = ev.last().unwrap() else { panic!("{ev:?}") };
    assert!(lines.iter().any(|l| l.ok), "{lines:?}");
    assert!(!std::path::Path::new(ETC_FILE).exists(), "the /etc file is gone again");

    // 4. Busy during a job, then stop lands.
    let mut b = Client::connect(&sock).unwrap();
    c.say("in project rail-live, add a script that prints the first hundred primes and a second one that prints the first hundred squares; decide everything yourself").unwrap();
    let t = Instant::now();
    loop { match c.next_event() { Some(Event::Plan { .. }) => break, Some(_) if t.elapsed() < Duration::from_secs(200) => {}, other => panic!("{other:?}") } }
    b.say("use bash instead").unwrap();
    let mut saw_busy = false;
    for _ in 0..50 { match b.next_event() { Some(Event::Busy { .. }) => { saw_busy = true; break } Some(Event::Done { .. }) => break, _ => {} } }
    assert!(saw_busy, "a message mid-job is answered with busy");
    b.say("stop").unwrap();
    let mut ended = None;
    let t = Instant::now();
    while let Some(e) = c.next_event() { if matches!(e, Event::Stopped { .. } | Event::Done { .. } | Event::Failed { .. }) { ended = Some(e); break; } assert!(t.elapsed().as_secs() < 300); }
    assert!(matches!(ended, Some(Event::Stopped { .. })), "stop landed within a step: {ended:?}");
}
