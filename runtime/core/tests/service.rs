//! Two clients on a temp socket, a scripted model, no display (1d design §7 item 2).
use aios_core::engine::Engine;
use aios_core::model::FakeModel;
use aios_core::moves::Move;
use aios_core::service;
use aios_core::store::Store;
use aios_core::testing::{scripted_workers, Recorder};
use aios_proto::{Client, Event};
use executor::action::Action;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ai-os-svc-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d); std::fs::create_dir_all(&d).unwrap(); d
}

/// The on-disk database `Skills`/`Clear` read (`AI_OS_DB`), set once for the whole test binary.
/// Cargo runs every `#[test]` here as a thread of one process, and `db_path()` reads the env var
/// fresh on every request — two tests each pointing it at their own path would race. `forget_chat`
/// never touches notebooks, so sharing it with the Skills test's own note is safe.
fn test_db() -> &'static PathBuf {
    static DB: OnceLock<PathBuf> = OnceLock::new();
    DB.get_or_init(|| {
        let db = temp("db").join("notes.db");
        std::env::set_var("AI_OS_DB", &db);
        db
    })
}

/// The tests that work on the shared database take turns: one's Clear would wipe the rows another
/// is about to read (final review, 2026-09-24).
fn db_turn() -> std::sync::MutexGuard<'static, ()> {
    static TURN: Mutex<()> = Mutex::new(());
    TURN.lock().unwrap_or_else(|e| e.into_inner())
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
    start_with(dir, moves, gate, false)
}

/// `on_disk`: the engine keeps its chat in the shared test database, as the real service does, so
/// a test can read what a Clear left behind.
fn start_with(dir: &PathBuf, moves: Vec<Move>, gate: Arc<Mutex<Option<std::sync::mpsc::Receiver<()>>>>, on_disk: bool) -> PathBuf {
    let db = on_disk.then(|| test_db().to_str().unwrap().to_string());
    let sock = dir.join("ai-os.sock");
    let listener = service::bind(&sock).unwrap();
    let dir = dir.clone();
    std::thread::spawn(move || {
        service::run(listener, Box::new(move |sink| {
            let rec = Recorder::default();
            // The projects root is its own folder, apart from home: home living under it would
            // make every plain write there a "project change" the engine expects notes for, and
            // a script with no move to spare for the reminder hangs the whole test (2026-09-24).
            let projects = dir.join("projects");
            std::fs::create_dir_all(&projects).unwrap();
            std::fs::create_dir_all(dir.join("hk")).unwrap();
            std::fs::create_dir_all(dir.join("home")).unwrap();
            let store = match &db { Some(p) => Store::open(p).unwrap(), None => Store::open_in_memory().unwrap() };
            Engine::new(store, GatedModel { inner: Mutex::new(FakeModel::new(moves)), gate }, projects, None, scripted_workers(&rec), dir.join("hk")).with_home(dir.join("home")).with_sink(sink)
        }))
    });
    let t = Instant::now();
    while !sock.exists() && t.elapsed() < Duration::from_secs(5) { std::thread::sleep(Duration::from_millis(20)); }
    sock
}

/// Every test connects through here: a read that waits longer than 30s closes the connection
/// instead of hanging CI on a mismatch. The option is on the socket (SO_RCVTIMEO), which the
/// cloned fd a `Reader` uses shares — set once here, before a possible `split()`, it bounds both
/// halves, not just `Client::next_event`.
fn connect(sock: &Path) -> Client {
    let c = Client::connect(sock).unwrap();
    c.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    c
}

/// A mismatch fails in 30s instead of hanging CI (the timeout is set once, in `connect` above).
fn until(c: &mut Client, pred: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut got = vec![];
    while let Some(e) = c.next_event() { let stop = pred(&e); got.push(e); if stop { return got; } }
    panic!("connection closed before the event: {got:?}");
}

fn write(p: &str) -> Action { Action::WriteFile { path: p.into(), contents: "x".into() } }
fn job() -> Vec<Move> { vec![
    Move::Act { thought: String::new(), action: write("a.txt") },
    Move::Reply { thought: String::new(), text: "finished".into(), outcome: aios_core::moves::Ending::Done },
] }

#[test]
fn both_clients_see_every_event_in_order_with_contiguous_seq() {
    let dir = temp("two");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut a = connect(&sock);
    let mut b = connect(&sock);
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
    assert!(matches!(flow[1], Event::Step { .. }), "{flow:?}");
}

#[test]
fn seq_is_contiguous_on_the_wire() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("seq");
    let sock = start(&dir, job(), Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
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
fn words_sent_during_work_join_it_instead_of_busy() {
    let dir = temp("inbox");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    // The reply after the words is not the end: they join the turn, so the model moves once more.
    let reply = |t: &str| Move::Reply { thought: String::new(), text: t.into(), outcome: aios_core::moves::Ending::Done };
    let moves = vec![Move::Act { thought: String::new(), action: write("a.txt") }, reply("one"), reply("finished")];
    let sock = start(&dir, moves, Arc::new(Mutex::new(Some(grx))));
    let mut a = connect(&sock);
    a.hello().unwrap();
    assert_eq!(a.next_event(), Some(Event::State { job: None }));
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); // the Act
    until(&mut a, |e| matches!(e, Event::Step { .. }));
    // The engine now waits at the gate before its next move: the turn is open.
    let mut b = connect(&sock);
    b.hello().unwrap();
    let got = until(&mut b, |e| matches!(e, Event::State { .. }));
    assert!(matches!(got.last(), Some(Event::State { job: Some(st) }) if st.steps.len() == 1), "{got:?}");
    b.say("use python").unwrap();
    let got = until(&mut b, |e| matches!(e, Event::You { .. }));
    assert!(!got.iter().any(|e| matches!(e, Event::Busy { text, .. } if text.contains("Say stop"))), "{got:?}");
    for _ in 0..3 { gtx.send(()).unwrap(); } // reply "one", reply "finished", the learning turn
    let got = until(&mut a, |e| matches!(e, Event::Done { .. }));
    assert!(matches!(got.last(), Some(Event::Done { text, .. }) if text == "finished"), "{got:?}");
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
    let mut a = connect(&sock);
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); // Act
    until(&mut a, |e| matches!(e, Event::Step { .. }));
    let mut b = connect(&sock);
    b.say("stop").unwrap();
    drop(b);
    // The stop must have reached the service before the model answers: with nothing left to run
    // after the Reply, a gate opened first would let the turn finish ahead of it.
    until(&mut a, |e| matches!(e, Event::You { text } if text == "stop"));
    gtx.send(()).unwrap(); // lets the model answer the Reply; the flag is checked before it is acted on
    let got = until(&mut a, |e| matches!(e, Event::Stopped { .. } | Event::Done { .. }));
    assert!(matches!(got.last().unwrap(), Event::Stopped { .. }), "{got:?}");
}

#[test]
fn a_bad_line_gets_error_and_nothing_else() {
    use std::io::{BufRead, BufReader, Write};
    let dir = temp("bad");
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
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
    let mut a = connect(&sock);
    a.hello().unwrap(); a.next_event();
    a.say("make p").unwrap();
    a.say("stop").unwrap();
    gtx.send(()).unwrap(); gtx.send(()).unwrap(); // Act, Reply
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
    let _turn = db_turn();
    let dir = temp("skills");
    let db = test_db();
    {
        let c = rusqlite::Connection::open(db).unwrap();
        aios_core::notes::init(&c).unwrap();
        aios_core::notes::put(&c, &aios_core::notes::Note { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox".into(), ..Default::default() }, None).unwrap();
    }
    let sock = start(&dir, vec![], Arc::new(Mutex::new(None)));
    let (mut r, mut w) = connect(&sock).split();
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
    // Clear works on the same database as every other test (test_db() above): forget_chat only
    // touches messages/instructions, never the notebook this test just put and forgot.
    let s = Store::open(db.to_str().unwrap()).unwrap();
    s.push_message("user", "open blender").unwrap();
    s.add_instruction("Blender is installed; new projects need a Models folder").unwrap();
    w.request(&aios_proto::Request::Clear {}).unwrap();
    assert_eq!(r.next_event(), Some(Event::Cleared {}));
    assert!(s.recent_messages(4).unwrap().is_empty());
    assert!(s.instructions().unwrap().is_empty(), "Clear forgets what it was told to keep too");
}

#[test]
fn clear_during_work_stops_it_then_forgets_every_row() {
    let _turn = db_turn();
    let dir = temp("clear-work");
    let db = test_db();
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start_with(&dir, job(), Arc::new(Mutex::new(Some(grx))), true);
    let (mut r, mut w) = connect(&sock).split();
    w.request(&aios_proto::Request::Say("make p".into())).unwrap();
    gtx.send(()).unwrap();
    while !matches!(r.next_event(), Some(Event::Step { .. }) | None) {}
    w.request(&aios_proto::Request::Clear {}).unwrap();
    // One reader thread takes a client's requests in order: the hello answered means the Clear
    // before it has armed the stop flag, so opening the gate next cannot race past the stop check.
    w.hello().unwrap();
    while !matches!(r.next_event(), Some(Event::State { .. }) | None) {}
    gtx.send(()).unwrap();
    // Cleared comes after Stopped now: the stopped turn writes its last rows first (final review).
    let mut got = vec![];
    while let Some(e) = r.next_event() { let end = matches!(e, Event::Cleared {}); got.push(e); if end { break; } }
    let ends: Vec<&Event> = got.iter().filter(|e| matches!(e, Event::Stopped { .. } | Event::Done { .. } | Event::Cleared {})).collect();
    assert!(matches!(&ends[..], [Event::Stopped { .. }, Event::Cleared {}]), "{got:?}");
    let rows = Store::open(db.to_str().unwrap()).unwrap().all_messages().unwrap();
    assert!(rows.is_empty(), "nothing of the stopped turn leaks into the fresh chat: {rows:?}");
}

#[test]
fn discard_sent_during_work_answers_the_notes_and_never_joins_the_turn() {
    let _turn = db_turn();
    let db = test_db();
    {
        let c = rusqlite::Connection::open(db).unwrap();
        aios_core::notes::init(&c).unwrap();
        let n = aios_core::notes::Note { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox".into(), ..Default::default() };
        aios_core::notes::put(&c, &n, Some("an-older-job")).unwrap();
    }
    let dir = temp("discard-work");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, job(), Arc::new(Mutex::new(Some(grx))));
    let mut a = connect(&sock);
    a.say("make p").unwrap();
    gtx.send(()).unwrap(); // the Act
    until(&mut a, |e| matches!(e, Event::Step { .. }));
    a.say("discard what you learned").unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Said { .. }));
    assert!(matches!(got.last(), Some(Event::Said { text }) if text == "Discarded it."), "{got:?}");
    // Had the words joined the turn, the model would be asked once more and find no move left.
    gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Done { .. } | Event::Failed { .. }));
    assert!(matches!(got.last(), Some(Event::Done { text, .. }) if text == "finished"), "{got:?}");
}

#[test]
fn a_message_while_only_a_chat_reply_is_in_progress_is_queued_not_busy() {
    let dir = temp("chat-queue");
    let (gtx, grx) = std::sync::mpsc::channel::<()>();
    let sock = start(&dir, vec![Move::Reply { thought: String::new(), text: "one".into(), outcome: aios_core::moves::Ending::Done }, Move::Reply { thought: String::new(), text: "two".into(), outcome: aios_core::moves::Ending::Done }], Arc::new(Mutex::new(Some(grx))));
    let mut a = connect(&sock);
    a.hello().unwrap(); a.next_event();
    a.say("hi").unwrap();
    a.say("hi again").unwrap(); // the engine is inside the first reply (gated) — no job, so no busy
    gtx.send(()).unwrap(); gtx.send(()).unwrap();
    let got = until(&mut a, |e| matches!(e, Event::Said { text } if text == "two"));
    assert!(!got.iter().any(|e| matches!(e, Event::Busy { text, .. } if text.contains("Say stop"))), "{got:?}");
}
