//! The engine as a service (1d design §3): one engine thread, one thread per client, JSON lines.
use crate::engine::{is_stop, Engine};
use crate::model::Model;
use aios_proto::{Event, JobState, Request, StepView, Waiting};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};

enum Command { Say(String) }

/// Everything the client threads need without touching the engine.
struct Shared {
    clients: Mutex<Vec<(u64, Sender<String>)>>,
    seq: AtomicU64,
    /// True while the engine is inside `handle_events`/`resume` — a job is being worked.
    running: AtomicBool,
    /// The open job as the events described it: `hello` is answered from here, at once, even
    /// mid-job (the engine's own `state()` is only reachable between calls).
    mirror: Mutex<Option<JobState>>,
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
            Event::Understood { job_id, name, text, housekeeping } => *m = Some(JobState { id: job_id.clone(), name: name.clone(), housekeeping: *housekeeping, understood: text.clone(), plan: vec![], steps: vec![], waiting: Waiting::None }),
            Event::Plan { steps, .. } => if let Some(j) = m.as_mut() { j.plan = steps.clone(); j.waiting = Waiting::None; },
            Event::Step { plan_step, text, ok, .. } => if let Some(j) = m.as_mut() { j.steps.push(StepView { plan_step: *plan_step, text: text.clone(), ok: *ok }); j.waiting = Waiting::None; },
            Event::NeedsAnswer { questions, options, .. } => if let Some(j) = m.as_mut() { j.waiting = Waiting::Answer { questions: questions.clone(), options: options.clone() }; },
            Event::NeedsOk { what, why, .. } => if let Some(j) = m.as_mut() { j.waiting = Waiting::Ok { what: what.clone(), why: why.clone() }; },
            Event::Done { .. } | Event::Failed { .. } | Event::Stopped { .. } => *m = None,
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
    let shared = Arc::new(Shared { clients: Mutex::new(vec![]), seq: AtomicU64::new(1), running: AtomicBool::new(false), mirror: Mutex::new(None) });
    let (tx, rx) = channel::<Command>();
    // The engine's stop flag, handed back once the engine exists. Nothing is accepted before it
    // arrives: a client that could connect first would have nothing to raise, and its stop
    // would be lost without a word.
    let (ready, engine_ready) = channel::<Arc<AtomicBool>>();
    let sh = shared.clone();
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
        sh.running.store(true, Ordering::SeqCst);
        // Everything a client thread reads is now set: clients may arrive, and they watch the
        // resumed job go by like any other.
        let _ = ready.send(engine.stop_flag());
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
        sh.running.store(false, Ordering::SeqCst);
        engine.stop_flag().swap(false, Ordering::SeqCst);
        for cmd in rx {
            let Command::Say(text) = cmd;
            sh.running.store(true, Ordering::SeqCst);
            let r = engine.handle_events(&text);
            sh.running.store(false, Ordering::SeqCst);
            // Silent: the queued word is what answers the user (the engine says "Nothing is
            // running now." itself, in every state). This only makes sure a flag nothing
            // consumed cannot survive into the next job.
            engine.stop_flag().swap(false, Ordering::SeqCst);
            if let Err(e) = r { sh.broadcast(&Event::Said { text: format!("(something went wrong: {e})") }); }
        }
        }));
        eprintln!("engine: the engine thread {} — exiting so the service is restarted", if ended.is_err() { "panicked" } else { "ended" });
        std::process::exit(1);
    });
    let stop = engine_ready.recv().expect("the engine thread builds the engine");
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
        std::thread::spawn(move || {
            for line in BufReader::new(stream).lines() {
                let Ok(line) = line else { break };
                if line.trim().is_empty() { continue; }
                match serde_json::from_str::<Request>(&line) {
                    Ok(Request::Hello(_)) => { let st = sh.mirror.lock().unwrap().clone(); sh.send_to(id, &Event::State { job: st }); }
                    Ok(Request::Say(text)) => {
                        sh.broadcast(&Event::You { text: text.clone() });
                        // Stop: arm the flag (lands between steps if a job is running) AND queue
                        // it (a stop typed before the engine picked the request up must not sit
                        // behind the whole job; an idle engine answers it as today). The
                        // engine thread clears a flag nothing consumed.
                        if is_stop(&text) {
                            stop.store(true, Ordering::SeqCst);
                            if tx.send(Command::Say(text)).is_err() { break; }
                            continue;
                        }
                        // Busy only while a JOB is being worked (§3.3): a chat reply in progress
                        // just queues the next message behind it, and so does a job that is
                        // waiting for an answer or an OK. `running` alone is not enough for
                        // that second one: it only falls once `handle_events` returns, which is
                        // after the `needs_ok` has already reached the client, so a question
                        // typed the instant the Needs-your-OK card appears would race it and
                        // come back "busy" — which is the one thing §2.1 promises it is not.
                        // The mirror does not race: the sink updates it before it broadcasts.
                        let job = sh.mirror.lock().unwrap().as_ref()
                            .filter(|j| j.waiting == Waiting::None)
                            .map(|j| (j.id.clone(), j.name.clone()));
                        match job {
                            Some((job_id, name)) if sh.running.load(Ordering::SeqCst) => {
                                sh.send_to(id, &Event::Busy { job_id, text: format!("I'm working on {name}. Say stop if you want me to change course.") });
                            }
                            _ => if tx.send(Command::Say(text)).is_err() { break; },
                        }
                    }
                    Err(_) => sh.send_to(id, &Event::Error { text: "could not read that message".into() }),
                }
            }
            sh.clients.lock().unwrap().retain(|(cid, _)| *cid != id);
        });
    }
    unreachable!("listener.incoming() never ends")
}
