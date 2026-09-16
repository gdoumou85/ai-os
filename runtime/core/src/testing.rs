//! Test-only helpers: a worker whose outcomes are scripted and whose calls are shared.
use executor::action::Action;
use executor::undo::UndoEntry;
use executor::worker::{Outcome, Worker};
use std::cell::RefCell;
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
    pub reversed: Rc<RefCell<Vec<UndoEntry>>>,
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
        Outcome::ok("reversed")
    }
}

/// Both lanes over one recorder, one pair per workspace the engine asks for.
pub fn scripted_workers(rec: &Recorder) -> crate::engine::WorkerFactory {
    let rec = rec.clone();
    Box::new(move |ws| (
        Box::new(ScriptedWorker { rec: rec.clone(), ws: ws.to_path_buf() }) as Box<dyn Worker>,
        Box::new(AdminRecorder(rec.clone())) as Box<dyn Worker>,
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
    let rec = Recorder::default();
    let root = temp_root(tag);
    let housekeeping = root.join("housekeeping");
    std::fs::create_dir_all(&housekeeping).unwrap();
    let e = crate::engine::Engine::new(
        crate::store::Store::open_in_memory().unwrap(),
        crate::model::FakeModel::new(moves),
        root.clone(),
        None,
        scripted_workers(&rec),
        housekeeping,
    );
    (e, rec, root)
}
