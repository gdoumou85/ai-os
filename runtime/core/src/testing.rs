//! Test-only helpers: a worker whose outcomes are scripted and whose calls are shared.
use executor::action::Action;
use executor::worker::{Outcome, Worker};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Default)]
pub struct Recorder {
    pub calls: Rc<RefCell<Vec<Action>>>,
    /// Outcomes handed out in order; when empty, everything succeeds with "ok".
    pub outcomes: Rc<RefCell<VecDeque<Outcome>>>,
}

pub struct ScriptedWorker(pub Recorder);

impl Worker for ScriptedWorker {
    fn run(&self, action: &Action) -> Outcome {
        self.0.calls.borrow_mut().push(action.clone());
        self.0.outcomes.borrow_mut().pop_front().unwrap_or(Outcome { ok: true, detail: "ok".into() })
    }
}

pub fn temp_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("ai-os-core-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    root
}

pub fn engine_with(moves: Vec<crate::moves::Move>, tag: &str) -> (crate::engine::Engine<crate::model::FakeModel>, Recorder, PathBuf) {
    let rec = Recorder::default();
    let r2 = rec.clone();
    let root = temp_root(tag);
    let e = crate::engine::Engine::new(
        crate::store::Store::open_in_memory().unwrap(),
        crate::model::FakeModel::new(moves),
        root.clone(),
        None,
        Box::new(move |_ws| Box::new(ScriptedWorker(r2.clone())) as Box<dyn Worker>),
    );
    (e, rec, root)
}
