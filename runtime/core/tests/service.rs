//! Two clients on a temp socket, a scripted model, no display (1d design §7 item 2).
use aios_core::engine::Engine;
use aios_core::model::FakeModel;
use aios_core::moves::Move;
use aios_core::service;
use aios_core::store::Store;
use aios_core::testing::{scripted_workers, Recorder};
use aios_proto::{Client, Event, Waiting};
use executor::action::Action;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ai-os-svc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
}

/// A model whose moves can be refilled from the test and that blocks on a gate so the test can
/// act *while* the engine is inside a job.
struct GatedModel { inner: Mutex<FakeModel>, gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>> }
impl aios_core::model::Model for GatedModel {
    fn next_move(&self, p: &aios_core::model::Prompt) -> Result<Move, aios_core::model::ModelError> {
        if let Some(rx) = self.gate.lock().unwrap().as_ref() { let _ = rx.recv_timeout(Duration::from_secs(5)); }
        self.inner.lock().unwrap().next_move(p)
    }
}

fn start(dir: &PathBuf, moves: Vec<Move>, gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>) -> PathBuf {
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    let root = dir.clone();
    std::thread::spawn(move || {
        service::run(listener, Box::new(move |sink| {
            let rec = Recorder::default();
            std::fs::create_dir_all(root.join("hk")).unwrap();
            Engine::new(Store::open_in_memory().unwrap(), GatedModel { inner: Mutex::new(FakeModel::new(moves)), gate }, root.clone(), None, scripted_workers(&rec), root.join("hk")).with_sink(sink)
        }))
    });
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

fn until(c: &mut Client, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut got = vec![];
    while let Some(e) = c.next_event() { let stop = pred(&e); got.push(e); if stop { return got; } }
    panic!("connection closed before the event: {got:?}");
}

fn write(p: &str) -> Action { Action::WriteFile { path: p.into(), contents: "x".into() } }
fn job() -> Vec<Move> { vec![
    Move::Start { project: "p".into(), new_project: true, description: "d".into(), goal: "g".into(), creative: true, understood: "Starting p".into(), skills: vec![], remember: None, folder: None },
    Move::Plan { steps: vec!["write".into()] },
    Move::Act { step: 1, action: write("BLUEPRINT.md") },
    Move::Done { summary: "finished".into(), check: Action::RunCommand { argv: vec!["true".into()] } },
] }

#[test]
fn both_clients_see_every_event_in_order_with_contiguous_seq() {
    let dir = temp("two");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut a = Client::connect(&sock).unwrap();
    let mut b = Client::connect(&sock).unwrap();
    // `connect` returns before the service has registered the client: a hello/state round trip
    // on each proves both are registered before anything is broadcast.
    a.hello().unwrap(); assert!(matches!(a.next_event(), Some(Event::State { .. })));
    b.hello().unwrap(); assert!(matches!(b.next_event(), Some(Event::State { .. })));
    a.say("make p").unwrap();
    let ea = until(&mut a, |e| matches!(e, Event::Done { .. }));
    let eb = until(&mut b, |e| matches!(e, Event::Done { .. }));
    assert_eq!(ea, eb);
    // The status line (engine `tick`) comes through as `busy` between these: it says what the
    // AI is doing this second, not what happened, so the flow of the job reads without it.
    let flow: Vec<&Event> = ea.iter().filter(|e| !matches!(e, Event::Busy { .. })).collect();
    assert!(matches!(flow[0], Event::You { text } if text == "make p"));
    assert!(matches!(flow[1], Event::Understood { .. }));
}

#[test]
fn seq_is_contiguous_on_the_wire() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("seq");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    s.write_all(b"{\"say\":\"make p\"}\n").unwrap();
    let mut seqs = vec![];
    loop {
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v.as_object().unwrap().keys().next().unwrap(), "kind", "kind is the first key: {line}");
        seqs.push(v["seq"].as_u64().unwrap());
        if v["kind"] == "done" { break; }
    }
    for w in seqs.windows(2) { assert_eq!(w[1], w[0] + 1, "{seqs:?}"); }
}

#[test]
fn hello_answers_state_and_busy_is_answered_during_a_job() {
    let dir = temp("busy");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(grx)));
    let sock = start(&dir, job(), gate.clone());
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap();
    assert_eq!(a.next_event(), Some(Event::State { job: None }));
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); // Start
    gtx.send(()).unwrap(); // Plan
    until(&mut a, |e| matches!(e, Event::Plan { .. }));
    // The engine is now blocked inside the job (waiting for the gate before Act).
    let mut b = Client::connect(&sock).unwrap();
    b.hello().unwrap();
    // b is registered on connect, so the engine's status line (a busy tick) can beat the state.
    let got = until(&mut b, |e| matches!(e, Event::State { .. }));
    let Some(Event::State { job: Some(st) }) = got.last().cloned() else { panic!("{got:?}") };
    assert_eq!((st.name.as_str(), st.plan.len(), st.waiting), ("p", 1, Waiting::None));
    b.say("use python").unwrap();
    until(&mut b, |e| matches!(e, Event::You { .. }));
    until(&mut b, |e| matches!(e, Event::Busy { text, .. } if text.contains("working on p")));
    gtx.send(()).unwrap(); gtx.send(()).unwrap(); // Act, Done
    until(&mut a, |e| matches!(e, Event::Done { .. }));
    b.hello().unwrap();
    let got = until(&mut b, |e| matches!(e, Event::State { .. }));
    assert_eq!(got.last(), Some(&Event::State { job: None }));
}

#[test]
fn stop_during_a_job_lands_and_a_dropped_client_changes_nothing() {
    let dir = temp("stop");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let gate = Arc::new(Mutex::new(Some(grx)));
    let sock = start(&dir, job(), gate);
    let mut a = Client::connect(&sock).unwrap();
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); gtx.send(()).unwrap();
    until(&mut a, |e| matches!(e, Event::Plan { .. }));
    let mut b = Client::connect(&sock).unwrap();
    b.say("stop").unwrap();
    drop(b);
    gtx.send(()).unwrap(); // lets the model answer the Act; the flag is checked before the next turn
    gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Stopped { .. } | Event::Done { .. }));
    assert!(matches!(got.last().unwrap(), Event::Stopped { .. }), "{got:?}");
}

#[test]
fn a_bad_line_gets_error_and_nothing_else() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("bad");
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.write_all(b"not json\n").unwrap();
    let mut line = String::new();
    BufReader::new(s.try_clone().unwrap()).read_line(&mut line).unwrap();
    let v: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["kind"], "error");
    assert!(v.get("seq").is_none(), "seq is on broadcasts only, so every client's run of seq is contiguous");
}

#[test]
fn stop_typed_right_after_a_request_still_stops_it() {
    // The stop arrives before the engine has even picked the request up: it must be queued AND
    // arm the flag, or it would sit behind the whole job.
    let dir = temp("early-stop");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, job(), Arc::new(Mutex::new(Some(grx))));
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap(); a.next_event();
    a.say("make p").unwrap();
    a.say("stop").unwrap();
    gtx.send(()).unwrap(); gtx.send(()).unwrap(); gtx.send(()).unwrap(); gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Stopped { .. } | Event::Done { .. } | Event::Said { .. }));
    assert!(matches!(got.last().unwrap(), Event::Stopped { .. }), "{got:?}");
}

#[test]
fn bind_refuses_a_live_socket_and_removes_a_stale_file() {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    // Something answers on it: a second engine on one database is never what the user meant.
    let live = start(&temp("bind-live"), vec![], Arc::new(Mutex::new(None)));
    let err = service::bind(&live).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    // Nothing answers: whatever is in the way goes, and the new socket is this user's alone.
    let stale = temp("bind-stale").join("ai-os.sock");
    std::fs::write(&stale, "left behind by a killed engine").unwrap();
    let listener = service::bind(&stale).unwrap();
    let md = std::fs::metadata(&stale).unwrap();
    assert!(md.file_type().is_socket(), "the stale file was not replaced by a socket");
    assert_eq!(md.permissions().mode() & 0o777, 0o600);
    drop(listener);
}

#[test]
fn the_skills_screen_is_answered_from_the_database_and_forget_deletes() {
    let dir = temp("skills");
    let db = dir.join("notes.db");
    std::env::set_var("AI_OS_DB", &db);
    {
        let c = rusqlite::Connection::open(&db).unwrap();
        aios_core::notes::init(&c).unwrap();
        aios_core::notes::put(&c, &aios_core::notes::Note { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox".into(), ..Default::default() }, None).unwrap();
    }
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let (mut r, mut w) = Client::connect(&sock).unwrap().split();
    w.request(&aios_proto::Request::Skills {}).unwrap();
    let Some(Event::Skills { notebooks }) = r.next_event() else { panic!("no skills event") };
    // What is on disk comes first, read-only (machine-map spec §4).
    assert_eq!(notebooks[0].name, "installed on this computer", "{notebooks:?}");
    assert!(notebooks[0].entries.iter().all(|n| n.kind == "program"));
    assert_eq!(notebooks[1].name, "this computer");
    assert_eq!(notebooks[1].entries[0].topic, "open a website");
    w.request(&aios_proto::Request::Forget { notebook: "this computer".into(), topic: "open a website".into() }).unwrap();
    let Some(Event::Skills { notebooks }) = r.next_event() else { panic!("no skills event after forget") };
    assert!(notebooks.iter().all(|n| n.name == "installed on this computer"), "{notebooks:?}");
    // Clear works on the same database (here, not its own test: AI_OS_DB is one per process).
    let s = Store::open(db.to_str().unwrap()).unwrap();
    s.push_message("user", "open blender").unwrap();
    s.add_instruction("Blender is installed; new projects need a Models folder").unwrap();
    w.request(&aios_proto::Request::Clear {}).unwrap();
    assert_eq!(r.next_event(), Some(Event::Cleared {}));
    assert!(s.recent_messages(4).unwrap().is_empty());
    assert!(s.instructions().unwrap().is_empty(), "Clear forgets what it was told to keep too");
}

#[test]
fn clear_ends_a_job_waiting_on_a_question() {
    // The owner's run, 2026-09-23: Clear left the car-rental question waiting, and the next "Hi"
    // went to it as the answer. Cleared or not (AI_OS_DB is one per process), the job must stop.
    let dir = temp("clear-job");
    let sock = start(&dir, vec![
        Move::Start { project: "p".into(), new_project: true, description: "d".into(), goal: "g".into(), creative: false, understood: "Starting p".into(), skills: vec![], remember: None, folder: None },
        Move::Ask { questions: vec!["Which stack?".into()], options: vec![] },
    ], Arc::new(Mutex::new(None)));
    let (mut r, mut w) = Client::connect(&sock).unwrap().split();
    w.request(&aios_proto::Request::Say("make p".into())).unwrap();
    while !matches!(r.next_event(), Some(Event::NeedsAnswer { .. }) | None) {}
    w.request(&aios_proto::Request::Clear {}).unwrap();
    let mut got = vec![];
    while let Some(e) = r.next_event() { let end = matches!(e, Event::Stopped { .. }); got.push(e); if end { break; } }
    assert!(matches!(got.last(), Some(Event::Stopped { .. })), "{got:?}");
}

#[test]
fn a_message_while_only_a_chat_reply_is_in_progress_is_queued_not_busy() {
    let dir = temp("chat-queue");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, vec![Move::Reply { text: "one".into(), remember: None }, Move::Reply { text: "two".into(), remember: None }], Arc::new(Mutex::new(Some(grx))));
    let mut a = Client::connect(&sock).unwrap();
    a.hello().unwrap(); a.next_event();
    a.say("hi").unwrap();
    a.say("hi again").unwrap(); // the engine is inside the first reply (gated) — no job, so no busy
    gtx.send(()).unwrap(); gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Said { text } if text == "two"));
    assert!(!got.iter().any(|e| matches!(e, Event::Busy { text, .. } if text.contains("Say stop"))), "{got:?}");
}
