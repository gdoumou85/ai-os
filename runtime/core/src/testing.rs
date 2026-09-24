//! Test-only helpers: a worker whose outcomes are scripted and whose calls are shared.
use executor::action::Action;
use executor::worker::{Outcome, Worker};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Recorder {
    /// The machine hand's calls (commands and files).
    pub calls: Rc<RefCell<Vec<Action>>>,
    /// Outcomes handed out in order; when empty, everything succeeds with "ok".
    pub outcomes: Rc<RefCell<VecDeque<Outcome>>>,
    /// The desktop hand's own lists, same shape.
    pub desktop_calls: Rc<RefCell<Vec<Action>>>,
    pub desktop_outcomes: Rc<RefCell<VecDeque<Outcome>>>,
}

/// The machine hand. It really writes inside the job's folder: the blueprint gate asks the
/// filesystem whether BLUEPRINT.md exists, so a worker that only records would let every
/// new-project test pass for the wrong reason. Nothing outside the folder is touched.
pub struct ScriptedWorker {
    pub rec: Recorder,
    pub ws: PathBuf,
}

impl ScriptedWorker {
    /// Where a path lands inside the workspace, or `None` if it leaves it — a test must never
    /// write to the real machine. A project lives outside the job's own folder (Task 8: the
    /// engine always works in the home folder, a project is elsewhere under the same test's
    /// root), so anywhere under this process's own temp dir is allowed too.
    fn target(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);
        if p.components().any(|c| c == Component::ParentDir) {
            return None;
        }
        let joined = if p.is_absolute() { p.to_path_buf() } else { self.ws.join(p) };
        if joined.starts_with(&self.ws) || joined.starts_with(std::env::temp_dir()) { Some(joined) } else { None }
    }
}

impl Worker for ScriptedWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::WriteFile { path, contents } => {
                if let Some(t) = self.target(path) {
                    if let Some(parent) = t.parent() { let _ = std::fs::create_dir_all(parent); }
                    let _ = std::fs::write(&t, contents);
                }
            }
            Action::EditFile { path, find, replace } => {
                if let Some(t) = self.target(path) {
                    if let Ok(old) = std::fs::read_to_string(&t) { let _ = std::fs::write(&t, old.replace(find, replace)); }
                }
            }
            Action::AppendFile { path, contents } => {
                if let Some(t) = self.target(path) {
                    if let Some(parent) = t.parent() { let _ = std::fs::create_dir_all(parent); }
                    let old = std::fs::read_to_string(&t).unwrap_or_default();
                    let _ = std::fs::write(&t, old + contents);
                }
            }
            _ => {}
        }
        self.rec.calls.borrow_mut().push(action.clone());
        self.rec.outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("ok"))
    }
}

/// The desktop hand: records only — no bus in a unit test.
pub struct DesktopRecorder(pub Recorder);

impl Worker for DesktopRecorder {
    fn run(&self, action: &Action) -> Outcome {
        self.0.desktop_calls.borrow_mut().push(action.clone());
        self.0.desktop_outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("ok"))
    }
}

/// Both lanes over one recorder, one pair per workspace the engine asks for.
pub fn scripted_workers(rec: &Recorder) -> crate::engine::WorkerFactory {
    let rec = rec.clone();
    Box::new(move |ws| (
        Box::new(ScriptedWorker { rec: rec.clone(), ws: ws.to_path_buf() }) as Box<dyn Worker>,
        Box::new(DesktopRecorder(rec.clone())) as Box<dyn Worker>,
    ))
}

/// Names a test's root without touching disk — for a move that must mention the root before
/// `engine_with` has created it.
pub fn temp_root_path(tag: &str) -> PathBuf {
    std::env::temp_dir().join(format!("ai-os-core-{tag}-{}", std::process::id()))
}

pub fn temp_root(tag: &str) -> PathBuf {
    let root = temp_root_path(tag);
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn engine_with(moves: Vec<crate::moves::Move>, tag: &str) -> (crate::engine::Engine<crate::model::FakeModel>, Recorder, PathBuf) {
    engine_with_model(crate::model::FakeModel::new(moves), tag)
}

/// `engine_with` for a test's own model.
pub fn engine_with_model<M: crate::model::Model>(model: M, tag: &str) -> (crate::engine::Engine<M>, Recorder, PathBuf) {
    let rec = Recorder::default();
    let root = temp_root(tag);
    for d in ["housekeeping", "projects", "home"] { std::fs::create_dir_all(root.join(d)).unwrap(); }
    let e = crate::engine::Engine::new(
        crate::store::Store::open_in_memory().unwrap(),
        model,
        root.join("projects"),
        None,
        scripted_workers(&rec),
        root.join("housekeeping"),
    ).with_home(root.join("home"));
    (e, rec, root)
}

/// The events of one user message — `handle`'s typed twin, for a test that reads the cards
/// rather than the prose.
pub fn events_of(e: &mut crate::engine::Engine<crate::model::FakeModel>, text: &str) -> Vec<aios_proto::Event> {
    e.handle_events(text).unwrap()
}

/// A finished job's own last event — every job that ends Done or Failed now emits a `Learned`
/// right after it, even an empty one (Task 8 ruling: the rail's spinner needs it), so a test that
/// wants the job's own last word skips that tail event.
pub fn before_learned(ev: &[aios_proto::Event]) -> &aios_proto::Event {
    ev.iter().rev().find(|e| !matches!(e, aios_proto::Event::Learned { .. })).unwrap()
}

/// A fake HTTP server on an ephemeral 127.0.0.1 port: answers the next `times` connections with
/// `response` verbatim. Each request is read to the end of its body (by Content-Length), so a big
/// POST is never cut short by a reset; a connection that sends nothing (a port probe) still counts.
pub fn serve(response: String, times: usize) -> std::net::SocketAddr {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for _ in 0..times {
            let Ok((mut stream, _)) = listener.accept() else { return };
            let mut received = Vec::new();
            let mut buf = [0u8; 4096];
            let mut want = usize::MAX;
            while received.len() < want {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => received.extend_from_slice(&buf[..n]),
                }
                if want == usize::MAX {
                    if let Some(end) = received.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4) {
                        let head = String::from_utf8_lossy(&received[..end]).to_ascii_lowercase();
                        let len = head.lines().find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok()).unwrap_or(0);
                        want = end + len;
                    }
                }
            }
            let _ = stream.write_all(response.as_bytes());
        }
    });
    addr
}

/// An HTTP/1.1 response with `status` (e.g. "200 OK") and a JSON body.
pub fn json_response(status: &str, body: &str) -> String {
    format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len())
}

/// A 127.0.0.1 port nothing listens on: bound, then let go.
pub fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}
