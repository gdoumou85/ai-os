//! Test-only helpers: a worker whose outcomes are scripted and whose calls are shared.
use executor::action::Action;
use executor::undo::UndoEntry;
use executor::worker::{Outcome, Worker};
use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Rc<RefCell<Vec<Action>>>,
    /// Outcomes handed out in order; when empty, everything succeeds with "ok".
    pub outcomes: Rc<RefCell<VecDeque<Outcome>>>,
    /// The privileged hand's own lists: a test can tell which lane ran an action.
    pub admin_calls: Rc<RefCell<Vec<Action>>>,
    pub admin_outcomes: Rc<RefCell<VecDeque<Outcome>>>,
    /// The desktop hand's own lists, same shape.
    pub desktop_calls: Rc<RefCell<Vec<Action>>>,
    pub desktop_outcomes: Rc<RefCell<VecDeque<Outcome>>>,
    pub reversed: Rc<RefCell<Vec<UndoEntry>>>,
    /// Outcomes for `reverse`, in order; when empty every reversal succeeds. A failing
    /// reversal is the only way to test that undo reports it and still runs the rest.
    pub reverse_outcomes: Rc<RefCell<VecDeque<Outcome>>>,
}

/// The sandbox hand. It really writes: the blueprint gate now asks the filesystem whether
/// BLUEPRINT.md exists, so a worker that only records would let every new-project test pass
/// for the wrong reason.
pub struct ScriptedWorker {
    pub rec: Recorder,
    pub ws: PathBuf,
}

impl ScriptedWorker {
    /// Where a path lands inside the workspace, or `None` if it escapes it — an escape is the
    /// scripted `Outcome`'s business, so nothing is written for it.
    fn target(&self, path: &str) -> Option<PathBuf> {
        let p = Path::new(path);
        if p.components().any(|c| c == Component::ParentDir) {
            return None;
        }
        let joined = if p.is_absolute() { p.to_path_buf() } else { self.ws.join(p) };
        if joined.starts_with(&self.ws) { Some(joined) } else { None }
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
            _ => {}
        }
        self.rec.calls.borrow_mut().push(action.clone());
        self.rec.outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("ok"))
    }
}

/// The privileged hand: records only — nothing a test runs should touch the real machine.
pub struct AdminRecorder(pub Recorder);

impl Worker for AdminRecorder {
    fn run(&self, action: &Action) -> Outcome {
        self.0.admin_calls.borrow_mut().push(action.clone());
        self.0.admin_outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("ok"))
    }
    fn reverse(&self, entry: &UndoEntry) -> Outcome {
        self.0.reversed.borrow_mut().push(entry.clone());
        self.0.reverse_outcomes.borrow_mut().pop_front().unwrap_or(Outcome::ok("reversed"))
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

/// All three lanes over one recorder, one triple per workspace the engine asks for.
pub fn scripted_workers(rec: &Recorder) -> crate::engine::WorkerFactory {
    let rec = rec.clone();
    Box::new(move |ws| (
        Box::new(ScriptedWorker { rec: rec.clone(), ws: ws.to_path_buf() }) as Box<dyn Worker>,
        Box::new(AdminRecorder(rec.clone())) as Box<dyn Worker>,
        Box::new(DesktopRecorder(rec.clone())) as Box<dyn Worker>,
    ))
}

/// The files half of undo, recorded instead of done. A temp root is never btrfs, so the real
/// `snapshot::take` always answers `None` there and the whole ProjectSnapshot path — the row at
/// job start, the restore on undo, the report line — had no unit coverage at all.
///
/// Clone-as-handle: the engine takes one, the test keeps another and reads what happened.
#[derive(Clone, Default)]
pub struct FakeSnapshotter(pub Rc<Snapshots>);

#[derive(Default)]
pub struct Snapshots {
    /// (folder, snapshots_dir, job_id) per `take`.
    pub taken: RefCell<Vec<(PathBuf, PathBuf, String)>>,
    /// (folder, snapshot) per `restore`.
    pub restored: RefCell<Vec<(PathBuf, PathBuf)>>,
    /// Whether `take` answers `Some(<snapshots_dir>/<project>@<job>)` or `None` (a plain folder).
    pub snapshots: Cell<bool>,
    pub take_fails: Cell<bool>,
    pub restore_fails: Cell<bool>,
}

impl FakeSnapshotter {
    /// One that really "snapshots" — the default records but answers `None`, like a plain folder.
    pub fn working() -> Self {
        let f = Self::default();
        f.0.snapshots.set(true);
        f
    }
    pub fn last_snapshot(&self) -> PathBuf {
        let taken = self.0.taken.borrow();
        let (folder, dir, job) = taken.last().expect("nothing was snapshotted");
        dir.join(format!("{}@{job}", folder.file_name().unwrap_or_default().to_string_lossy()))
    }
}

fn nope(what: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, format!("the fake {what} was told to fail"))
}

impl crate::snapshot::Snapshotter for FakeSnapshotter {
    fn take(&self, folder: &Path, snapshots_dir: &Path, job_id: &str) -> std::io::Result<Option<PathBuf>> {
        self.0.taken.borrow_mut().push((folder.to_path_buf(), snapshots_dir.to_path_buf(), job_id.to_string()));
        if self.0.take_fails.get() {
            return Err(nope("snapshot"));
        }
        Ok(self.0.snapshots.get().then(|| self.last_snapshot()))
    }
    fn restore(&self, folder: &Path, snapshot: &Path) -> std::io::Result<()> {
        self.0.restored.borrow_mut().push((folder.to_path_buf(), snapshot.to_path_buf()));
        if self.0.restore_fails.get() { Err(nope("restore")) } else { Ok(()) }
    }
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
    let rec = Recorder::default();
    let root = temp_root(tag);
    let housekeeping = root.join("housekeeping");
    std::fs::create_dir_all(&housekeeping).unwrap();
    // A temp root is never btrfs, so `snapshot::take` returns None here and no test job
    // ever gets a ProjectSnapshot row — the folder still has to exist for the engine.
    let snapshots = root.join("snapshots");
    std::fs::create_dir_all(&snapshots).unwrap();
    let e = crate::engine::Engine::new(
        crate::store::Store::open_in_memory().unwrap(),
        crate::model::FakeModel::new(moves),
        root.clone(),
        None,
        scripted_workers(&rec),
        housekeeping,
        snapshots,
    );
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

/// Same engine, with the files half of undo faked so a temp root can exercise it.
pub fn engine_with_snapshots(moves: Vec<crate::moves::Move>, tag: &str, snap: &FakeSnapshotter)
    -> (crate::engine::Engine<crate::model::FakeModel>, Recorder, PathBuf) {
    let (e, rec, root) = engine_with(moves, tag);
    (e.with_snapshotter(Box::new(snap.clone())), rec, root)
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
