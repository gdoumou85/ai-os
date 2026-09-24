//! The engine as a service (1d design §3): one engine thread, one thread per client, JSON lines.
use crate::engine::{is_discard_learned, is_keep_learned, is_stop, Engine};
use crate::model::Model;
use aios_proto::{Event, JobState, Request, StepView, Waiting};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

/// `Clear`: pressed while a turn worked. The screen was cleared at once; this waits behind that
/// turn so its last rows are written first and then forgotten with the rest (final review,
/// 2026-09-24).
enum Command { Say(String), Clear }

/// Everything the client threads need without touching the engine.
struct Shared {
    clients: Mutex<Vec<(u64, Sender<String>)>>,
    seq: AtomicU64,
    /// The open job as the events described it: `hello` is answered from here, at once, even
    /// mid-job (the engine's own `state()` is only reachable between calls).
    mirror: Mutex<Option<JobState>>,
}

fn open(job_id: &str) -> JobState {
    JobState { id: job_id.into(), name: String::new(), housekeeping: false, understood: String::new(), plan: vec![], steps: vec![], waiting: Waiting::None }
}

impl Shared {
    /// To every client, numbered: `seq` counts broadcasts only, so each client's run is
    /// contiguous and a gap means a dropped line. Answers to one client (`send_to`) carry no seq.
    fn broadcast(&self, ev: &Event) {
        // The number and the send under one lock: two broadcasters that numbered their lines
        // first (a reader thread's `You`, the engine thread's `Step`) could take 11 and 12 and
        // then push in the other order, and every client would read 10, 12, 11. Nothing but
        // mpsc sends happen while it is held — no I/O.
        let mut clients = self.clients.lock().unwrap();
        let mut v = serde_json::to_value(ev).expect("event serialises");
        v["seq"] = serde_json::Value::from(self.seq.fetch_add(1, Ordering::SeqCst));
        let line = v.to_string();
        clients.retain(|(_, tx)| tx.send(line.clone()).is_ok());
    }
    fn send_to(&self, id: u64, ev: &Event) {
        let line = serde_json::to_string(ev).expect("event serialises");
        let mut clients = self.clients.lock().unwrap();
        if let Some(pos) = clients.iter().position(|(cid, _)| *cid == id) {
            if clients[pos].1.send(line).is_err() { clients.remove(pos); }
        }
    }
    fn update_mirror(&self, ev: &Event) {
        let mut m = self.mirror.lock().unwrap();
        match ev {
            // A turn's card opens on its first to-do list or action (one-loop design §3).
            Event::Plan { job_id, steps } => { m.get_or_insert_with(|| open(job_id)).plan = steps.clone(); }
            Event::Step { job_id, plan_step, text, ok } => { m.get_or_insert_with(|| open(job_id)).steps.push(StepView { plan_step: *plan_step, text: text.clone(), ok: *ok }); }
            Event::NeedsAnswer { .. } | Event::Done { .. } | Event::Failed { .. } | Event::Stopped { .. } => *m = None,
            _ => {}
        }
    }
}

/// Listen on the socket, private to this user. A socket something still answers on is a live
/// service: refuse rather than unlink it and run two engines on one database. A stale file
/// (nothing answers) is removed.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    if UnixStream::connect(path).is_ok() {
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, format!("an engine is already listening on {}", path.display())));
    }
    let _ = std::fs::remove_file(path);
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

/// Where the engine's database is: its notebooks are read from here by the Skills screen.
pub fn db_path() -> String { std::env::var("AI_OS_DB").unwrap_or_else(|_| "/data/ai-os.db".into()) }

/// The notebooks as the Skills screen draws them, read on the client's own thread so the screen
/// opens while a job runs (Phase 3 §6).
fn skills_event(forget: Option<(&str, &str)>) -> Event {
    let read = || -> Result<Vec<aios_proto::Notebook>, crate::store::StoreError> {
        let c = rusqlite::Connection::open(db_path())?;
        c.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::notes::init(&c)?;
        crate::machine::init(&c)?;
        if let Some((nb, tp)) = forget { crate::notes::remove(&c, nb, tp)?; }
        // What is on disk first, read-only (machine-map spec §4): kind `program` has no ✕.
        let program = |topic: String, text: String| aios_proto::NoteView { topic, kind: "program".into(), text, uses: 0, failed: false, needs_check: false };
        let mut entries: Vec<_> = crate::machine::apps_in(&executor::atspi::APP_DIRS).into_iter().map(|a| program(a.name, format!("open_app {}", a.id))).collect();
        entries.extend(crate::machine::installed(&c)?.into_iter().map(|(p, via)| program(p, format!("installed by the AI with {via}"))));
        let mut all = vec![aios_proto::Notebook { name: "installed on this computer".into(), entries }];
        all.extend(crate::notes::all(&c)?.into_iter().map(|(name, notes)| aios_proto::Notebook {
            name,
            entries: notes.into_iter().map(|n| aios_proto::NoteView { topic: n.topic, kind: n.kind, text: n.text, uses: n.uses, failed: n.failed, needs_check: n.needs_check }).collect(),
        }));
        Ok(all)
    };
    match read() { Ok(notebooks) => Event::Skills { notebooks }, Err(e) => Event::Error { text: format!("could not read the notebooks: {e}") } }
}

/// The projects as the Projects page draws them, read on the client's own thread so the page
/// opens while a job runs.
fn projects_event(default: &Path) -> Event {
    match crate::store::Store::open(&db_path()).and_then(|s| crate::projects::views(&s, default)) {
        Ok(projects) => Event::Projects { projects },
        Err(e) => Event::Error { text: format!("could not list the projects: {e}") },
    }
}

/// `$AI_OS_SOCKET_DIR/ai-os.sock` when set (tests, a second engine on purpose), else
/// `$XDG_RUNTIME_DIR/ai-os.sock` (a systemd user service and every shell in the distro have
/// it), else `/run/user/<uid>/ai-os.sock`.
pub fn socket_path() -> std::path::PathBuf {
    let dir = std::env::var("AI_OS_SOCKET_DIR").ok()
        .or_else(|| std::env::var("XDG_RUNTIME_DIR").ok())
        .unwrap_or_else(|| {
            use std::os::unix::fs::MetadataExt;
            let uid = std::fs::metadata("/proc/self").map(|m| m.uid()).unwrap_or(0);
            format!("/run/user/{uid}")
        });
    Path::new(&dir).join("ai-os.sock")
}

/// Serve forever. `make` builds the engine on the engine thread and receives the broadcast sink.
pub fn run<M: Model + 'static>(listener: UnixListener, make: Box<dyn FnOnce(Box<dyn FnMut(&Event)>) -> Engine<M> + Send>) -> ! {
    let shared = Arc::new(Shared { clients: Mutex::new(vec![]), seq: AtomicU64::new(1), mirror: Mutex::new(None) });
    let (tx, rx) = channel::<Command>();
    // The engine's stop flag, inbox and the projects' default root, handed back once the engine
    // exists. Nothing is accepted before they arrive: a client that could connect first would have
    // nothing to raise or fill, and its word would be lost without a trace.
    let (ready, engine_ready) = channel::<(Arc<AtomicBool>, crate::engine::Inbox, PathBuf)>();
    // Stop words sent but not yet taken by the engine thread. The flag belongs to the queue, not to
    // the moment: a "stop" typed behind a request that has not started must still stop it, and one
    // with nothing queued before it must not stop the next job. The engine thread sets the flag
    // from this count as it takes each command; a stop raises it and queues its word under the same
    // lock, so the two never interleave. Clearing it blindly after each command instead lost a stop
    // typed right after a request (CI, 2026-09-24: the engine cleared it after `resume`).
    let stops = Arc::new(Mutex::new(0usize));
    let sh = shared.clone();
    let engine_stops = stops.clone();
    std::thread::spawn(move || {
        // The engine thread IS the service. If it ever ends — a panic in the store, a poisoned
        // mutex, the command channel closing — the whole process must end with it: an accept loop
        // still running over a dead engine drops every client without a word, systemd's
        // `Restart=on-failure` never fires, and `bind` then refuses a manual restart because
        // something is still answering on the socket. A release build aborts on panic before this
        // is reached (runtime/Cargo.toml); this is the same end for a debug build, which unwinds.
        let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let sink_sh = sh.clone();
        let mut engine = make(Box::new(move |ev| { sink_sh.update_mirror(ev); sink_sh.broadcast(ev); }));
        // A store read that fails here would leave `hello` answering "nothing open" while a real
        // job resumes in front of the user: say so rather than swallow it.
        match engine.state() {
            Ok(st) => *sh.mirror.lock().unwrap() = st,
            Err(e) => eprintln!("engine: reading the open job failed: {e}"),
        }
        // Everything a client thread reads is now set: clients may arrive, and they watch the
        // resumed job go by like any other.
        let _ = ready.send((engine.stop_flag(), engine.inbox(), engine.default_root()));
        // At boot the engine starts before the network does (the owner's VM, 2026-09-19: "Network
        // is unreachable", and the open job left where it was). A resume the model runner could not
        // answer is tried again for a minute; anything else is reported once.
        for attempt in 1.. {
            match engine.resume() {
                Err(e) if attempt < 12 && e.to_string().contains("model http") => {
                    eprintln!("engine: resume waiting for the model runner ({e}); trying again in 5 s");
                    std::thread::sleep(std::time::Duration::from_secs(5));
                }
                Err(e) => { eprintln!("engine: resume failed: {e}"); break }
                Ok(_) => break,
            }
        }
        for cmd in rx {
            {
                let mut waiting = engine_stops.lock().unwrap();
                if matches!(&cmd, Command::Say(t) if is_stop(t)) { *waiting -= 1; }
                engine.stop_flag().store(*waiting > 0, Ordering::SeqCst);
            }
            let text = match cmd {
                Command::Say(text) => text,
                // `Cleared` already went out from the client thread.
                Command::Clear => {
                    if let Err(e) = engine.clear() { sh.broadcast(&Event::Error { text: format!("could not clear the chat: {e}") }); }
                    continue;
                }
            };
            let r = engine.handle_events(&text);
            if let Err(e) = r { sh.broadcast(&Event::Said { text: format!("(something went wrong: {e})") }); }
        }
        }));
        eprintln!("engine: the engine thread {} — exiting so the service is restarted", if ended.is_err() { "panicked" } else { "ended" });
        std::process::exit(1);
    });
    let (stop, inbox, root) = engine_ready.recv().expect("the engine thread builds the engine");
    let mut next_id = 1u64;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let id = next_id; next_id += 1;
        let (ctx, crx) = channel::<String>();
        shared.clients.lock().unwrap().push((id, ctx));
        // Writer: everything broadcast to this client, in order.
        let mut w = match stream.try_clone() { Ok(s) => s, Err(_) => continue };
        std::thread::spawn(move || { for line in crx { if w.write_all(line.as_bytes()).and_then(|_| w.write_all(b"\n")).is_err() { break; } } });
        // Reader: the client's requests.
        let sh = shared.clone();
        let tx = tx.clone();
        let stop = stop.clone();
        let stops = stops.clone();
        let inbox = inbox.clone();
        let root = root.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Request>(&line) {
                    Ok(Request::Hello(_)) => { let st = sh.mirror.lock().unwrap().clone(); sh.send_to(id, &Event::State { job: st }); }
                    // Keep/Discard answer the notes waiting on disk, never the turn at work: fed to
                    // it as words, the model saw them and the turn's own learning then threw the
                    // entry away (final review, 2026-09-24). Same answers as the engine's own branch.
                    Ok(Request::Say(text)) if is_keep_learned(&text) || is_discard_learned(&text) => {
                        sh.broadcast(&Event::You { text: text.clone() });
                        let keep = is_keep_learned(&text);
                        let done = rusqlite::Connection::open(db_path()).map_err(crate::store::StoreError::from).and_then(|c| {
                            c.busy_timeout(std::time::Duration::from_secs(5))?;
                            crate::notes::init(&c)?;
                            if keep { crate::notes::keep_pending(&c) } else { crate::notes::discard_pending(&c) }
                        });
                        match done {
                            Ok(n) => sh.broadcast(&Event::Said { text: match (n, keep) { (0, _) => "There is nothing waiting to be kept or discarded.", (_, true) => "Kept what I learned.", (_, false) => "Discarded it." }.into() }),
                            Err(e) => sh.send_to(id, &Event::Error { text: format!("could not reach the notes: {e}") }),
                        }
                    }
                    Ok(Request::Say(text)) => {
                        // Stop: the flag lands between actions; queued too, for a turn not yet begun.
                        if is_stop(&text) {
                            let mut waiting = stops.lock().unwrap();
                            *waiting += 1;
                            stop.store(true, Ordering::SeqCst);
                            sh.broadcast(&Event::You { text: text.clone() });
                            if tx.send(Command::Say(text)).is_err() { break; }
                            drop(waiting);
                            continue;
                        }
                        // Decide before the echo goes out (as with the stop flag above): a test —
                        // or the engine's own gate — that waits on `You` and then acts must already
                        // find the word either in the inbox or on its way as a new turn, never in
                        // the gap between the two.
                        let joined = match inbox.lock().unwrap().as_mut() { Some(v) => { v.push(text.clone()); true } None => false };
                        sh.broadcast(&Event::You { text: text.clone() });
                        // While a turn works, the words join it at its next step (one-loop design §3);
                        // otherwise they are the next turn.
                        if !joined && tx.send(Command::Say(text)).is_err() { break; }
                    }
                    Ok(Request::Skills {}) => sh.send_to(id, &skills_event(None)),
                    Ok(Request::Forget { notebook, topic }) => sh.send_to(id, &skills_event(Some((&notebook, &topic)))),
                    Ok(Request::Projects {}) => sh.send_to(id, &projects_event(&root)),
                    // The Watchers page and `ai-os-alert` land in Task 5 (watchers design §2); for
                    // now these requests reach the service and are accepted but do nothing.
                    Ok(Request::Watchers {}) | Ok(Request::WatcherPause { .. }) | Ok(Request::WatcherDelete { .. }) | Ok(Request::Alert { .. }) => {}
                    // A turn open (the inbox exists): its inbox closes and the words in it go (the
                    // user cleared them), the flag lands between its steps as "stop" would, and the
                    // screen empties now; the engine forgets the rows once that turn has ended
                    // (`Command::Clear`). Words typed after this find no inbox, so they queue behind
                    // the Clear and run in the fresh chat. A question already ended its own turn,
                    // so then nothing is running to stop.
                    Ok(Request::Clear {}) if inbox.lock().unwrap().take().is_some() => {
                        stop.store(true, Ordering::SeqCst);
                        if tx.send(Command::Clear).is_err() { break; }
                        sh.broadcast(&Event::Cleared {});
                    }
                    // Nothing running: on this thread at once, like the Skills screen.
                    Ok(Request::Clear {}) => match crate::store::Store::open(&db_path()).and_then(|s| s.forget_chat()) {
                        Ok(()) => sh.broadcast(&Event::Cleared {}),
                        Err(e) => sh.send_to(id, &Event::Error { text: format!("could not clear the chat: {e}") }),
                    },
                    Err(_) => sh.send_to(id, &Event::Error { text: "could not read that message".into() }),
                }
            }
            sh.clients.lock().unwrap().retain(|(cid, _)| *cid != id);
        });
    }
    unreachable!("listener.incoming() never ends")
}
