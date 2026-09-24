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

/// Whether the user's own words ask for something to hold from now on. The model filled
/// `remember` on its own — a project's description, "Blender is installed" — and every one of
/// those notes pulled the next chat back to the old work (the owner, 2026-09-23).
/// ponytail: a word list; a note the user wanted in other words is lost, and they say it again.
fn asks_to_keep(text: &str) -> bool {
    let t = text.to_lowercase();
    ["always", "never", "from now on", "remember", "keep in mind", "every time", "whenever",
     "in future", "in the future", "going forward", "by default"].iter().any(|w| t.contains(w))
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
        Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::ReadFile { path, .. } => vec![Path::new(folder).join(path)],
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
            stop: Arc::new(AtomicBool::new(false)), image: None, system: std::cell::OnceCell::new(), inbox: Arc::new(Mutex::new(None)), just_stopped: false, pending: vec![], asked: None, answered: false, chat_project: None }
    }

    /// Where commands run (tests point it at a temp folder).
    pub fn with_home(mut self, home: PathBuf) -> Self { self.home = home; self }

    /// The service's handle on words sent while a turn works.
    pub fn inbox(&self) -> Inbox { self.inbox.clone() }

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
        if let Err(e) = crate::notes::discard_pending(self.store.conn()) { eprintln!("engine: an old proposal could not be discarded ({e})"); }
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
        self.store.forget_messages()?;
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
        let mut reminded = false;
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
            for w in self.take_inbox(false) { self.heard(turn, &w)?; }
            if turn.steps.len() >= Self::MAX_ACTIONS {
                return self.end(turn, State::Failed, format!("I stopped after {} actions without finishing.", Self::MAX_ACTIONS));
            }
            if moves >= Self::MAX_MOVES {
                let text = Self::WENT_ROUND.to_string();
                if turn.steps.is_empty() && turn.plan.is_empty() { self.emit(Event::Said { text }); return Ok(()); }
                return self.end(turn, State::Failed, text);
            }
            moves += 1;
            let mut p = self.prompt_for(turn)?;
            if let Some(m) = last { p.allowed.retain(|a| *a != m); }
            self.tick(&turn.id, "Thinking…".into());
            let mv = match self.model.next_move(&p) {
                Ok(m) => { unreadable = 0; m }
                Err(ModelError::BadJson(e)) if unreadable == 0 => {
                    unreadable = 1;
                    self.store.push_message("result", &not_a_move(&e))?;
                    continue;
                }
                Err(ModelError::BadJson(_)) => {
                    let text = "(I could not read my own answer twice — the model is not answering in the required format. Please say that again, or switch the model.)".to_string();
                    if turn.steps.is_empty() && turn.plan.is_empty() { self.emit(Event::Said { text }); return Ok(()); }
                    return self.end(turn, State::Failed, text);
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
                Move::Reply { text, outcome, .. } => {
                    if !reminded {
                        if let Some(note) = self.notes_not_updated(turn)? {
                            reminded = true;
                            self.store.push_message("result", &note)?;
                            continue;
                        }
                    }
                    let more = self.take_inbox(true);
                    if !more.is_empty() { for w in more { self.heard(turn, &w)?; } continue; }
                    // A to-do list already put a card up: it closes like work did.
                    if turn.steps.is_empty() && turn.plan.is_empty() { self.emit(Event::Said { text }); return Ok(()); }
                    return self.end(turn, if outcome == Ending::CouldNot { State::Failed } else { State::Done }, text);
                }
                Move::Ask { .. } if self.answered => {
                    self.store.push_message("result", "the user already answered your question: work with that answer, or reply")?;
                }
                Move::Ask { question, options, .. } => {
                    let more = self.take_inbox(true);
                    if !more.is_empty() { for w in more { self.heard(turn, &w)?; } continue; }
                    self.image = None;
                    if !turn.steps.is_empty() { self.write_journal(turn, "asked a question", &question); }
                    self.asked = Some(format!("{}
(you asked) {question}
(their answer) ", turn.request));
                    self.emit(Event::NeedsAnswer { job_id: turn.id.clone(), questions: vec![question], options: vec![options] });
                    return Ok(());
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
                    let note = if asks_to_keep(&turn.request) {
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
        let system = prompt::brief(sees);
        let (machine, places) = (self.machine_block(), self.places()?);
        let (instructions, journal, notes) = (self.store.instructions()?, self.journal(&turn.request), self.named_notes(&turn.request)?);
        let programs = executor::programs::running();
        let ctx = prompt::context_block(&prompt::Context { machine: &machine, places: &places, programs: &programs, instructions: &instructions,
            journal: &journal, notes: &notes, tips: &turn.notes_block, request: &turn.request, todo: &turn.plan });
        let budget = self.model.context_tokens().saturating_sub(1500 + (system.len() + ctx.len()) / 4);
        let mut chat = prompt::fit(&self.store.all_messages()?, budget);
        let last = chat.pop().map(|m| m.content).unwrap_or_default();
        Ok(Prompt { system, user: format!("{ctx}\n{last}"), history: chat, allowed: if self.answered { vec!["reply", "todo", "act", "remember"] } else { vec!["reply", "ask", "todo", "act", "remember"] }, image: self.image.take(), no_screen: !sees })
    }

    /// Runs one action and puts its result in the conversation. `true`: the turn ended here.
    fn act(&mut self, turn: &mut Job, action: Action, thought: &str) -> Result<bool, EngineError> {
        self.tick(&turn.id, if thought.trim().is_empty() { crate::event::doing(&action) } else { short(thought) });
        let mut outcome = match &action {
            Action::Wait { seconds } => self.wait(*seconds),
            Action::SetSetting { key, value } => self.apply_setting(key, value)?,
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
        let key = serde_json::to_string(&action).unwrap_or_default();
        let same = 1 + turn.steps.iter().rev()
            // `starts_with`: a stored detail may carry the warning or a project's notes after it.
            .take_while(|s| !s.ok && s.detail.starts_with(&outcome.detail) && serde_json::to_string(&s.action).unwrap_or_default() == key).count();
        // The same action over and over, whatever it gives: a 27B enlarged one screen square 20
        // times and never clicked (the owner, 2026-09-24). Keys, scrolls, waits and a program's
        // output are fine to repeat.
        let again = if matches!(action, Action::Key { .. } | Action::Scroll { .. } | Action::Wait { .. } | Action::ProgramOutput { .. }) { 0 }
            else { 1 + turn.steps.iter().rev().take_while(|s| serde_json::to_string(&s.action).unwrap_or_default() == key).count() };
        let n = again.max(if outcome.ok { 0 } else { same });
        if n >= Self::SAME_FAIL_WARN {
            outcome.detail.push_str(&if outcome.ok { format!(" (you have done exactly this {n} times in a row: it shows nothing new, do something different)") }
                else { format!(" (this has failed {n} times in a row: do something different)") });
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
        if let Action::WriteFile { path, .. } | Action::EditFile { path, .. } = action {
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
        Ok(named.iter().map(|p| format!("Notes of the project {} ({}/BLUEPRINT.md):\n{}", p.name, p.folder, Self::notes_of(p, 1500))).collect::<Vec<_>>().join("\n"))
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
                Ok(_) => format!("\n[{} is a project; its notes, {}/BLUEPRINT.md:]\n{}", p.name, p.folder, Self::notes_of(&p, 2000)),
                Err(_) => format!("\n[{} ({}) is a project with no BLUEPRINT.md yet: write one there — what it is, how it is built and run, where it stands.]", p.name, p.folder),
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
            let (Action::WriteFile { path, .. } | Action::EditFile { path, .. }) = &s.action else { continue };
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
            if self.stop.load(Ordering::SeqCst) || self.inbox.lock().unwrap().as_ref().is_some_and(|v| !v.is_empty()) { break; }
            std::thread::sleep(std::time::Duration::from_secs(1));
            waited += 1;
        }
        let running = executor::programs::running();
        let now = if running.is_empty() { String::new() } else { format!(" Running in the background now: {}.", running.join(", ")) };
        Outcome::ok(format!("waited {waited} s of {n}.{now}"))
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
            let (Action::WriteFile { path, .. } | Action::EditFile { path, .. }) = &s.action else { continue };
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
        // for the next turn, and a word sent after this is a new turn of its own.
        let left = self.inbox.lock().unwrap().take().unwrap_or_default();
        self.pending.extend(left);
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
    fn journal_lines_are_found_by_telling_words_only() {
        let j = "- done: asked \"make a flappy bird game\" → made it. Paths: /home/g/Projects/game.js.\n- done: asked \"a car rental site\" → built.\n";
        assert!(journal_for(j, "delete the flappy files").contains("flappy bird game"));
        assert!(!journal_for(j, "delete the flappy files").contains("car rental"));
        assert_eq!(journal_for(j, "delete any project files"), "");
    }
}
