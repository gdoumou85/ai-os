//! The loop (one-loop design): a message starts a turn; the model makes one move at a time over
//! one conversation until it replies or asks. The engine holds only Stop, the action cap, the loop
//! catch and the notes it hands over — the model decides the rest.
use crate::event::describe;
use crate::job::{Job, State, StepRecord};
use crate::model::{Model, ModelError, Prompt};
use crate::moves::{Ending, Move};
use crate::prompt;
use crate::store::{ProjectRow, Store, StoreError};
use aios_proto::{ChangedFile, Event, FileKind, JobState};
use executor::action::Action;
use executor::executor::Executor;
use executor::log::{ActionLog, LogError};
use executor::worker::{Outcome, Worker};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}};

/// One project folder in, the two hands that serve it out: (machine, desktop).
pub type WorkerFactory = Box<dyn Fn(&Path) -> (Box<dyn Worker>, Box<dyn Worker>)>;

/// The scratch folder a housekeeping job works in — the machine's own jobs have no project.
pub const HOUSEKEEPING_DIR: &str = "/data/housekeeping";

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)] Store(#[from] StoreError),
    #[error(transparent)] Model(#[from] ModelError),
    #[error(transparent)] Log(#[from] LogError),
    #[error("io: {0}")] Io(#[from] std::io::Error),
}

/// Words the user sends while a turn works (one-loop design §3). `None` when no turn is running:
/// the service then queues the words as a new turn instead.
pub type Inbox = Arc<Mutex<Option<Vec<String>>>>;

/// Alerts waiting for the engine thread (watchers design §2), urgent ones first.
pub type Alerts = Arc<Mutex<std::collections::VecDeque<crate::watchers::Alert>>>;

pub struct Engine<M: Model> {
    pub store: Store,
    pub model: M,
    default_root: PathBuf,
    log_path: Option<String>,
    workers: WorkerFactory,
    housekeeping_dir: PathBuf,
    /// Where commands run: the user's home, as a terminal would open.
    home: PathBuf,
    sink: Box<dyn FnMut(&Event)>,
    out: Vec<Event>,
    stop: Arc<AtomicBool>,
    image: Option<Vec<u8>>,
    system: std::cell::OnceCell<String>,
    inbox: Inbox,
    /// The last turn ended on a Stop: the "stop" queued behind it is already answered.
    just_stopped: bool,
    /// Words sent while a turn was ending (Stop, the cap, an error): they run as the next turn.
    pending: Vec<String>,
    /// The last turn ended on a question: its request and the question, so the answer carries
    /// them. Alone, "search the whole system" read as a new vague request and a 9B asked again,
    /// round after round (the owner, 2026-09-24).
    asked: Option<String>,
    /// This turn answers a question: no second one before the model has acted on the answer.
    answered: bool,
    /// The project the chat is about: a message naming another starts a fresh chat.
    chat_project: Option<String>,
    /// Alerts waiting: the service fills it, the engine thread takes one between turns, and a
    /// turn takes an urgent one when `yield_` is up.
    alerts: Alerts,
    /// Raised with an urgent alert: the turn at work parks at its next step.
    yield_: Arc<AtomicBool>,
    /// The turn running is an alert's: no inbox, no question, nothing kept for later.
    in_alert: bool,
}

/// The windows a job worked: those it looked into, in first-seen order, from the steps that
/// succeeded (2a §7). Ids come only from a windowed look, so nothing is pressed, typed or read in
/// a window that was never looked at — and an `open_app` desktop-entry id is not a window's name.
pub(crate) fn windows_worked(job: &Job) -> Vec<String> {
    let mut v: Vec<String> = vec![];
    for s in job.steps.iter().filter(|s| s.ok) {
        // A blank name is the model's way of writing "list the windows" (the hand reads it as
        // none), so it is not a window this job worked: the live run's Done card named one.
        let Action::Look { window: Some(w), .. } = &s.action else { continue };
        if w.trim().is_empty() || v.contains(w) { continue; }
        v.push(w.clone());
    }
    v
}

pub fn sanitize_project_name(raw: &str) -> String {
    let mut s = String::new();
    let mut dash = false;
    for c in raw.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() { s.push(c); dash = false; }
        else if !dash && !s.is_empty() { s.push('-'); dash = true; }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "project".into() } else { s }
}

// ponytail: fixed word lists, not the model — a stop must never depend on a 9B reading tone.
fn normalize(text: &str) -> String {
    text.trim().to_lowercase().trim_end_matches(['.', '!', '?', ',']).trim().to_string()
}

pub fn is_stop(text: &str) -> bool {
    const STOP_PHRASES: [&str; 10] = [
        "stop", "cancel", "leave it", "abort", "never mind", "forget it",
        "stop it", "stop now", "please stop", "stop please",
    ];
    STOP_PHRASES.contains(&normalize(text).as_str())
}

/// Keep or throw away what the last learning turn left waiting (Phase 3 §5): fixed words, no model.
pub fn is_keep_learned(text: &str) -> bool { normalize(text) == "keep what you learned" }
pub fn is_discard_learned(text: &str) -> bool { normalize(text) == "discard what you learned" }

/// Whether a reply says work is under way or coming instead of doing it: a reply ends the turn and
/// runs nothing. The owner, 2026-09-23 ("I will…" and nothing happened) and again on a free cloud
/// model, 2026-09-25: "I'm reviewing the current files … now" twice, and nothing read. "I cannot"
/// on a machine it has root on is the same (full-access rule); forgetting, which Clear does, is not.
// ponytail: phrase list, a model's words in English; widen it when a live one slips past.
fn promises_work(text: &str) -> bool {
    let t = text.to_lowercase().replace('’', "'");
    let under_way = t.split(|c: char| !c.is_alphanumeric() && c != '\'').collect::<Vec<_>>()
        .windows(2).any(|w| matches!(w[0], "i'm" | "am" | "we're") && w[1].len() > 4 && w[1].ends_with("ing"));
    under_way || ["i will ", "i'll ", "let me ", "proceeding"].iter().any(|w| t.contains(w))
        || (["i cannot ", "i can't ", "i could not ", "i couldn't ", "i am unable", "i'm unable", "i was unable", "outside what i can"].iter().any(|w| t.contains(w)) && !t.contains("clear"))
        // "no filesystem action tool", "no file or command tools are available" (free cloud models, 2026-09-25)
        || (t.contains("tool") && ["no ", "not ", "unavailable", "unable"].iter().any(|w| t.contains(w)))
}

/// Whether the user's own words ask for something to hold from now on. The model filled
/// `remember` on its own — a project's description, "Blender is installed" — and every one of
/// those notes pulled the next chat back to the old work (the owner, 2026-09-23).
/// ponytail: a word list; a note the user wanted in other words is lost, and they say it again.
fn asks_to_keep(text: &str) -> bool {
    let t = text.to_lowercase();
    ["always", "never", "from now on", "remember", "keep in mind", "every time", "whenever",
     "in future", "in the future", "going forward", "by default"].iter().any(|w| t.contains(w))
}

/// The owner's own words asking to be warned (watchers design §1): a watcher made for them is theirs.
fn asks_to_watch(text: &str) -> bool {
    let t = text.to_lowercase();
    ["watch", "warn", "remind", "alert", "notify", "tell me when", "every"].iter().any(|w| t.contains(w))
}

/// Where the user's things are, as the user would know it. Never told, a 9B clearing "all
/// project work" guessed /home/user/projects and worked on a folder that was not there
/// (the owner's run, 2026-09-23).
fn places(home: &str, root: &Path, scratch: &Path, projects: &[ProjectRow]) -> String {
    let mut s = format!("Where things are: the user's home is {home}; projects live in {} (the projects_root setting)", root.display());
    if !projects.is_empty() {
        // ponytail: the 10 most recent; an older one is found with ls, like any folder.
        let list: Vec<String> = projects.iter().take(10).map(|p| format!("{} ({})", p.name, p.folder)).collect();
        s.push_str(&format!("; projects so far: {}", list.join(", ")));
    }
    s.push_str(&format!("; the scratch folder for housekeeping is {}.", scratch.display()));
    s
}

/// The journal lines that share a telling word with what the user just said, newest last. A word
/// every request has ("delete", "project") tells nothing and would bring back old work unasked —
/// the Blender chat that would not go away (2026-09-23).
// ponytail: word overlap with a stop list; a project called by another name is missed, and the
// user names it again.
fn journal_for(journal: &str, words: &str) -> String {
    const PLAIN: &[&str] = &["project", "projects", "delete", "remove", "create", "make", "start", "files", "file",
        "folder", "folders", "please", "want", "this", "that", "with", "from", "into", "what", "your", "have", "them",
        "then", "there", "these", "those", "will", "would", "should", "could", "just", "also", "like", "some", "lets",
        "about", "work", "build", "done", "asked", "housekeeping", "paths", "home", "data", "user", "again", "anything", "everything"];
    let split = |s: &str| s.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 4 && !PLAIN.contains(w)).map(String::from).collect::<Vec<_>>();
    let wanted = split(words);
    let lines: Vec<&str> = journal.lines().filter(|l| l.starts_with("- ") && split(l).iter().any(|w| wanted.contains(w))).collect();
    if lines.is_empty() { return String::new() }
    format!("What earlier jobs did (the journal, lines about this request only):\n{}\n", lines[lines.len().saturating_sub(5)..].join("\n"))
}

/// The paths an action touches: the file it reads or writes, and absolute paths in a command.
fn paths_in(action: &Action, folder: &str) -> Vec<PathBuf> {
    match action {
        Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::AppendFile { path, .. } | Action::ReadFile { path, .. } => vec![Path::new(folder).join(path)],
        Action::RunCommand { argv } | Action::StartProgram { argv, .. } => argv.iter().flat_map(|a| a.split_whitespace())
            .map(|w| w.trim_matches(|c: char| "'\";&|()".contains(c)).to_string())
            .filter(|w| w.len() > 1 && w.starts_with('/') && !["/usr/", "/bin/", "/dev/", "/proc/", "/tmp/"].iter().any(|p| w.starts_with(p)))
            .map(PathBuf::from).collect(),
        _ => vec![],
    }
}

/// One line of the journal for a turn that did something (one-loop design §2).
fn journal_line(turn: &Job, how: &str, text: &str) -> String {
    let mut paths: Vec<String> = vec![];
    for s in turn.steps.iter().filter(|s| s.ok) {
        for p in paths_in(&s.action, &turn.folder) {
            let p = p.display().to_string();
            if !paths.contains(&p) { paths.push(p); }
        }
    }
    let flat = |s: &str, n: usize| s.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(n).collect::<String>();
    let mut line = format!("- {how}: asked \"{}\" → {}", flat(&turn.request, 150), flat(text, 200));
    // ponytail: the first eight; a turn that touched more is summed up by its own words.
    if !paths.is_empty() { line.push_str(&format!(" Paths: {}.", paths.iter().take(8).cloned().collect::<Vec<_>>().join(", "))); }
    line
}

/// What is at the top of a project's folder, so a new chat builds on the files already there
/// instead of writing them again: a stopped run left index.html and style.css, its notes never
/// said so, and the next run made both anew (the owner, 2026-09-25).
fn files_of(folder: &str) -> String {
    let Ok(rd) = std::fs::read_dir(folder) else { return String::new() };
    let mut v: Vec<String> = rd.flatten().filter_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') { return None }
        let m = e.metadata().ok()?;
        Some(if m.is_dir() { format!("{name}/") } else { format!("{name} ({} bytes)", m.len()) })
    }).collect();
    if v.is_empty() { return "Files there: none yet.".into() }
    v.sort();
    // ponytail: the top level, first 30; ls for the rest.
    let more = if v.len() > 30 { format!(" and {} more", v.len() - 30) } else { String::new() };
    v.truncate(30);
    format!("Files there: {}{more}.", v.join(", "))
}

fn not_a_move(error: &str) -> String {
    format!("your answer was not a move ({}). Answer with one JSON object whose first key is \"move\": reply, ask, todo, act or remember.", error.chars().take(120).collect::<String>())
}

fn short(s: &str) -> String {
    if s.chars().count() <= 80 { return s.to_string() }
    format!("{}…", s.chars().take(80).collect::<String>().trim_end())
}

impl<M: Model> Engine<M> {
    const MAX_ACTIONS: usize = 100;
    /// Every move counts here, not only actions: a model going round on to-do lists, notes that are
    /// not kept or unreadable answers would otherwise never end (final review, 2026-09-24).
    const MAX_MOVES: usize = 200;
    const WENT_ROUND: &'static str = "I went round without getting anywhere, so I stopped.";
    /// What a model that cannot see is told where the desktop hand offered the screen (final
    /// review, 2026-09-24): the picture never reaches it, so neither does the advice to use it.
    const CANNOT_SEE: &'static str = "this window lists no controls, and this model cannot see the screen: use key, the command line, or the program's own scripting (like blender --background --python)";
    /// The same action this many times in a row (any result), or failing the same way: warn, then end.
    const SAME_FAIL_WARN: usize = 3;
    const SAME_FAIL_END: usize = 5;

    pub fn new(store: Store, model: M, default_root: PathBuf, log_path: Option<String>, workers: WorkerFactory, housekeeping_dir: PathBuf) -> Self {
        let home = PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into()));
        Self { store, model, default_root, log_path, workers, housekeeping_dir, home, sink: Box::new(|_| {}), out: vec![],
            stop: Arc::new(AtomicBool::new(false)), image: None, system: std::cell::OnceCell::new(), inbox: Arc::new(Mutex::new(None)), just_stopped: false, pending: vec![], asked: None, answered: false, chat_project: None,
            alerts: Default::default(), yield_: Arc::new(AtomicBool::new(false)), in_alert: false }
    }

    /// Where commands run (tests point it at a temp folder).
    pub fn with_home(mut self, home: PathBuf) -> Self { self.home = home; self }

    /// The service's handle on words sent while a turn works.
    pub fn inbox(&self) -> Inbox { self.inbox.clone() }

    /// The service's handle on the alerts waiting (watchers design §2).
    pub fn alerts(&self) -> Alerts { self.alerts.clone() }

    /// Raised by the service with an urgent alert.
    pub fn yield_flag(&self) -> Arc<AtomicBool> { self.yield_.clone() }

    /// What this machine has, ahead of every prompt (machine-map spec §2). Programs and installs
    /// are scanned each time, so something installed shows on the very next turn.
    fn places(&self) -> Result<String, EngineError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "unknown".into());
        // A folder under the root is a project whether or not the store has a row for it.
        let live: Vec<ProjectRow> = self.projects()?;
        Ok(places(&home, &self.projects_root()?, &self.housekeeping_dir, &live))
    }

    fn journal_path(&self) -> PathBuf { self.housekeeping_dir.join("JOURNAL.md") }

    /// The journal lines about `words`, for a prompt ("" when none are).
    fn journal(&self, words: &str) -> String {
        journal_for(&std::fs::read_to_string(self.journal_path()).unwrap_or_default(), words)
    }

    fn machine_block(&self) -> String {
        crate::machine::current_block(self.store.conn(), self.system.get_or_init(crate::machine::system_line))
    }

    /// A clone of the engine's own stop flag — raise it to cancel the open job between steps.
    pub fn stop_flag(&self) -> Arc<AtomicBool> { self.stop.clone() }

    /// Where projects go when the user never said (the service lists them from its client threads).
    pub fn default_root(&self) -> PathBuf { self.default_root.clone() }

    /// Hand every event to `sink` as it happens — what the service broadcasts from.
    pub fn with_sink(mut self, sink: Box<dyn FnMut(&Event)>) -> Self {
        self.sink = sink;
        self
    }

    /// One event out: to the sink now, and onto this call's list for `handle_events`.
    fn emit(&mut self, ev: Event) {
        (self.sink)(&ev);
        self.out.push(ev);
    }

    /// A passing status line: to the sink (the rail's status bar) and nowhere else. Not on
    /// `out`, so it never reaches the chat history — four of these would push the person's own
    /// last words out of the front door's `recent_messages(4)` window.
    fn tick(&mut self, job_id: &str, text: String) {
        (self.sink)(&Event::Busy { job_id: job_id.into(), text });
    }

    pub fn housekeeping_dir(&self) -> &Path { &self.housekeeping_dir }

    /// Where a new project's folder goes. A store error is propagated, not read as "unset":
    /// that would quietly put the project under the wrong root (I3's lesson).
    fn projects_root(&self) -> Result<PathBuf, EngineError> {
        Ok(crate::projects::root(&self.store, &self.default_root)?)
    }

    /// The engine's own lane (`executor::Lane::Engine`): a setting is a row in our store, so no
    /// worker can apply it. One known key so far; an unknown one names what is known. Any
    /// absolute folder will do (full-access spec).
    fn apply_setting(&self, key: &str, value: &str) -> Result<Outcome, EngineError> {
        if key != "projects_root" {
            return Ok(Outcome::err(format!("unknown setting '{key}'; known settings: projects_root")));
        }
        if !Path::new(value).is_absolute() {
            return Ok(Outcome::err(format!("projects_root must be an absolute path, not {value}")));
        }
        self.store.set_setting(key, value)?;
        Ok(Outcome::ok(format!("setting {key} = {value}")))
    }

    /// `watch` (watchers design §1): creates the watcher or replaces the one of that name.
    fn watch(&mut self, turn: &Job, name: &str, reason: &str, urgent: bool, when: &str, command: &[String]) -> Result<Outcome, EngineError> {
        let who = if !self.in_alert && asks_to_watch(&turn.request) { "you" } else { "AI" };
        let words: String = turn.request.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
        let mut w = match crate::watchers::build(name, reason, urgent, &format!("{who} (\"{words}\")"), when, command.to_vec()) {
            Ok(w) => w,
            Err(e) => return Ok(Outcome::err(e)),
        };
        let old = crate::watchers::get(self.store.conn(), &w.name)?;
        // Every alert brings its watcher's reason up to date: that does not make it the AI's.
        if let Some(old) = old.as_ref().filter(|_| self.in_alert) { w.made_by = old.made_by.clone(); }
        // A live program runs its new command from the scheduler's next tick.
        if old.is_some_and(|old| crate::watchers::needs_restart(&old, &w)) {
            let _ = executor::programs::stop(&crate::watchers::program_name(&w.name));
        }
        crate::watchers::put(self.store.conn(), &w, crate::store::now_ms())?;
        self.watchers_changed()?;
        let what = match w.kind { crate::watchers::Kind::Timer => "a timer", crate::watchers::Kind::Check => "a check", crate::watchers::Kind::Push => "a live program" };
        Ok(Outcome::ok(format!("watching {}: {}, {what}", w.name, crate::watchers::when_text(&w))))
    }

    fn unwatch(&mut self, name: &str) -> Result<Outcome, EngineError> {
        if !crate::watchers::delete(self.store.conn(), name)? {
            let names: Vec<String> = crate::watchers::list(self.store.conn())?.into_iter().map(|w| w.name).collect();
            return Ok(Outcome::err(format!("no watcher is called {name}; there are: {}", if names.is_empty() { "none".to_string() } else { names.join(", ") })));
        }
        let _ = executor::programs::stop(&crate::watchers::program_name(name));
        self.watchers_changed()?;
        Ok(Outcome::ok(format!("stopped watching {name}")))
    }

    /// Every rail's Watchers page redrawn.
    fn watchers_changed(&mut self) -> Result<(), EngineError> {
        let watchers = crate::watchers::views(self.store.conn())?;
        self.emit(Event::Watchers { watchers });
        Ok(())
    }

    /// One model call after a job, never fatal (Phase 3 §4): the job is already over and said so.
    ///
    /// A `Learned` always follows, even with nothing kept: the rail's spinner keeps turning
    /// through this whole turn (a slow local model can take minutes for it), and `Learned` is
    /// the only event that tells it to stop (`cards::busy_after`).
    fn learn(&mut self, job: &Job, passed: bool) {
        let job_id = job.id.clone();
        let nothing = Event::Learned { job_id: job_id.clone(), lines: vec![], pending: false };
        // What an earlier job left waiting for Keep/Discard and nobody answered goes now (spec §5),
        // whatever this turn comes to: the rail takes the older card's Keep away on this Learned.
        // An alert is not the owner moving on: what waits for their Keep still waits.
        if !self.in_alert {
            if let Err(e) = crate::notes::discard_pending(self.store.conn()) { eprintln!("engine: an old proposal could not be discarded ({e})"); }
        }
        // A job that did not pass can only mark the tips it was shown; with none, there is nothing
        // to ask the model.
        if !passed && job.shown_notes.is_empty() { self.emit(nothing); return }
        self.tick(&job_id, "Thinking about what to remember from this job…".into());
        let mv = match self.model.next_move(&prompt::learning_turn(job, passed)) {
            Ok(m) => m,
            Err(e) => { eprintln!("engine: no learning turn ({e})"); self.emit(nothing); return }
        };
        let Move::Learn { entries, used, wrong, remove } = mv else { self.emit(nothing); return };
        match crate::learn::apply(self.store.conn(), job, entries, &used, &wrong, &remove, passed) {
            Ok(l) => self.emit(Event::Learned { job_id, lines: l.lines, pending: l.pending }),
            Err(e) => { eprintln!("engine: what was learned could not be saved ({e})"); self.emit(nothing); }
        }
    }

    /// A turn lives in memory: nothing is open between messages.
    pub fn state(&self) -> Result<Option<JobState>, EngineError> { Ok(None) }

    /// At start: a job left open by v0.9.x is closed, and the chat says so.
    pub fn resume(&mut self) -> Result<Vec<Event>, EngineError> {
        self.out.clear();
        // A restart shows an empty screen, so the AI starts with an empty chat too: an update left
        // twenty old to-do lists behind a fresh "Hi AI" (the owner, 2026-09-24). What is kept
        // lives in the projects' notes, the journal and the standing instructions.
        self.store.forget_all_messages()?;
        if let Some(mut old) = self.store.open_job()? {
            old.state = State::Cancelled;
            self.store.save_job(&old)?;
            let what = if old.request.is_empty() { old.goal.clone() } else { old.request.clone() };
            self.emit(Event::Said { text: format!("(An unfinished job from before the update was closed: \"{}\". Say it again if you still want it.)", short(&what)) });
        }
        Ok(std::mem::take(&mut self.out))
    }

    pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        Ok(self.handle_events(text)?.iter().flat_map(crate::event::lines).collect())
    }

    pub fn handle_events(&mut self, text: &str) -> Result<Vec<Event>, EngineError> {
        self.out.clear();
        let r = self.handle_inner(text);
        let out = std::mem::take(&mut self.out);
        r?;
        Ok(out)
    }

    /// An alert the service took off the queue between turns (watchers design §2).
    pub fn handle_alert(&mut self, a: &crate::watchers::Alert) -> Result<Vec<Event>, EngineError> {
        self.out.clear();
        let r = self.alert_turn(a);
        let out = std::mem::take(&mut self.out);
        r?;
        Ok(out)
    }

    /// One alert as a turn of its own, in a chat of its own that goes when it ends; the owner's
    /// chat, and a turn of theirs parked under this one, are left as they were. How it ended.
    fn alert_turn(&mut self, a: &crate::watchers::Alert) -> Result<State, EngineError> {
        let mut turn = Job::turn(&self.home.display().to_string(), &prompt::alert_request(a));
        let main = self.store.chat().to_string();
        let (answered, image, project) = (std::mem::take(&mut self.answered), self.image.take(), self.chat_project.clone());
        self.store.set_chat(&format!("alert-{}", turn.id));
        self.in_alert = true;
        self.give_tips(&mut turn);
        self.emit(Event::Alert { job_id: turn.id.clone(), watcher: a.watcher.clone(), text: a.text.clone(), reason: a.reason.clone(), urgent: a.urgent });
        let r = self.store.push_message("user", &turn.request).map_err(EngineError::from).and_then(|_| self.run_turn(&mut turn));
        // Its card is up from the start, so an error closes it like work that could not go on.
        let r = match r { Err(e) if turn.is_open() => self.end(&mut turn, State::Failed, format!("I could not go on: {e}")), r => r };
        let forgot = self.store.forget_messages();
        self.store.set_chat(&main);
        self.in_alert = false;
        // A project the alert reached into is not what the owner's chat is about.
        (self.answered, self.image, self.chat_project) = (answered, image, project);
        r?;
        forgot?;
        Ok(turn.state)
    }

    /// Whether the turn has its card up: from its first to-do list or action, or from the start
    /// for an alert.
    fn has_card(&self, turn: &Job) -> bool { self.in_alert || !turn.steps.is_empty() || !turn.plan.is_empty() }

    fn handle_inner(&mut self, text: &str) -> Result<(), EngineError> {
        if is_keep_learned(text) || is_discard_learned(text) {
            let keep = is_keep_learned(text);
            let c = self.store.conn();
            let n = if keep { crate::notes::keep_pending(c)? } else { crate::notes::discard_pending(c)? };
            let text = match (n, keep) { (0, _) => "There is nothing waiting to be kept or discarded.", (_, true) => "Kept what I learned.", (_, false) => "Discarded it." };
            self.emit(Event::Said { text: text.into() });
            return Ok(());
        }
        if is_stop(text) {
            // The word that ended the last turn by its flag arrives here after it: answered already.
            if !std::mem::take(&mut self.just_stopped) { self.emit(Event::Said { text: "Nothing is running now.".into() }); }
            return Ok(());
        }
        self.just_stopped = false;
        let mut text = text.to_string();
        loop {
            // Only while the question is still the last word: a Clear since then forgot it.
            let last_ask = self.store.recent_messages(1)?.first().is_some_and(|(r, t)| r == "assistant" && t.contains(r#""move":"ask""#));
            let answer_to = self.asked.take().filter(|_| last_ask);
            self.answered = answer_to.is_some();
            if answer_to.is_none() { self.switch_project(&text)?; }
            self.store.push_message("user", &text)?;
            let request = answer_to.map_or(text.clone(), |a| format!("{a}{text}"));
            let mut turn = Job::turn(&self.home.display().to_string(), &request);
            self.give_tips(&mut turn);
            *self.inbox.lock().unwrap() = Some(vec![]);
            let r = self.run_turn(&mut turn);
            // Whatever is still in the inbox was sent to this turn and never heard: never dropped.
            let left = self.inbox.lock().unwrap().take().unwrap_or_default();
            self.pending.extend(left);
            if let Err(e) = r {
                // A model or executor error mid-turn (a 429 from a free cloud provider is common)
                // still closes the card; with no card it is said as a line. Either way the words
                // sent meanwhile run next, so the error is said here rather than returned.
                if turn.is_open() && !(turn.steps.is_empty() && turn.plan.is_empty()) {
                    self.end(&mut turn, State::Failed, format!("I could not go on: {e}"))?;
                } else if turn.is_open() {
                    self.emit(Event::Said { text: format!("(something went wrong: {e})") });
                } else {
                    return Err(e);
                }
            }
            if self.pending.is_empty() { return Ok(()) }
            // Already echoed to the chat by the service when they were sent.
            text = std::mem::take(&mut self.pending).join("\n");
        }
    }

    /// A message naming another project than the chat's starts the chat afresh: that project's
    /// notes carry the work, and the other project's talk would only mix in (the owner, 2026-09-24).
    fn switch_project(&mut self, text: &str) -> Result<(), EngineError> {
        // A Clear on the rail's own thread emptied the chat without telling the engine.
        if self.store.recent_messages(1)?.is_empty() { self.chat_project = None; }
        let named: Vec<ProjectRow> = self.projects()?.into_iter().filter(|p| prompt::named_in(p, text)).collect();
        let Some(first) = named.first() else { return Ok(()) };
        if named.iter().any(|p| self.chat_project.as_ref() == Some(&p.folder)) { return Ok(()) }
        if self.chat_project.is_some() {
            self.store.forget_messages()?;
            self.emit(Event::Said { text: format!("(A fresh chat for {}: I go by its notes, and the chat about the other project is set aside.)", first.name) });
        }
        self.chat_project = Some(first.folder.clone());
        Ok(())
    }

    /// Skill tips by the message's words; notebooks the message names count as its skills.
    fn give_tips(&self, turn: &mut Job) {
        let c = self.store.conn();
        let words = turn.request.to_lowercase();
        turn.skills = crate::notes::notebooks(c).unwrap_or_default().into_iter()
            .filter(|n| n != crate::notes::THIS_COMPUTER && words.contains(n.as_str())).take(3).collect();
        match crate::notes::for_job(c, &turn.request, &turn.skills) {
            Ok((block, shown)) => { turn.notes_block = block; turn.shown_notes = shown; }
            Err(e) => eprintln!("engine: no tips for this turn ({e})"),
        }
    }

    /// Words the user sent mid-work, taken. `closing`: the turn is about to end, so with none
    /// waiting the inbox shuts in the same breath — a word sent after that is a new turn.
    fn take_inbox(&self, closing: bool) -> Vec<String> {
        let mut g = self.inbox.lock().unwrap();
        let got = g.as_mut().map(std::mem::take).unwrap_or_default();
        if closing && got.is_empty() { *g = None; }
        got
    }

    /// A word from the user mid-work joins the conversation and the pinned request.
    fn heard(&mut self, turn: &mut Job, text: &str) -> Result<(), EngineError> {
        self.store.push_message("user", text)?;
        turn.request.push_str(&format!("\n(added while you worked) {text}"));
        Ok(())
    }

    fn run_turn(&mut self, turn: &mut Job) -> Result<(), EngineError> {
        let _awake = executor::awake::hold("the AI is working");
        let mut unreadable = 0;
        let (mut reminded, mut nudged) = (false, false);
        let mut moves = 0;
        // The last move, when it changes nothing done twice (todo, remember): the next may not be
        // the same. A 27B re-sent one to-do list every 20 seconds and never acted (the owner,
        // 2026-09-24).
        let mut last: Option<&'static str> = None;
        loop {
            if self.stop.swap(false, Ordering::SeqCst) {
                self.just_stopped = true;
                return self.end(turn, State::Cancelled, "Stopped.".into());
            }
            // An urgent alert parks this turn where it stands (watchers design §2): the alert's
            // turn runs, then this one carries on with its request, to-do list and steps.
            if !self.in_alert && self.yield_.swap(false, Ordering::SeqCst) {
                let urgent = { let mut q = self.alerts.lock().unwrap(); q.iter().position(|a| a.urgent).and_then(|i| q.remove(i)) };
                if let Some(a) = urgent {
                    // A Stop during the alert ends the parked turn too.
                    if self.alert_turn(&a)? == State::Cancelled { return self.end(turn, State::Cancelled, "Stopped.".into()); }
                    self.store.push_message("result", &format!("(an urgent alert was handled in between: {}; carry on)", a.watcher))?;
                    // The alert's card took this one's place on the rail: it comes back with its
                    // list, its spinner and its Stop.
                    if self.has_card(turn) { self.emit(Event::Plan { job_id: turn.id.clone(), steps: turn.plan.clone() }); }
                    continue;
                }
            }
            // An alert's turn hears nobody: the owner's words wait for their own chat.
            if !self.in_alert { for w in self.take_inbox(false) { self.heard(turn, &w)?; } }
            if turn.steps.len() >= Self::MAX_ACTIONS {
                return self.end(turn, State::Failed, format!("I stopped after {} actions without finishing.", Self::MAX_ACTIONS));
            }
            if moves >= Self::MAX_MOVES {
                let text = Self::WENT_ROUND.to_string();
                if !self.has_card(turn) { self.emit(Event::Said { text }); return Ok(()); }
                return self.end(turn, State::Failed, text);
            }
            moves += 1;
            let mut p = self.prompt_for(turn)?;
            if let Some(m) = last { p.allowed.retain(|a| *a != m); }
            self.tick(&turn.id, "Thinking…".into());
            // The words so far on the status line, and Stop dropping the answer where it stands:
            // a big file at 5 words a second is a long answer (the owner, 2026-09-24).
            let (stop, sink, id) = (&self.stop, &mut self.sink, turn.id.clone());
            let answer = self.model.next_move_watched(&p, &mut |h| {
                if stop.load(Ordering::SeqCst) { return false }
                let text = match h {
                    crate::model::Heard { writing: 0, thinking: 0 } => "Thinking…".to_string(),
                    crate::model::Heard { writing: 0, thinking } => format!("Thinking… {thinking} words"),
                    crate::model::Heard { writing, .. } => format!("Writing its answer… {writing} words"),
                };
                sink(&Event::Busy { job_id: id.clone(), text });
                true
            });
            // The cloud pool switched models on its own: the owner hears which (2026-09-25).
            if let Some(text) = self.model.news() { self.emit(Event::Said { text }); }
            let mv = match answer {
                Ok(m) => { unreadable = 0; m }
                // A whole file past the runner's length limit: not a bad format, so said as what it is.
                Err(ModelError::CutOff(words)) if unreadable == 0 => {
                    unreadable = 1;
                    self.store.push_message("result", &format!("your answer was cut off at the model's length limit after {words} words, so nothing was done. Write a big file in parts: write_file the first part, then append_file each next part."))?;
                    continue;
                }
                Err(ModelError::Stuck(words)) if unreadable == 0 => {
                    unreadable = 1;
                    self.store.push_message("result", &format!("your answer got stuck writing blank space after {words} words, so nothing was done. Answer again, and keep it short."))?;
                    continue;
                }
                Err(ModelError::BadJson(e)) if unreadable == 0 => {
                    unreadable = 1;
                    self.store.push_message("result", &not_a_move(&e))?;
                    continue;
                }
                Err(ModelError::BadJson(_)) => {
                    let text = "(I could not read my own answer twice — the model is not answering in the required format. Please say that again, or switch the model.)".to_string();
                    if !self.has_card(turn) { self.emit(Event::Said { text }); return Ok(()); }
                    return self.end(turn, State::Failed, text);
                }
                Err(ModelError::Stopped) => {
                    self.stop.swap(false, Ordering::SeqCst);
                    self.just_stopped = true;
                    return self.end(turn, State::Cancelled, "Stopped.".into());
                }
                Err(e) => return Err(e.into()),
            };
            // A Stop while the model was thinking: its move is not acted on.
            if self.stop.swap(false, Ordering::SeqCst) {
                self.just_stopped = true;
                return self.end(turn, State::Cancelled, "Stopped.".into());
            }
            self.store.push_message("assistant", &serde_json::to_string(&mv).unwrap_or_default())?;
            let before = std::mem::replace(&mut last, match mv { Move::Todo { .. } => Some("todo"), Move::Remember { .. } => Some("remember"), _ => None });
            match mv {
                // For a runner that does not hold the model to the narrowed moves.
                Move::Todo { .. } | Move::Remember { .. } if before.is_some() && before == last => {
                    self.store.push_message("result", "you just did that: now act on the first open item of your to-do list, or reply")?;
                }
                // Work promised before any was done: asked once to do it instead.
                Move::Reply { text, .. } if !nudged && turn.steps.is_empty() && promises_work(&text) => {
                    nudged = true;
                    self.store.push_message("result", "a reply ends your turn and does nothing: you said you would do the work, or that you could not. You have what you need: act runs an action (run_command, read_file, write_file, web_search…) on this computer as the user, with sudo. Do the work now with act (or todo first), or reply with what you actually found")?;
                }
                Move::Reply { text, outcome, .. } => {
                    if !reminded {
                        if let Some(note) = self.notes_not_updated(turn)? {
                            reminded = true;
                            self.store.push_message("result", &note)?;
                            continue;
                        }
                    }
                    let more = if self.in_alert { vec![] } else { self.take_inbox(true) };
                    if !more.is_empty() { for w in more { self.heard(turn, &w)?; } continue; }
                    // A to-do list already put a card up: it closes like work did.
                    if !self.has_card(turn) { self.emit(Event::Said { text }); return Ok(()); }
                    return self.end(turn, if outcome == Ending::CouldNot { State::Failed } else { State::Done }, text);
                }
                Move::Ask { .. } if self.answered || self.in_alert => {
                    self.store.push_message("result", if self.in_alert { "nobody is here to answer during an alert: decide yourself, act, or reply" } else { "the user already answered your question: work with that answer, or reply" })?;
                }
                Move::Ask { question, options, .. } => {
                    let more = if self.in_alert { vec![] } else { self.take_inbox(true) };
                    if !more.is_empty() { for w in more { self.heard(turn, &w)?; } continue; }
                    self.image = None;
                    if !turn.steps.is_empty() { self.write_journal(turn, "asked a question", &question); }
                    self.asked = Some(format!("{}
(you asked) {question}
(their answer) ", turn.request));
                    self.emit(Event::NeedsAnswer { job_id: turn.id.clone(), questions: vec![question], options: vec![options] });
                    return Ok(());
                }
                // Nothing on the list: no card for it, so a plain reply after still reads as one.
                Move::Todo { items, .. } if prompt::todo_lines(&items).is_empty() => {
                    self.store.push_message("result", "your to-do list was empty: act, or reply")?;
                }
                Move::Todo { items, .. } => {
                    turn.plan = prompt::todo_lines(&items);
                    self.emit(Event::Plan { job_id: turn.id.clone(), steps: turn.plan.clone() });
                    self.store.push_message("result", "your to-do list is on the user's screen: now act on its first open item")?;
                }
                Move::Act { thought, action } => {
                    // Acting on the answer: a new question may come up now.
                    self.answered = false;
                    if self.act(turn, action, &thought)? { return Ok(()); }
                }
                Move::Remember { text, .. } => {
                    let note = if asks_to_keep(&turn.request) && !self.in_alert {
                        self.store.add_instruction(&text)?;
                        self.emit(Event::Said { text: format!("(Noted for the future: {text})") });
                        format!("kept for every later conversation: {text}")
                    } else { "not kept: the user did not ask for anything to hold from now on".to_string() };
                    self.store.push_message("result", &note)?;
                }
                Move::Learn { .. } => self.store.push_message("result", "learn is only for the note after work: reply, ask, todo, act or remember")?,
            }
        }
    }

    /// The conversation for the next call: the brief, the chat that fits, and — last, with the
    /// newest message — the context block, where the request and the to-do list are pinned.
    fn prompt_for(&mut self, turn: &Job) -> Result<Prompt, EngineError> {
        let sees = self.model.sees();
        // Helpers while Cloud has an account for them; an alert's turn does its own work.
        let helpers = !self.in_alert && !self.model.helper_accounts().is_empty();
        let system = prompt::brief(sees) + if helpers { prompt::HELPERS } else { "" };
        let (machine, places) = (self.machine_block(), self.places()?);
        let (instructions, journal, notes) = (self.store.instructions()?, self.journal(&turn.request), self.named_notes(&turn.request)?);
        let programs = executor::programs::running();
        let watchers = crate::watchers::line(self.store.conn())?;
        let ctx = prompt::context_block(&prompt::Context { machine: &machine, places: &places, programs: &programs, instructions: &instructions,
            journal: &journal, notes: &notes, tips: &turn.notes_block, request: &turn.request, todo: &turn.plan, watchers: &watchers });
        // The answer needs room too, and a whole file is one answer: a quarter of the window, at
        // least the 1500 tokens it always had.
        let window = self.model.context_tokens();
        let budget = window.saturating_sub((window / 4).max(1500) + (system.len() + ctx.len()) / 4);
        let mut chat = prompt::fit(&self.store.all_messages()?, budget);
        let last = chat.pop().map(|m| m.content).unwrap_or_default();
        Ok(Prompt { system, user: format!("{ctx}\n{last}"), history: chat, allowed: if self.answered || self.in_alert { vec!["reply", "todo", "act", "remember"] } else { vec!["reply", "ask", "todo", "act", "remember"] }, image: self.image.take(), no_screen: !sees, helpers, machine_only: false })
    }

    /// Runs one action and puts its result in the conversation. `true`: the turn ended here.
    fn act(&mut self, turn: &mut Job, action: Action, thought: &str) -> Result<bool, EngineError> {
        self.tick(&turn.id, if thought.trim().is_empty() { crate::event::doing(&action) } else { short(thought) });
        let new_file = matches!(&action, Action::WriteFile { path, .. } if !Path::new(&turn.folder).join(path).exists());
        let mut outcome = match &action {
            Action::Wait { seconds } => self.wait(*seconds),
            Action::SetSetting { key, value } => self.apply_setting(key, value)?,
            Action::Watch { name, reason, urgent, when, command } => self.watch(turn, name, reason, *urgent, when, command)?,
            Action::Unwatch { name } => self.unwatch(name)?,
            Action::Delegate { helpers } => self.delegate(turn, helpers),
            _ => {
                let exec = self.executor_for(turn)?;
                let mut o = exec.execute(&turn.id, &action)?;
                let image = o.image.take();
                if self.model.sees() { self.image = image; }
                else if let Some(i) = o.detail.find(executor::atspi::SCREEN_INSTEAD) {
                    o.detail.truncate(i);
                    o.detail.push_str(Self::CANNOT_SEE);
                }
                o
            }
        };
        if outcome.ok { self.after_ok(turn, &action)?; }
        if outcome.ok && new_file { self.note_new_file(turn, &action, thought)?; }
        let key = serde_json::to_string(&action).unwrap_or_default();
        let same = 1 + turn.steps.iter().rev()
            // `starts_with`: a stored detail may carry the warning or a project's notes after it.
            .take_while(|s| !s.ok && s.detail.starts_with(&outcome.detail) && serde_json::to_string(&s.action).unwrap_or_default() == key).count();
        // The same file written again and again counts as the same action whatever it says: a 27B
        // rewrote style.css until the owner stopped it (2026-09-25). append_file parts are fine.
        let key_of = |a: &Action| match a { Action::WriteFile { path, .. } => format!("write_file {path}"), a => serde_json::to_string(a).unwrap_or_default() };
        let again = if matches!(action, Action::Key { .. } | Action::Scroll { .. } | Action::Wait { .. } | Action::ProgramOutput { .. }) { 0 }
            else { 1 + turn.steps.iter().rev().take_while(|s| key_of(&s.action) == key_of(&action)).count() };
        let n = again.max(if outcome.ok { 0 } else { same });
        if n >= Self::SAME_FAIL_WARN {
            outcome.detail.push_str(&if outcome.ok {
                if matches!(action, Action::WriteFile { .. }) { format!(" (you have written this file {n} times in a row: read it or run it to see what is wrong, then change it with edit_file)") }
                else { format!(" (you have done exactly this {n} times in a row: it shows nothing new, do something different)") }
            } else { format!(" (this has failed {n} times in a row: do something different)") });
        }
        outcome.detail.push_str(&self.notes_on_first_touch(turn, &action)?);
        turn.steps.push(StepRecord { plan_step: 0, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
        self.emit(Event::Step { job_id: turn.id.clone(), plan_step: 0, text: describe(&action), ok: outcome.ok });
        self.store.push_message("result", &format!("{} -> {}: {}", prompt::compact_action(&action), if outcome.ok { "ok" } else { "failed" }, outcome.detail))?;
        if n >= Self::SAME_FAIL_END {
            let text = if outcome.ok { format!("I stopped: \"{}\" {n} times in a row, without getting anywhere.", describe(&action)) }
                else { format!("I stopped: {} failed {n} times in a row. {}", describe(&action), short(&outcome.detail)) };
            self.end(turn, State::Failed, text)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// What a successful action changes beyond its result: an install is recorded, a BLUEPRINT.md
    /// outside the projects folder registers its folder as a project.
    fn after_ok(&mut self, turn: &Job, action: &Action) -> Result<(), EngineError> {
        if let Action::RunCommand { argv } = action {
            if let Err(e) = crate::machine::record(self.store.conn(), argv) { eprintln!("engine: install not recorded ({e})"); }
        }
        if let Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::AppendFile { path, .. } = action {
            let p = Path::new(&turn.folder).join(path);
            if p.file_name().is_some_and(|f| f == "BLUEPRINT.md") {
                if let Some(folder) = p.parent() {
                    if !folder.starts_with(self.projects_root()?) && self.store.list_projects()?.iter().all(|r| Path::new(&r.folder) != folder) {
                        let name = sanitize_project_name(&folder.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
                        self.store.upsert_project(&name, &folder.display().to_string(), "")?;
                    }
                }
            }
        }
        Ok(())
    }

    /// A new file in a project goes into its notes at once, in the AI's own words for it, so a run
    /// that is stopped still leaves them current (the owner, 2026-09-25). A project with no notes
    /// yet is left to write its own.
    fn note_new_file(&self, turn: &Job, action: &Action, thought: &str) -> Result<(), EngineError> {
        let Action::WriteFile { path, .. } = action else { return Ok(()) };
        let file = Path::new(&turn.folder).join(path);
        if file.file_name().is_some_and(|f| f == "BLUEPRINT.md") { return Ok(()) }
        let Some(p) = self.project_of(&file)? else { return Ok(()) };
        let bp = Path::new(&p.folder).join("BLUEPRINT.md");
        let Ok(mut notes) = std::fs::read_to_string(&bp) else { return Ok(()) };
        if !notes.is_empty() && !notes.ends_with('\n') { notes.push('\n'); }
        if !notes.lines().any(|l| l.trim() == "## Files") { notes.push_str("\n## Files\n"); }
        let rel = file.strip_prefix(&p.folder).unwrap_or(&file).display().to_string();
        // One line, so a thought of several cannot break the list.
        let why = thought.split_whitespace().collect::<Vec<_>>().join(" ");
        notes.push_str(&format!("- {rel}{}\n", if why.is_empty() { String::new() } else { format!(": {why}") }));
        if let Err(e) = std::fs::write(&bp, notes) { eprintln!("engine: the new file was not noted in {} ({e})", bp.display()); }
        Ok(())
    }

    /// Every project (`core::projects`).
    fn projects(&self) -> Result<Vec<ProjectRow>, EngineError> {
        Ok(crate::projects::list(&self.store, &self.default_root)?)
    }

    /// The project a path is under, if any.
    fn project_of(&self, path: &Path) -> Result<Option<ProjectRow>, EngineError> {
        Ok(self.projects()?.into_iter().find(|p| path.starts_with(&p.folder)))
    }

    fn notes_of(p: &ProjectRow, cap: usize) -> String {
        match std::fs::read_to_string(Path::new(&p.folder).join("BLUEPRINT.md")) {
            Ok(t) if t.chars().count() > cap => format!("{}… (cut; read_file for the rest)", t.chars().take(cap).collect::<String>()),
            Ok(t) => t,
            Err(_) => "(no BLUEPRINT.md yet)".into(),
        }
    }

    /// The notes of the projects the message names, for the context block (at most two).
    fn named_notes(&self, words: &str) -> Result<String, EngineError> {
        let named: Vec<ProjectRow> = self.projects()?.into_iter().filter(|p| prompt::named_in(p, words)).take(2).collect();
        Ok(named.iter().map(|p| format!("Notes of the project {} ({}/BLUEPRINT.md):\n{}\n{}", p.name, p.folder, Self::notes_of(p, 1500), files_of(&p.folder))).collect::<Vec<_>>().join("\n"))
    }

    /// A project's notes, the first time in a turn an action reaches into its folder.
    fn notes_on_first_touch(&mut self, turn: &mut Job, action: &Action) -> Result<String, EngineError> {
        let mut out = String::new();
        for path in paths_in(action, &turn.folder) {
            let Some(p) = self.project_of(&path)? else { continue };
            if turn.projects_seen.contains(&p.name) { continue; }
            turn.projects_seen.push(p.name.clone());
            if self.chat_project.is_none() { self.chat_project = Some(p.folder.clone()); }
            out.push_str(&match std::fs::read_to_string(Path::new(&p.folder).join("BLUEPRINT.md")) {
                Ok(_) => format!("\n[{} is a project; its notes, {}/BLUEPRINT.md:]\n{}\n{}", p.name, p.folder, Self::notes_of(&p, 2000), files_of(&p.folder)),
                Err(_) => format!("\n[{} ({}) is a project with no BLUEPRINT.md yet: write one there — what it is, how it is built and run, where it stands.]\n{}", p.name, p.folder, files_of(&p.folder)),
            });
        }
        Ok(out)
    }

    /// A project the turn changed without touching its notes: one reminder before the reply.
    fn notes_not_updated(&self, turn: &Job) -> Result<Option<String>, EngineError> {
        let mut changed: Vec<ProjectRow> = vec![];
        let mut noted: Vec<PathBuf> = vec![];
        let all = self.projects()?;
        for s in turn.steps.iter().filter(|s| s.ok) {
            let (Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::AppendFile { path, .. }) = &s.action else { continue };
            let p = Path::new(&turn.folder).join(path);
            if p.file_name().is_some_and(|f| f == "BLUEPRINT.md") { if let Some(d) = p.parent() { noted.push(d.to_path_buf()); } continue; }
            if let Some(proj) = all.iter().find(|r| p.starts_with(&r.folder)) { if changed.iter().all(|c| c.folder != proj.folder) { changed.push(proj.clone()); } }
        }
        Ok(changed.into_iter().find(|p| !noted.iter().any(|n| n == Path::new(&p.folder))).map(|p| format!(
            "Before you finish: you changed {} ({}) but not its notes. Bring {}/BLUEPRINT.md up to date so the next conversation knows where it stands, then reply again.", p.name, p.folder, p.folder)))
    }

    fn write_journal(&self, turn: &Job, how: &str, text: &str) {
        let line = journal_line(turn, how, text);
        let wrote = std::fs::create_dir_all(&self.housekeeping_dir).and_then(|_| std::fs::OpenOptions::new().create(true).append(true).open(self.journal_path()))
            .and_then(|mut f| std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes()));
        if let Err(e) = wrote { eprintln!("engine: the journal was not written ({e})"); }
    }
    /// Gives something time to happen (one-loop design §1b). Stop, or a word from the user, cuts
    /// it short; the flag is left set so the loop ends the turn as a Stop.
    fn wait(&mut self, seconds: u32) -> Outcome {
        let n = seconds.clamp(1, 300);
        let mut waited = 0;
        while waited < n {
            if self.stop.load(Ordering::SeqCst) { break; }
            if !self.in_alert && (self.yield_.load(Ordering::SeqCst) || self.inbox.lock().unwrap().as_ref().is_some_and(|v| !v.is_empty())) { break; }
            std::thread::sleep(std::time::Duration::from_secs(1));
            waited += 1;
        }
        let running = executor::programs::running();
        let now = if running.is_empty() { String::new() } else { format!(" Running in the background now: {}.", running.join(", ")) };
        Outcome::ok(format!("waited {waited} s of {n}.{now}"))
    }

    /// Pieces of work to role helpers, all at once, spread over the cloud accounts (helpers
    /// design): each account's own model first, then the other models its key opens.
    fn delegate(&mut self, turn: &Job, tasks: &[executor::action::HelperTask]) -> Outcome {
        let accounts = self.model.helper_accounts();
        if accounts.is_empty() { return Outcome::err("helpers need Cloud on with an OpenRouter or NVIDIA account: do the work yourself") }
        if tasks.is_empty() || tasks.len() > 6 { return Outcome::err("delegate takes 1 to 6 helpers") }
        if let Some(t) = tasks.iter().find(|t| !crate::helpers::ROLES.contains(&t.role.as_str())) {
            return Outcome::err(format!("no helper role '{}'; the roles are {}", t.role, crate::helpers::ROLES.join(", ")))
        }
        let mut models = accounts.clone();
        for a in &accounts {
            for m in self.model.other_models(a) {
                if !models.iter().any(|x| x.url == a.url && x.model == m) { models.push(crate::cloud::Account { model: m, ..a.clone() }) }
            }
        }
        self.tick(&turn.id, format!("{} helper{} working…", tasks.len(), if tasks.len() == 1 { "" } else { "s" }));
        let (id, stop, folder, dir) = (turn.id.clone(), self.stop.clone(), PathBuf::from(&turn.folder), self.model.cloud_dir());
        let reports = crate::helpers::run(tasks, &models, accounts.len(), dir.as_deref(), &folder, &stop, &mut |text| self.emit(Event::Step { job_id: id.clone(), plan_step: 0, text, ok: true }));
        let text = format!("the helpers are back:\n{}", crate::helpers::summary(&reports));
        if reports.iter().any(|r| r.done) { Outcome::ok(text) } else { Outcome::err(text) }
    }

    /// The machine hand works in the user's home.
    fn executor_for(&self, turn: &Job) -> Result<Executor<Box<dyn Worker>>, EngineError> {
        let ws = PathBuf::from(&turn.folder);
        let log = match &self.log_path { Some(p) => ActionLog::open(p)?, None => ActionLog::open_in_memory()? };
        let (machine, desktop) = (self.workers)(&ws);
        Ok(Executor::new(machine, desktop, log, ws))
    }

    /// The files a turn wrote, for the card's Open buttons.
    fn files_written(turn: &Job) -> Vec<ChangedFile> {
        let mut v: Vec<ChangedFile> = vec![];
        for s in turn.steps.iter().filter(|s| s.ok) {
            let (Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::AppendFile { path, .. }) = &s.action else { continue };
            let p = Path::new(&turn.folder).join(path).display().to_string();
            if v.iter().any(|f| f.path == p) { continue; }
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            v.push(ChangedFile { kind: FileKind::of(&p), path: p, size });
        }
        v.truncate(20);
        v
    }

    /// The end of a turn that did something: the card closes, the journal gets its line, and a
    /// finished or failed turn gets its learning turn.
    fn end(&mut self, turn: &mut Job, state: State, text: String) -> Result<(), EngineError> {
        // The inbox shuts before the learning turn (it can take minutes): what came in is kept
        // for the next turn, and a word sent after this is a new turn of its own. A parked turn's
        // inbox stays open under an alert.
        if !self.in_alert { let left = self.inbox.lock().unwrap().take().unwrap_or_default(); self.pending.extend(left); }
        self.image = None;
        turn.state = state;
        turn.outcome_text = text.clone();
        if !turn.steps.is_empty() {
            let how = match state { State::Done => "done", State::Cancelled => "stopped", _ => "not finished" };
            self.write_journal(turn, how, &text);
        }
        // What the model did not say itself joins the chat, so the next message knows.
        if state == State::Failed { self.store.push_message("result", &format!("(the work ended: {text})"))?; }
        if state == State::Cancelled { self.store.push_message("result", "(the user stopped the work)")?; }
        let job_id = turn.id.clone();
        let files = Self::files_written(turn);
        self.emit(match state {
            State::Done => Event::Done { job_id, text, check: None, files, windows: windows_worked(turn) },
            State::Cancelled => Event::Stopped { job_id, text, files },
            _ => Event::Failed { job_id, text, files },
        });
        if state != State::Cancelled {
            // A to-do-only turn has nothing to learn from, but the rail's spinner stops only on a
            // `Learned` (`cards::busy_after`), so it gets an empty one (final review, 2026-09-24).
            if turn.steps.is_empty() { self.emit(Event::Learned { job_id: turn.id.clone(), lines: vec![], pending: false }); }
            else { self.learn(turn, state == State::Done); }
        }
        Ok(())
    }

    /// Clear pressed while a turn worked: run by the engine thread after the stopped turn has
    /// written its last rows, so none of them leak into the fresh chat (final review, 2026-09-24).
    /// No "stop" word was queued behind this stop, so the user's next real one must be answered;
    /// and nothing said before the Clear may run after it.
    pub fn clear(&mut self) -> Result<(), EngineError> {
        self.just_stopped = false;
        self.chat_project = None;
        self.pending.clear();
        Ok(self.store.forget_chat()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moves::TodoItem;
    use crate::testing::{engine_with, events_of};

    fn reply(t: &str) -> Move { Move::Reply { thought: String::new(), text: t.into(), outcome: Ending::Done } }
    fn act(a: Action) -> Move { Move::Act { thought: "doing it".into(), action: a } }
    fn run(cmd: &str) -> Action { Action::RunCommand { argv: vec![cmd.into()] } }

    #[test]
    fn a_chat_message_is_a_line_and_no_card() {
        let (mut e, _, _) = engine_with(vec![reply("hello")], "chat");
        let ev = events_of(&mut e, "hi");
        assert_eq!(ev, vec![Event::Said { text: "hello".into() }]);
    }

    #[test]
    fn delete_it_sees_what_it_was_and_where_it_is() {
        // The owner, 2026-09-23: "show me my projects", "delete it", "where is it".
        let (mut e, rec, root) = engine_with(vec![], "delete-it");
        let folder = root.join("projects").join("new-project");
        std::fs::create_dir_all(&folder).unwrap();
        let rm = run(&format!("rm -rf {}", folder.display()));
        e.model = crate::model::FakeModel::new(vec![reply("You have one: new-project."), act(rm.clone()), reply("Deleted new-project.")]);
        e.handle("show me my projects").unwrap();
        let ev = events_of(&mut e, "delete it");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[0].user.contains(&folder.display().to_string()), "where things are names the folder: {}", prompts[0].user);
        assert!(prompts[1].history.iter().any(|m| m.content == "show me my projects"), "{:?}", prompts[1].history);
        assert!(rec.calls.borrow().contains(&rm));
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { text, .. } if text == "Deleted new-project."), "{ev:?}");
    }

    #[test]
    fn a_new_project_is_just_work_and_its_notes_register_it() {
        let (mut e, _, root) = engine_with(vec![], "flappy");
        let bp = root.join("home").join("games").join("flappy").join("BLUEPRINT.md");
        e.model = crate::model::FakeModel::new(vec![
            act(Action::WriteFile { path: bp.display().to_string(), contents: "# Flappy\n".into() }),
            reply("Started Flappy in ~/games/flappy."),
        ]);
        let ev = events_of(&mut e, "create a new project Flappy in ~/games");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert!(!ev.iter().any(|v| matches!(v, Event::Understood { .. })), "no job types any more");
        assert!(e.store.list_projects().unwrap().iter().any(|p| p.name == "flappy"));
    }

    #[test]
    fn the_todo_list_is_on_the_card_and_pinned() {
        let (mut e, _, _) = engine_with(vec![
            Move::Todo { thought: String::new(), items: vec![TodoItem { text: "write it".into(), done: true }, TodoItem { text: "run it".into(), done: false }] },
            act(run("true")), reply("done"),
        ], "todo");
        let ev = events_of(&mut e, "build it");
        assert!(ev.iter().any(|v| matches!(v, Event::Plan { steps, .. } if steps == &vec!["[x] write it".to_string(), "[ ] run it".to_string()])), "{ev:?}");
        assert!(e.model.prompts.borrow()[1].user.contains("Your to-do list: [x] write it / [ ] run it"));
    }

    #[test]
    fn a_question_ends_the_turn_and_the_answer_is_the_next_message() {
        let (mut e, _, _) = engine_with(vec![
            Move::Ask { thought: String::new(), question: "Which language?".into(), options: vec!["Python".into(), "Rust".into()] },
            reply("Python it is."),
        ], "ask");
        let ev = events_of(&mut e, "write me a script");
        assert!(matches!(ev.last(), Some(Event::NeedsAnswer { questions, options, .. }) if questions[0] == "Which language?" && options[0].len() == 2), "{ev:?}");
        e.handle("Python").unwrap();
        let p = &e.model.prompts.borrow()[1];
        assert!(p.history.iter().any(|m| m.content.contains("Which language?")) && p.user.ends_with("Python"), "{p:?}");
        assert!(p.user.contains("working on: write me a script
(you asked) Which language?
(their answer) Python") && !p.allowed.contains(&"ask"), "{p:?}");
    }

    #[test]
    fn a_todo_list_is_not_sent_twice_in_a_row() {
        let todo = || Move::Todo { thought: String::new(), items: vec![TodoItem { text: "change it".into(), done: false }] };
        let (mut e, _, _) = engine_with(vec![todo(), todo(), act(run("true")), reply("done")], "todo twice");
        let ev = events_of(&mut e, "change it");
        assert_eq!(ev.iter().filter(|v| matches!(v, Event::Plan { .. })).count(), 1, "{ev:?}");
        let ps = e.model.prompts.borrow();
        assert!(ps[0].allowed.contains(&"todo") && !ps[1].allowed.contains(&"todo") && !ps[2].allowed.contains(&"todo") && ps[3].allowed.contains(&"todo"));
    }

    #[test]
    fn a_remember_is_not_sent_twice_in_a_row() {
        let rem = || Move::Remember { thought: String::new(), text: "use Python".into() };
        let (mut e, _, _) = engine_with(vec![rem(), rem(), reply("ok")], "remember twice");
        e.handle("hello").unwrap();
        let ps = e.model.prompts.borrow();
        assert!(!ps[1].allowed.contains(&"remember") && !ps[2].allowed.contains(&"remember"));
        assert!(e.store.all_messages().unwrap().iter().any(|(r, t)| r == "result" && t.contains("you just did that")));
    }

    #[test]
    fn after_acting_on_an_answer_a_new_question_may_come() {
        let q = || Move::Ask { thought: String::new(), question: "Which scope?".into(), options: vec!["All".into()] };
        let (mut e, _, _) = engine_with(vec![q(), act(run("true")), q()], "ask after act");
        e.handle("list my projects").unwrap();
        let ev = events_of(&mut e, "All");
        assert!(matches!(ev.last(), Some(Event::NeedsAnswer { .. })), "{ev:?}");
        let ps = e.model.prompts.borrow();
        assert!(!ps[1].allowed.contains(&"ask") && ps[2].allowed.contains(&"ask"));
    }

    #[test]
    fn an_answered_question_is_not_asked_again() {
        let q = || Move::Ask { thought: String::new(), question: "Which scope?".into(), options: vec!["All".into()] };
        let (mut e, _, _) = engine_with(vec![q(), q(), reply("Here they are."), q()], "ask twice");
        e.handle("list my projects").unwrap();
        let ev = events_of(&mut e, "All");
        assert_eq!(ev, vec![Event::Said { text: "Here they are.".into() }]);
        // A Clear forgets the question: the next words are a request of their own, and may be asked about.
        e.store.forget_chat().unwrap();
        let ev = events_of(&mut e, "something new");
        assert!(matches!(ev.last(), Some(Event::NeedsAnswer { .. })), "{ev:?}");
        assert!(e.model.prompts.borrow()[3].user.contains("working on: something new
"));
    }

    #[test]
    fn stop_ends_the_turn_and_its_queued_word_says_nothing_more() {
        let (mut e, _, _) = engine_with(vec![act(run("true"))], "stop");
        e.stop_flag().store(true, Ordering::SeqCst);
        let ev = events_of(&mut e, "make p");
        assert!(matches!(ev.last(), Some(Event::Stopped { .. })), "{ev:?}");
        assert!(events_of(&mut e, "stop").is_empty(), "the stop was answered by the Stopped card");
        assert_eq!(events_of(&mut e, "stop"), vec![Event::Said { text: "Nothing is running now.".into() }]);
    }

    #[test]
    fn the_same_failure_warns_at_three_and_ends_at_five() {
        let (mut e, rec, _) = engine_with((0..5).map(|_| act(run("make"))).collect(), "loop");
        rec.outcomes.borrow_mut().extend((0..5).map(|_| Outcome::err("no rule to make target")));
        let ev = events_of(&mut e, "build it");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("5 times")), "{ev:?}");
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("3 times in a row")), "{rows:?}");
    }

    #[test]
    fn the_same_action_warns_at_three_and_ends_at_five_even_when_it_works() {
        let (mut e, _, _) = engine_with((0..5).map(|_| act(run("ls"))).collect(), "repeat");
        let ev = events_of(&mut e, "look around");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("5 times")), "{ev:?}");
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("exactly this 3 times in a row")), "{rows:?}");
    }

    #[test]
    fn a_hundred_actions_is_the_cap() {
        let (mut e, _, _) = engine_with((0..101).map(|i| act(run(&format!("step{i}")))).collect(), "cap");
        let ev = events_of(&mut e, "go");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("100 actions")), "{ev:?}");
    }

    #[test]
    fn remember_is_kept_only_when_the_user_asks_for_it() {
        let rem = |t: &str| Move::Remember { thought: String::new(), text: t.into() };
        let (mut e, _, _) = engine_with(vec![rem("use python3"), reply("ok"), rem("Blender is installed"), reply("ok")], "remember");
        e.handle("from now on always use python3").unwrap();
        e.handle("open blender").unwrap();
        assert_eq!(e.store.instructions().unwrap(), vec!["use python3".to_string()]);
    }

    #[test]
    fn an_unreadable_answer_gets_one_more_try_then_plain_words() {
        let (mut e, _, _) = engine_with(vec![], "unreadable");
        e.model.unreadable.set(2);
        let ev = events_of(&mut e, "hi");
        assert!(matches!(ev.last(), Some(Event::Said { text }) if text.contains("could not read my own answer twice")), "{ev:?}");
    }

    #[test]
    fn a_model_that_cannot_see_is_told_so_and_offered_no_screen() {
        let (mut e, _, _) = engine_with(vec![reply("a"), reply("b")], "sees");
        e.handle("hi").unwrap();
        e.model.sees.set(true);
        e.handle("hi").unwrap();
        let p = e.model.prompts.borrow();
        assert!(p[0].no_screen && p[0].system.contains("cannot see pictures"));
        assert!(!p[1].no_screen && p[1].system.contains("screen_look"));
    }

    #[test]
    fn a_job_open_from_before_the_update_is_closed_at_start() {
        let (mut e, _, _) = engine_with(vec![], "old-job");
        let mut old = Job::new("p", "/data/projects/p", "make p", false, "Starting p");
        old.state = State::Working;
        e.store.save_job(&old).unwrap();
        let ev = e.resume().unwrap();
        assert!(matches!(&ev[..], [Event::Said { text }] if text.contains("closed")), "{ev:?}");
        assert!(e.store.open_job().unwrap().is_none());
    }

    /// Runs `hook` once, on the first call, then answers from `moves`; `fail_after` calls in, it
    /// answers with an http error instead (a free cloud provider's 429).
    struct Hooked { inner: crate::model::FakeModel, hook: std::cell::RefCell<Option<Box<dyn FnOnce()>>>, calls: std::cell::Cell<usize>, fail_after: usize }
    impl Model for Hooked {
        fn next_move(&self, p: &Prompt) -> Result<Move, ModelError> {
            if let Some(h) = self.hook.borrow_mut().take() { h() }
            self.calls.set(self.calls.get() + 1);
            if self.calls.get() > self.fail_after && p.allowed != ["learn"] { return Err(ModelError::Http("429 too many requests".into())) }
            self.inner.next_move(p)
        }
    }
    fn hooked(moves: Vec<Move>, fail_after: usize) -> Hooked {
        Hooked { inner: crate::model::FakeModel::new(moves), hook: Default::default(), calls: Default::default(), fail_after }
    }

    /// Streams: says it has 12 words, after `before` runs once (a Stop pressed mid-answer).
    struct Streaming { inner: crate::model::FakeModel, before: std::cell::RefCell<Option<Box<dyn FnOnce()>>> }
    impl Model for Streaming {
        fn next_move(&self, p: &Prompt) -> Result<Move, ModelError> { self.inner.next_move(p) }
        fn next_move_watched(&self, p: &Prompt, watch: &mut dyn FnMut(crate::model::Heard) -> bool) -> Result<Move, ModelError> {
            if let Some(b) = self.before.borrow_mut().take() { b() }
            if !watch(crate::model::Heard { thinking: 0, writing: 12 }) { return Err(ModelError::Stopped) }
            self.inner.next_move(p)
        }
    }

    #[test]
    fn a_long_answer_shows_its_words_and_stop_cuts_it() {
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let s = seen.clone();
        let model = Streaming { inner: crate::model::FakeModel::new(vec![reply("hi"), act(run("true"))]), before: Default::default() };
        let (e, _, _) = crate::testing::engine_with_model(model, "streaming");
        let mut e = e.with_sink(Box::new(move |ev: &Event| s.borrow_mut().push(ev.clone())));
        e.handle("hello").unwrap();
        assert!(seen.borrow().iter().any(|v| matches!(v, Event::Busy { text, .. } if text.contains("12 words"))), "{:?}", seen.borrow());
        let stop = e.stop_flag();
        *e.model.before.borrow_mut() = Some(Box::new(move || stop.store(true, Ordering::SeqCst)));
        let ev = e.handle_events("make it").unwrap();
        assert!(ev.iter().any(|v| matches!(v, Event::Stopped { .. })), "{ev:?}");
        assert_eq!(e.model.inner.prompts.borrow().len(), 1, "the stopped answer was not acted on");
    }

    /// Cut off at the runner's length limit once, then answers from `inner`.
    struct CutOnce { inner: crate::model::FakeModel, cut: std::cell::Cell<bool> }
    impl Model for CutOnce {
        fn next_move(&self, p: &Prompt) -> Result<Move, ModelError> {
            if !self.cut.replace(true) { return Err(ModelError::CutOff(900)) }
            self.inner.next_move(p)
        }
    }

    #[test]
    fn an_answer_cut_at_the_length_limit_is_told_to_write_in_parts() {
        let model = CutOnce { inner: crate::model::FakeModel::new(vec![reply("Written in parts.")]), cut: Default::default() };
        let (mut e, _, _) = crate::testing::engine_with_model(model, "cut-off");
        let ev = e.handle_events("write the whole page").unwrap();
        assert_eq!(ev, vec![Event::Said { text: "Written in parts.".into() }]);
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("cut off at the model's length limit after 900 words")), "{rows:?}");
    }

    #[test]
    fn a_word_sent_while_a_turn_is_stopped_runs_as_the_next_turn() {
        let (mut e, _, _) = crate::testing::engine_with_model(hooked(vec![act(run("true")), reply("blue it is")], usize::MAX), "late-word");
        let (inbox, stop) = (e.inbox(), e.stop_flag());
        *e.model.hook.borrow_mut() = Some(Box::new(move || {
            inbox.lock().unwrap().as_mut().unwrap().push("and make it blue".into());
            stop.store(true, Ordering::SeqCst);
        }));
        let ev = e.handle_events("make it").unwrap();
        assert!(ev.iter().any(|v| matches!(v, Event::Stopped { .. })), "{ev:?}");
        assert!(matches!(ev.last(), Some(Event::Said { text }) if text == "blue it is"), "{ev:?}");
        let prompts = e.model.inner.prompts.borrow();
        assert!(prompts.last().unwrap().user.ends_with("and make it blue"), "{}", prompts.last().unwrap().user);
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t == "(the user stopped the work)"), "{rows:?}");
    }

    #[test]
    fn a_todo_list_with_no_actions_still_closes_its_card() {
        let (mut e, _, _) = engine_with(vec![
            Move::Todo { thought: String::new(), items: vec![TodoItem { text: "think".into(), done: false }] },
            reply("nothing needed doing"),
        ], "todo-only");
        let ev = events_of(&mut e, "check it");
        // Done then an empty Learned: the rail's spinner stops only on Learned (final review).
        assert!(matches!(&ev[ev.len() - 2..], [Event::Done { text, .. }, Event::Learned { lines, pending: false, .. }] if text == "nothing needed doing" && lines.is_empty()), "{ev:?}");
    }

    #[test]
    fn two_hundred_moves_without_getting_anywhere_end_the_turn() {
        let todo = || Move::Todo { thought: String::new(), items: vec![TodoItem { text: "think".into(), done: false }] };
        let (mut e, _, _) = engine_with((0..201).map(|_| todo()).collect(), "went-round");
        let ev = events_of(&mut e, "go");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("went round")), "{ev:?}");
        assert_eq!(e.model.prompts.borrow().iter().filter(|p| p.allowed != ["learn"]).count(), 200);
    }

    #[test]
    fn the_screen_offered_by_the_desktop_hand_reaches_only_a_model_that_sees() {
        for sees in [false, true] {
            let look = Action::Look { window: Some("Blender".into()), find: None };
            let (mut e, rec, _) = engine_with(vec![act(look), reply("ok")], if sees { "screen-sees" } else { "screen-blind" });
            e.model.sees.set(sees);
            let detail = format!("nothing that lists its controls is called Blender. {} The screen: squares 1-48.", executor::atspi::SCREEN_INSTEAD);
            rec.desktop_outcomes.borrow_mut().push_back(Outcome { image: Some(vec![1, 2, 3]), ..Outcome::ok(detail) });
            e.handle("work in blender").unwrap();
            let p = &e.model.prompts.borrow()[1];
            assert_eq!(p.image.is_some(), sees);
            assert_eq!(p.user.contains("screen_look"), sees, "{}", p.user);
            if !sees { assert!(p.user.contains("this model cannot see the screen"), "{}", p.user); }
        }
    }

    #[test]
    fn a_model_error_mid_work_closes_the_card() {
        let (mut e, _, _) = crate::testing::engine_with_model(hooked(vec![act(run("true"))], 1), "http-error");
        let ev = e.handle_events("go").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("could not go on") && text.contains("429")), "{ev:?}");
        assert!(matches!(ev.last(), Some(Event::Learned { .. })), "{ev:?}");
    }

    #[test]
    fn keep_what_you_learned_is_answered_without_the_model() {
        let (mut e, _, _) = engine_with(vec![], "keep");
        assert_eq!(e.handle("keep what you learned").unwrap(), vec!["There is nothing waiting to be kept or discarded.".to_string()]);
    }

    #[test]
    fn a_projects_notes_come_with_its_first_touch_only() {
        let (mut e, _, root) = engine_with(vec![], "touch");
        let p = root.join("projects").join("site");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("BLUEPRINT.md"), "a site for car rentals").unwrap();
        let read = Action::ReadFile { path: p.join("index.html").display().to_string(), from_line: None, lines: None };
        e.model = crate::model::FakeModel::new(vec![act(read.clone()), act(read), reply("read it")]);
        e.handle("look at my page").unwrap();
        let rows = e.store.all_messages().unwrap();
        let results: Vec<&String> = rows.iter().filter(|(r, _)| r == "result").map(|(_, t)| t).collect();
        assert!(results[0].contains("a site for car rentals"), "{results:?}");
        assert!(!results[1].contains("a site for car rentals"), "once per turn");
    }

    #[test]
    fn a_project_named_in_the_message_brings_its_notes() {
        let (mut e, _, root) = engine_with(vec![reply("ok"), reply("hi")], "named");
        let p = root.join("projects").join("car-rental");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("BLUEPRINT.md"), "rentals by the day").unwrap();
        e.handle("carry on with the car rental site").unwrap();
        e.handle("hi").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[0].user.contains("rentals by the day"));
        assert!(!prompts[1].user.contains("rentals by the day"), "a greeting brings up no project");
    }

    #[test]
    fn a_projects_files_come_with_its_notes() {
        let (mut e, _, root) = engine_with(vec![reply("ok")], "files-listed");
        let p = root.join("projects").join("pool-game");
        std::fs::create_dir_all(p.join("assets")).unwrap();
        std::fs::write(p.join("BLUEPRINT.md"), "a pool table").unwrap();
        std::fs::write(p.join("index.html"), "<html></html>").unwrap();
        std::fs::write(p.join("style.css"), "body{}").unwrap();
        e.handle("lets continue working with the pool game").unwrap();
        let user = &e.model.prompts.borrow()[0].user;
        assert!(user.contains("Files there: BLUEPRINT.md (12 bytes), assets/, index.html (13 bytes), style.css (6 bytes)."), "{user}");
    }

    #[test]
    fn the_same_file_written_again_and_again_warns_at_three_and_ends_at_five() {
        let w = |i: usize| act(Action::WriteFile { path: "style.css".into(), contents: format!("body {{ margin: {i}px }}") });
        let (mut e, _, _) = engine_with((0..5).map(w).collect(), "rewrite");
        let ev = events_of(&mut e, "style the page");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text.contains("5 times")), "{ev:?}");
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("written this file 3 times in a row")), "{rows:?}");
    }

    #[test]
    fn a_new_file_in_a_project_goes_into_its_notes_at_once() {
        let (mut e, _, root) = engine_with(vec![], "noted-at-once");
        let p = root.join("projects").join("pool-game");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("BLUEPRINT.md"), "# Pool game\n").unwrap();
        let write = |f: &str, why: &str| Move::Act { thought: why.into(), action: Action::WriteFile { path: p.join(f).display().to_string(), contents: "x".into() } };
        e.model = crate::model::FakeModel::new(vec![write("game.js", "The rules and the shots."), write("game.js", "Fix the shot."), write("style.css", " "), reply("done"), reply("done")]);
        e.handle("make the pool game").unwrap();
        assert_eq!(std::fs::read_to_string(p.join("BLUEPRINT.md")).unwrap(), "# Pool game\n\n## Files\n- game.js: The rules and the shots.\n- style.css\n");
    }

    #[test]
    fn a_project_in_a_group_folder_is_found_by_its_notes() {
        let (mut e, _, root) = engine_with(vec![reply("ok")], "nested");
        let p = root.join("projects").join("WEb Games").join("Pool Game");
        std::fs::create_dir_all(&p).unwrap();
        std::fs::write(p.join("BLUEPRINT.md"), "a pool table in the browser").unwrap();
        std::fs::create_dir_all(root.join("projects").join("loose").join("src")).unwrap();
        std::fs::create_dir_all(root.join("projects").join("WEb Games").join("Chess")).unwrap();
        let names: Vec<String> = e.projects().unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Chess", "Pool Game", "loose"], "a group's sibling with no notes yet still counts");
        e.handle("lets continue our work with the pool web game").unwrap();
        assert!(e.model.prompts.borrow()[0].user.contains("a pool table in the browser"));
    }

    #[test]
    fn naming_another_project_starts_a_fresh_chat() {
        let (mut e, _, root) = engine_with(vec![reply("a"), reply("b"), reply("c")], "switch");
        for n in ["pool-game", "car-rental"] {
            let p = root.join("projects").join(n);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("BLUEPRINT.md"), format!("notes of {n}")).unwrap();
        }
        e.handle("continue the pool game").unwrap();
        e.handle("hi").unwrap();
        let ev = events_of(&mut e, "now the car rental site");
        assert!(ev.iter().any(|v| matches!(v, Event::Said { text } if text.contains("fresh chat for car-rental"))), "{ev:?}");
        let ps = e.model.prompts.borrow();
        assert!(ps[1].history.iter().any(|m| m.content.contains("continue the pool game")), "a greeting keeps the chat");
        assert!(!ps[2].history.iter().any(|m| m.content.contains("continue the pool game")) && !ps[2].user.contains("continue the pool game"));
        assert!(ps[2].user.contains("notes of car-rental"));
    }

    #[test]
    fn a_restart_starts_a_fresh_chat_and_keeps_the_standing_instructions() {
        let (mut e, _, _) = engine_with(vec![], "restart");
        e.store.push_message("user", "old talk").unwrap();
        e.store.add_instruction("always use Python").unwrap();
        e.resume().unwrap();
        assert!(e.store.all_messages().unwrap().is_empty());
        assert_eq!(e.store.instructions().unwrap(), ["always use Python"]);
    }

    #[test]
    fn a_change_to_a_project_without_its_notes_gets_one_reminder() {
        let (mut e, _, root) = engine_with(vec![], "remind");
        let p = root.join("projects").join("game");
        std::fs::create_dir_all(&p).unwrap();
        e.model = crate::model::FakeModel::new(vec![
            act(Action::WriteFile { path: p.join("game.js").display().to_string(), contents: "x".into() }),
            reply("done"), reply("done, really"),
        ]);
        let ev = events_of(&mut e, "add birds");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { text, .. } if text == "done, really"), "{ev:?}");
        assert!(e.store.all_messages().unwrap().iter().any(|(r, t)| r == "result" && t.contains("but not its notes")));
    }

    #[test]
    fn a_turn_that_worked_leaves_a_journal_line_the_next_chat_about_it_reads() {
        let (mut e, _, root) = engine_with(vec![], "journal");
        let game = root.join("home").join("Projects").join("game.js");
        e.model = crate::model::FakeModel::new(vec![
            act(Action::WriteFile { path: game.display().to_string(), contents: "x".into() }), reply("made it"),
            reply("found it"), reply("hello"),
        ]);
        e.handle("make a flappy bird game").unwrap();
        e.store.forget_chat().unwrap();
        e.handle("delete any flappy bird plans").unwrap();
        e.handle("hi, what is new").unwrap();
        let prompts = e.model.prompts.borrow();
        let n = prompts.len();
        assert!(prompts[n - 2].user.contains("What earlier jobs did") && prompts[n - 2].user.contains("game.js"), "{}", prompts[n - 2].user);
        assert!(!prompts[n - 1].user.contains("What earlier jobs did"));
    }

    #[test]
    fn words_sent_mid_work_join_the_next_step() {
        // A model that, on its first call, has the user say something more.
        struct Chatty { inner: crate::model::FakeModel, inbox: std::cell::RefCell<Option<Inbox>> }
        impl Model for Chatty {
            fn next_move(&self, p: &Prompt) -> Result<Move, ModelError> {
                if let Some(i) = self.inbox.borrow_mut().take() { i.lock().unwrap().as_mut().unwrap().push("make it blue".into()); }
                self.inner.next_move(p)
            }
        }
        let rec = crate::testing::Recorder::default();
        let root = crate::testing::temp_root("chatty");
        for d in ["housekeeping", "projects", "home"] { std::fs::create_dir_all(root.join(d)).unwrap(); }
        let model = Chatty { inner: crate::model::FakeModel::new(vec![act(run("true")), reply("blue it is")]), inbox: Default::default() };
        let mut e = Engine::new(Store::open_in_memory().unwrap(), model, root.join("projects"), None, crate::testing::scripted_workers(&rec), root.join("housekeeping")).with_home(root.join("home"));
        *e.model.inbox.borrow_mut() = Some(e.inbox());
        e.handle("draw a bird").unwrap();
        let p = e.model.inner.prompts.borrow();
        assert!(p[1].user.contains("(added while you worked) make it blue"), "{}", p[1].user);
        assert!(e.inbox().lock().unwrap().is_none(), "closed when the turn ends");
    }

    #[test]
    fn wait_ends_early_on_stop() {
        let (mut e, _, _) = engine_with(vec![], "wait");
        e.stop_flag().store(true, Ordering::SeqCst);
        let t = std::time::Instant::now();
        let o = e.wait(300);
        assert!(t.elapsed() < std::time::Duration::from_secs(3) && o.detail.starts_with("waited 0 s"), "{}", o.detail);
        assert!(e.stop_flag().load(Ordering::SeqCst), "the flag is left for the loop to end the turn");
    }

    #[test]
    fn delegate_without_cloud_says_so() {
        let (mut e, _, _) = engine_with(vec![act(Action::Delegate { helpers: vec![executor::action::HelperTask { role: "coding".into(), task: "t".into() }] }), reply("Done myself.")], "delegate");
        events_of(&mut e, "build it with helpers");
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("helpers need Cloud")), "{rows:?}");
        assert!(!e.model.prompts.borrow()[0].helpers, "no cloud: delegate is not offered");
    }

    #[test]
    fn a_reply_that_promises_work_is_told_to_do_it() {
        assert!(promises_work("I'm reviewing the current files and all design documents together now"));
        assert!(promises_work("I'll summarize it next."));
        assert!(promises_work("I cannot delete that project"));
        assert!(promises_work("I could not inspect the full blueprint because this turn exposed no filesystem action tool."));
        assert!(promises_work("I'm unable to inspect the files because no file or command tools are available."));
        assert!(!promises_work("Hi! I'm ready to help. What would you like to do?"));
        assert!(!promises_work("Done: the game now has a score board."));
        assert!(!promises_work("I cannot forget that; press Clear."));
    }

    #[test]
    fn journal_lines_are_found_by_telling_words_only() {
        let j = "- done: asked \"make a flappy bird game\" → made it. Paths: /home/g/Projects/game.js.\n- done: asked \"a car rental site\" → built.\n";
        assert!(journal_for(j, "delete the flappy files").contains("flappy bird game"));
        assert!(!journal_for(j, "delete the flappy files").contains("car rental"));
        assert_eq!(journal_for(j, "delete any project files"), "");
    }

    #[test]
    fn watch_makes_the_watcher_the_owner_asked_for_and_unwatch_removes_it() {
        let w = |when: &str| act(Action::Watch { name: "time".into(), reason: "say the time".into(), urgent: false, when: when.into(), command: vec![] });
        let (mut e, _, _) = engine_with(vec![w("every 30 seconds"), w("every 2 minutes"), reply("You will hear the time.")], "watch");
        let ev = events_of(&mut e, "every 2 minutes tell me the time");
        let rows = e.store.all_messages().unwrap();
        assert!(rows.iter().any(|(r, t)| r == "result" && t.contains("failed") && t.contains("every N minutes")), "{rows:?}");
        let got = crate::watchers::get(e.store.conn(), "time").unwrap().unwrap();
        assert!(got.made_by.starts_with("you (\"every 2 minutes") && got.every_s == 120, "{got:?}");
        assert!(ev.iter().any(|v| matches!(v, Event::Watchers { watchers } if watchers.len() == 1 && watchers[0].when == "every 2 min")), "{ev:?}");
        e.model = crate::model::FakeModel::new(vec![act(Action::Unwatch { name: "time".into() }), reply("Stopped.")]);
        e.handle("stop telling me the time").unwrap();
        assert!(crate::watchers::list(e.store.conn()).unwrap().is_empty());
    }

    #[test]
    fn a_watcher_the_ai_sets_itself_is_the_ais_and_every_prompt_lists_them() {
        let (mut e, _, _) = engine_with(vec![
            act(Action::Watch { name: "site".into(), reason: "the build ends; check it".into(), urgent: false, when: "every 5 minutes".into(), command: vec!["ls".into()] }),
            reply("ok"), reply("hi"),
        ], "watch-ai");
        e.handle("build the site").unwrap();
        assert!(crate::watchers::get(e.store.conn(), "site").unwrap().unwrap().made_by.starts_with("AI"));
        e.handle("hello").unwrap();
        assert!(e.model.prompts.borrow().last().unwrap().user.contains("Your watchers: site (every 5 min)."));
    }

    fn alert(w: &str, urgent: bool) -> crate::watchers::Alert {
        crate::watchers::Alert { watcher: w.into(), text: "AAPL is at 180".into(), reason: "I expect it to drop; tell the user".into(), urgent, made_by: "AI".into() }
    }

    /// Runs its next hook before each call, then answers from `inner`.
    struct Staged { inner: crate::model::FakeModel, hooks: std::cell::RefCell<std::collections::VecDeque<Box<dyn FnOnce()>>> }
    impl Model for Staged {
        fn next_move(&self, p: &Prompt) -> Result<Move, ModelError> {
            if let Some(h) = self.hooks.borrow_mut().pop_front() { h() }
            self.inner.next_move(p)
        }
    }

    #[test]
    fn an_alert_is_a_turn_in_its_own_chat_and_the_owners_chat_is_left_as_it_was() {
        let (mut e, _, _) = engine_with(vec![reply("hi"), reply("Told them."), reply("still here")], "alert-chat");
        e.handle("hello").unwrap();
        let before = e.store.all_messages().unwrap();
        let ev = e.handle_alert(&alert("price", false)).unwrap();
        assert!(matches!(&ev[0], Event::Alert { watcher, urgent: false, .. } if watcher == "price"), "{ev:?}");
        assert!(ev.iter().any(|v| matches!(v, Event::Done { text, .. } if text == "Told them.")), "the alert's card closes: {ev:?}");
        {
            let ps = e.model.prompts.borrow();
            assert!(ps[1].user.contains("An alert woke you. Watcher: price (made by AI; not urgent)") && ps[1].user.contains("I expect it to drop"), "{}", ps[1].user);
            assert!(!ps[1].history.iter().any(|m| m.content == "hello") && !ps[1].allowed.contains(&"ask"), "{:?}", ps[1]);
        }
        assert_eq!(e.store.all_messages().unwrap(), before);
        let left: i64 = e.store.conn().query_row("SELECT COUNT(*) FROM messages WHERE chat != 'main'", [], |r| r.get(0)).unwrap();
        assert_eq!(left, 0, "an alert's chat goes when its turn ends");
        e.handle("are you there").unwrap();
        assert!(e.model.prompts.borrow()[2].history.iter().any(|m| m.content == "hello"));
    }

    #[test]
    fn an_urgent_alert_parks_the_turn_and_it_carries_on_with_its_list_and_steps() {
        let todo = Move::Todo { thought: String::new(), items: vec![TodoItem { text: "build it".into(), done: false }] };
        let model = Staged { inner: crate::model::FakeModel::new(vec![todo, act(run("true")), reply("Told them."), reply("built")]), hooks: Default::default() };
        let (mut e, _, _) = crate::testing::engine_with_model(model, "urgent");
        let (q, y) = (e.alerts(), e.yield_flag());
        let raise: Box<dyn FnOnce()> = Box::new(move || { crate::watchers::enqueue(&mut q.lock().unwrap(), alert("price", true)); y.store(true, Ordering::SeqCst); });
        e.model.hooks.borrow_mut().extend([Box::new(|| {}) as Box<dyn FnOnce()>, raise]);
        let ev = e.handle_events("build it").unwrap();
        let at = |f: &dyn Fn(&Event) -> bool| ev.iter().position(f).unwrap_or_else(|| panic!("{ev:?}"));
        let step = at(&|v| matches!(v, Event::Step { .. }));
        let alerted = at(&|v| matches!(v, Event::Alert { urgent: true, .. }));
        let done = at(&|v| matches!(v, Event::Done { text, .. } if text == "built"));
        assert!(step < alerted && alerted < done, "{ev:?}");
        // The parked turn's card comes back with its list after the alert's closed.
        let Event::Plan { job_id: parked, steps: list } = &ev[at(&|v| matches!(v, Event::Plan { .. }))] else { unreachable!() };
        let told = at(&|v| matches!(v, Event::Done { text, .. } if text == "Told them."));
        let back = ev.iter().rposition(|v| matches!(v, Event::Plan { job_id, steps } if job_id == parked && steps == list)).unwrap();
        assert!(alerted < told && told < back && back < done, "{ev:?}");
        let ps = e.model.inner.prompts.borrow();
        let last = ps.iter().rev().find(|p| p.allowed != ["learn"]).unwrap();
        assert!(last.user.contains("Your to-do list: [ ] build it") && last.user.contains("urgent alert was handled in between: price"), "{}", last.user);
        assert!(last.history.iter().any(|m| m.content.contains("run_command")), "its steps are still in its chat");
        assert!(e.alerts().lock().unwrap().is_empty());
    }

    #[test]
    fn stop_during_an_urgent_alert_ends_the_parked_turn_too() {
        let model = Staged { inner: crate::model::FakeModel::new(vec![act(run("true")), act(run("ls")), reply("never")]), hooks: Default::default() };
        let (mut e, _, _) = crate::testing::engine_with_model(model, "urgent-stop");
        let (q, y, stop) = (e.alerts(), e.yield_flag(), e.stop_flag());
        let raise: Box<dyn FnOnce()> = Box::new(move || { crate::watchers::enqueue(&mut q.lock().unwrap(), alert("price", true)); y.store(true, Ordering::SeqCst); });
        let press: Box<dyn FnOnce()> = Box::new(move || stop.store(true, Ordering::SeqCst));
        e.model.hooks.borrow_mut().extend([raise, press]);
        let ev = e.handle_events("build it").unwrap();
        assert_eq!(ev.iter().filter(|v| matches!(v, Event::Stopped { .. })).count(), 2, "{ev:?}");
        assert_eq!(e.model.inner.prompts.borrow().len(), 2);
        assert!(e.handle_events("stop").unwrap().is_empty(), "one Stop answered both");
    }

    #[test]
    fn a_watcher_set_during_an_alert_is_the_ais() {
        let (mut e, _, _) = engine_with(vec![
            act(Action::Watch { name: "price".into(), reason: "keep an eye on it".into(), urgent: false, when: "every 5 minutes".into(), command: vec![] }),
            reply("Updated."),
        ], "alert-watch");
        e.handle_alert(&alert("price", false)).unwrap();
        assert!(crate::watchers::get(e.store.conn(), "price").unwrap().unwrap().made_by.starts_with("AI"));
    }

    #[test]
    fn an_alert_bringing_the_owners_watcher_up_to_date_leaves_it_the_owners() {
        let (mut e, _, _) = engine_with(vec![
            act(Action::Watch { name: "time".into(), reason: "say the time; they liked it".into(), urgent: false, when: "every 2 minutes".into(), command: vec![] }),
            reply("Told them."),
        ], "alert-rewatch");
        let mine = crate::watchers::build("time", "say the time", false, "you (\"every 2 minutes tell me the time\")", "every 2 minutes", vec![]).unwrap();
        crate::watchers::put(e.store.conn(), &mine, 0).unwrap();
        e.handle_alert(&alert("time", false)).unwrap();
        let got = crate::watchers::get(e.store.conn(), "time").unwrap().unwrap();
        assert!(got.made_by.starts_with("you (") && got.reason == "say the time; they liked it", "{got:?}");
    }

    #[test]
    fn an_alert_leaves_what_waits_for_the_owners_keep() {
        let (mut e, _, _) = engine_with(vec![act(run("true")), reply("Told them.")], "alert-keep");
        let n = crate::notes::Note { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox".into(), ..Default::default() };
        crate::notes::put(e.store.conn(), &n, Some("an-older-job")).unwrap();
        e.handle_alert(&alert("price", false)).unwrap();
        let waiting: i64 = e.store.conn().query_row("SELECT COUNT(*) FROM notes_pending", [], |r| r.get(0)).unwrap();
        assert_eq!(waiting, 1, "the alert did an action and learned, and the owner's entry still waits");
    }

    #[test]
    fn a_project_an_alert_reaches_into_is_not_the_owners_chat_project() {
        let (mut e, _, root) = engine_with(vec![], "alert-project");
        for n in ["pool-game", "car-rental"] {
            let p = root.join("projects").join(n);
            std::fs::create_dir_all(&p).unwrap();
            std::fs::write(p.join("BLUEPRINT.md"), format!("notes of {n}")).unwrap();
        }
        let page = root.join("projects/pool-game/index.html").display().to_string();
        e.model = crate::model::FakeModel::new(vec![reply("a"), act(Action::ReadFile { path: page, from_line: None, lines: None }), reply("Told them."), reply("c")]);
        e.handle("hi").unwrap();
        e.handle_alert(&alert("price", false)).unwrap();
        let ev = events_of(&mut e, "now the car rental site");
        assert!(!ev.iter().any(|v| matches!(v, Event::Said { text } if text.contains("fresh chat"))), "{ev:?}");
        assert!(e.store.all_messages().unwrap().iter().any(|(r, t)| r == "user" && t == "hi"), "the owner's chat keeps its words");
    }
}
