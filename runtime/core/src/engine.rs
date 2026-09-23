use crate::event::describe;
use crate::job::{Job, State, StepRecord};
use crate::model::{Model, ModelError};
use crate::moves::Move;
use crate::prompt;
use crate::store::{ProjectRow, Store, StoreError};
use aios_proto::{ChangedFile, Event, FileKind, JobState, StepView, Waiting};
use executor::action::Action;
use executor::executor::Executor;
use executor::log::{ActionLog, LogError};
use executor::worker::{Outcome, Worker};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

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

pub struct Engine<M: Model> {
    pub store: Store,
    pub model: M,
    /// Where a *new* project lands when the `projects_root` setting says nothing.
    default_root: PathBuf,
    log_path: Option<String>,
    workers: WorkerFactory,
    /// The workspace every housekeeping job runs in (it has no project folder).
    housekeeping_dir: PathBuf,
    /// Where events go the moment they happen (the service's broadcast). Default: nowhere.
    sink: Box<dyn FnMut(&Event)>,
    /// The events of the `handle_events` call in progress, returned at its end.
    out: Vec<Event>,
    /// Raised from outside the turn loop to cancel the open job between steps.
    stop: Arc<AtomicBool>,
    /// The screen hand's latest picture, for the next model turn only (2b): one image at a time is
    /// what fits the 8k budget. In memory, never in the stored job.
    image: Option<Vec<u8>>,
    /// The machine's system line, read once (it does not change while the engine runs).
    system: std::cell::OnceCell<String>,
}

/// What the job left behind: entries under `folder` modified at or after `since` (unix
/// seconds), hidden entries and dependency folders skipped, sorted by path, at most 20.
/// ponytail: mtime in whole seconds, not content — a file touched but unchanged is listed, and
/// so is one the user touched in the same second the job began.
pub(crate) fn changed_files(folder: &Path, since: u64) -> Vec<ChangedFile> {
    const SKIP: [&str; 2] = ["node_modules", "target"]; // hidden names (.git, .venv) are skipped below
    fn walk(dir: &Path, since: u64, out: &mut Vec<ChangedFile>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || SKIP.contains(&name.as_str()) { continue; }
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() { walk(&path, since, out); continue; }
            let mtime = meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
            if mtime >= since {
                let p = path.display().to_string();
                out.push(ChangedFile { kind: FileKind::of(&p), path: p, size: meta.len() });
            }
        }
    }
    let mut out = vec![];
    walk(folder, since, &mut out);
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out.truncate(20);
    out
}

/// The five actions that work a window (2a §3). Their world is the window, which moves on between
/// steps, so neither the "just succeeded" guard nor the "already failed" one holds over them.
fn on_the_desktop(action: &Action) -> bool {
    matches!(action, Action::Look { .. } | Action::Press { .. } | Action::Type { .. } | Action::Read { .. } | Action::OpenApp { .. }
        | Action::ScreenLook { .. } | Action::ScreenClick { .. } | Action::ScreenType { .. })
}

/// An `ask` that is really a request for permission. The owner's run (2026-09-19): "Is it okay
/// to proceed with installing the necessary packages?" three times over, each yes answered with
/// the same question and never an install. Nobody is asked before a step (full-access spec), so
/// the model is sent back to take it. Whole words, so "look to" is not "ok to".
fn asks_permission(q: &str) -> bool {
    let words: String = q.to_lowercase().chars().map(|c| if c.is_alphanumeric() || c == '\'' { c } else { ' ' }).collect();
    let padded = format!(" {} ", words.split_whitespace().collect::<Vec<_>>().join(" "));
    // Only asking for leave to act. "Would you like me to…" and "How would you like to handle
    // this?" are how the user takes part in the work and stay allowed (the owner, 2026-09-20):
    // a model that asks those is stuck, and what it was stuck on is fixed where it happened.
    ["okay to", "ok to", "all right to", "alright to", "may i", "shall i go", "go ahead", "your permission",
     "should i proceed", "can i proceed", "shall i proceed", "want me to proceed", "should i go ahead"]
        .iter().any(|p| padded.contains(&format!(" {p} ")))
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

/// What to call a job in a line the user reads. A housekeeping job has no project, so
/// `job.project` is the empty string — "Stopped the job in ." is not a sentence.
/// What the model is told when its answer was not a move (the owner's LM Studio run, 2026-09-19:
/// a thinking Qwen wrote `{"understood":…,"act":{…}}`, which the runner did not stop).
/// A reply that says it is about to act. A question back ("I need to know…") is not one.
// ponytail: phrase list, a small model's promises in English; widen it when a live one slips past.
fn promises_work(text: &str) -> bool {
    let t = text.to_lowercase().replace('’', "'");
    ["i will ", "i'll ", "let me ", "i am going to ", "i'm going to ", "proceeding"].iter().any(|w| t.contains(w))
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

/// A project job making itself another home: a BLUEPRINT.md outside its folder, or a new
/// projects_root. The owner's run, 2026-09-23: Flappy Bird planned "create the project folder
/// and set projects_root", wrote its blueprint to /opt/flappy_bird, and circled on step 1.
/// ponytail: only the blueprint is watched; the rest of a stray project follows it back.
fn strays(job: &Job, action: &Action) -> bool {
    match action {
        Action::SetSetting { key, .. } => key == "projects_root",
        Action::WriteFile { path, .. } | Action::EditFile { path, .. } => {
            let p = Path::new(path);
            p.is_absolute() && p.file_name().is_some_and(|f| f == "BLUEPRINT.md") && p.parent() != Some(Path::new(&job.folder))
        }
        _ => false,
    }
}

/// A written file where the last thing done was writing that same file: nothing new happened, and
/// the step it claims is not done. The owner's run, 2026-09-23: "list the folder" and "delete the
/// folder" were each a `write_file` of the folder's own path, and every one ticked its step.
/// A write after something else — a run that failed, say — is the ordinary fix-and-retry loop.
fn rewrites_last(job: &Job, action: &Action) -> bool {
    let Action::WriteFile { path, .. } = action else { return false };
    let same = |a: &Action| matches!(a, Action::WriteFile { path: p, .. } if p == path);
    job.steps.iter().rev().find(|s| !(same(&s.action) && !s.ok)).is_some_and(|s| s.ok && same(&s.action))
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

fn not_a_move(allowed: &[&str], error: &str) -> String {
    let moves = if allowed.is_empty() { String::new() } else { format!(", one of: {}", allowed.join(", ")) };
    format!("your answer was not a move ({}). Answer with one JSON object whose first key is \"move\"{moves}.", error.chars().take(120).collect::<String>())
}

/// The live status line, in words the owner reads: which plan step of how many, and what is
/// happening this second. No plan yet (asking, planning) means no number to give.
fn step_line(job: &Job, step: usize, what: &str) -> String {
    if job.plan.is_empty() { return what.to_string() }
    format!("Step {} of {}: {what}", step.clamp(1, job.plan.len()), job.plan.len())
}

/// A plan step is a sentence the model wrote; the status line is one line under the cards.
fn short(s: &str) -> String {
    if s.chars().count() <= 60 { return s.to_string() }
    format!("{}…", s.chars().take(60).collect::<String>().trim_end())
}

fn display_name(job: &Job) -> &str {
    if job.housekeeping { "housekeeping" } else { &job.project }
}

impl<M: Model> Engine<M> {
    pub fn new(store: Store, model: M, default_root: PathBuf, log_path: Option<String>, workers: WorkerFactory, housekeeping_dir: PathBuf) -> Self {
        Self { store, model, default_root, log_path, workers, housekeeping_dir, sink: Box::new(|_| {}), out: vec![], stop: Arc::new(AtomicBool::new(false)), image: None, system: std::cell::OnceCell::new() }
    }

    /// What this machine has, ahead of every prompt (machine-map spec §2). Programs and installs
    /// are scanned each time, so something installed shows on the very next turn.
    fn places(&self) -> Result<String, EngineError> {
        let home = std::env::var("HOME").unwrap_or_else(|_| "unknown".into());
        // A project whose folder is gone is not somewhere things are.
        let live: Vec<ProjectRow> = self.store.list_projects()?.into_iter().filter(|p| Path::new(&p.folder).is_dir()).collect();
        Ok(places(&home, &self.projects_root()?, &self.housekeeping_dir, &live))
    }

    fn machine_block(&self) -> String {
        crate::machine::current_block(self.store.conn(), self.system.get_or_init(crate::machine::system_line))
    }

    /// A clone of the engine's own stop flag — raise it to cancel the open job between steps.
    pub fn stop_flag(&self) -> Arc<AtomicBool> { self.stop.clone() }

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

    /// I3: a store error here used to be swallowed (`.ok().flatten()`) — a job left unreadable
    /// (a corrupt row, a locked file) looked exactly like "no job open", so the engine would
    /// silently start a fresh one over it. Propagate instead.
    pub fn open_job(&self) -> Result<Option<Job>, EngineError> { Ok(self.store.open_job()?) }

    /// The open job as a front door needs to draw it (1d §2.2).
    pub fn state(&self) -> Result<Option<JobState>, EngineError> {
        Ok(self.open_job()?.map(|job| JobState {
            id: job.id.clone(),
            name: display_name(&job).to_string(),
            housekeeping: job.housekeeping,
            understood: job.understood.clone(),
            plan: job.plan.clone(),
            steps: job.steps[job.plan_from.min(job.steps.len())..].iter().map(|s| StepView { plan_step: s.plan_step, text: crate::event::describe(&s.action), ok: s.ok }).collect(),
            waiting: match job.state {
                State::WaitingAnswer => Waiting::Answer { questions: job.pending_questions.clone(), options: job.pending_options.clone() },
                _ => Waiting::None,
            },
        }))
    }

    /// The job's own folder, as recorded on the job — never recomputed from the project name,
    /// so moving the root (or a project) cannot redirect a job that is already running.
    fn workspace(&self, job: &Job) -> PathBuf { PathBuf::from(&job.folder) }

    /// Where a new project's folder goes. A store error is propagated, not read as "unset":
    /// that would quietly put the project under the wrong root (I3's lesson).
    fn projects_root(&self) -> Result<PathBuf, EngineError> {
        Ok(match self.store.get_setting("projects_root")? {
            Some(p) => PathBuf::from(p),
            None => self.default_root.clone(),
        })
    }

    /// One executor per project folder: the machine hand works in the job's own folder.
    pub(crate) fn executor_for(&self, job: &Job) -> Result<Executor<Box<dyn Worker>>, EngineError> {
        let ws = self.workspace(job);
        let log = match &self.log_path { Some(p) => ActionLog::open(p)?, None => ActionLog::open_in_memory()? };
        let (machine, desktop) = (self.workers)(&ws);
        Ok(Executor::new(machine, desktop, log, ws))
    }

    pub(crate) fn read_blueprint(&self, job: &Job) -> Option<String> {
        std::fs::read_to_string(self.workspace(job).join("BLUEPRINT.md")).ok()
    }

    /// The tips a job starts with, fixed on the job (Phase 3 §3). Tips are a help, never a
    /// condition: a notes table that cannot be read starts the job without them.
    fn give_tips(&self, job: &mut Job, skills: &[String]) {
        job.skills.clear();
        for s in skills.iter().map(|s| crate::notes::norm_notebook(s)) {
            if !s.is_empty() && s != crate::notes::THIS_COMPUTER && !job.skills.contains(&s) && job.skills.len() < 3 { job.skills.push(s); }
        }
        match crate::notes::for_job(self.store.conn(), &format!("{} {}", job.goal, job.request), &job.skills) {
            Ok((block, shown)) => { job.notes_block = block; job.shown_notes = shown; }
            Err(e) => eprintln!("engine: no tips for this job ({e})"),
        }
    }

    /// One user message in, the lines to show the user out — the terminal's view of
    /// `handle_events`.
    pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        Ok(self.handle_events(text)?.iter().flat_map(crate::event::lines).collect())
    }

    /// The same, as typed events; each one reached the sink as it happened.
    pub fn handle_events(&mut self, text: &str) -> Result<Vec<Event>, EngineError> {
        self.store.push_message("user", text)?;
        self.out.clear();
        let r = self.handle_inner(text);
        // The events already reached the sink (and every client) even when `r` is an error, so the
        // conversation history must hold them too — record before propagating.
        let out = std::mem::take(&mut self.out);
        for l in out.iter().flat_map(crate::event::lines) { self.store.push_message("assistant", &l)?; }
        r?;
        Ok(out)
    }

    /// A job left mid-work (a restart): carry on with it. Nothing open → nothing emitted.
    pub fn resume(&mut self) -> Result<Vec<Event>, EngineError> {
        self.out.clear();
        let job = match self.open_job()? { Some(j) if matches!(j.state, State::Working | State::Planning | State::Asking) => j, _ => return Ok(vec![]) };
        // Same pre-1c guard `handle_inner` applies: a job with no recorded folder must not reach
        // the model, here either.
        let r = if job.folder.is_empty() { self.fail_predates_folder(job) } else { self.run_turns(job) };
        // Same invariant as `handle_events`: the events already reached the sink even when `r`
        // is an error, so the conversation history must hold them too — record before propagating.
        let out = std::mem::take(&mut self.out);
        for l in out.iter().flat_map(crate::event::lines) { self.store.push_message("assistant", &l)?; }
        r?;
        Ok(out)
    }

    /// A job saved before 1c recorded folders has none, so every one of its actions would run in
    /// the process's own directory. Fail it on sight — no model call, and not through `finish`,
    /// which would drop a LAST_RUN.md in that same directory. Shared by `handle_inner` and
    /// `resume`: either can find such a job open.
    fn fail_predates_folder(&mut self, mut job: Job) -> Result<(), EngineError> {
        job.state = State::Failed;
        job.outcome_text = "This job predates the folder record; please start it again.".into();
        self.store.save_job(&job)?;
        self.emit(Event::Failed { job_id: job.id.clone(), text: job.outcome_text, files: vec![] });
        // No learning turn, but the rail's spinner still waits for the Learned that ends one.
        self.emit(Event::Learned { job_id: job.id, lines: vec![], pending: false });
        Ok(())
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
        let open = self.open_job()?;
        if let Some(mut job) = open {
            // A job saved before 1c recorded folders has none, so every one of its actions would
            // run in the process's own directory. Fail it on sight — no model call, and not
            // through `finish`, which would drop a LAST_RUN.md in that same directory.
            if job.folder.is_empty() {
                return self.fail_predates_folder(job);
            }
            if is_stop(text) {
                // A stop reaches the engine through both doors at once: the service arms the flag
                // (it lands between steps) and queues the word (it lands here). Whichever arrives
                // first ends the job; clearing the flag here is what stops the other one from
                // announcing the same stop a second time.
                self.stop.store(false, Ordering::SeqCst);
                let message = format!("Stopped the job in {}.", display_name(&job));
                return self.finish(job, State::Cancelled, message);
            }
            match job.state {
                State::WaitingAnswer => {
                    let q = job.pending_questions.join(" / ");
                    job.answers.push((q, text.to_string()));
                    job.pending_questions.clear();
                    job.pending_options.clear();
                    job.state = if job.plan.is_empty() { State::Planning } else { State::Working };
                    job.rejections = 0;
                    self.store.save_job(&job)?;
                    return self.run_turns(job);
                }
                _ => {
                    // A job left mid-work (crash/restart): carry on with it.
                    return self.run_turns(job);
                }
            }
        }
        // Nothing open, and the word is stop: a fixed answer, never a model call. The front door
        // below is a `start`-capable grammar, so handing "stop" to a 9B could start a job with it.
        if is_stop(text) {
            self.emit(Event::Said { text: "Nothing is running now.".into() });
            return Ok(());
        }
        // Idle: the model decides — chat, or work.
        // Names to pick skills from, a help like the tips: unreadable means none listed, not no answer.
        let notebooks = crate::notes::notebooks(self.store.conn()).unwrap_or_else(|e| { eprintln!("engine: no notebooks for the front door ({e})"); vec![] });
        let mut p = prompt::front_door(&self.store.instructions()?, &self.store.list_projects()?, &notebooks, &self.store.recent_messages(4)?, text);
        p.user = format!("{}\n\n{}", self.machine_block(), p.user);
        self.tick("", "Working out what you are asking for…".into());
        // An answer that is not a move gets one more try with the reason; a second one is said in
        // plain words, never as "something went wrong".
        let mv = match self.model.next_move(&p) {
            Err(ModelError::BadJson(e)) => {
                p.user.push_str(&format!("\n\n{}", not_a_move(&p.allowed, &e)));
                match self.model.next_move(&p) {
                    Err(ModelError::BadJson(_)) => {
                        self.emit(Event::Said { text: "(I could not read my own answer twice — the model is not answering in the required format. Please say that again, or switch the model.)".into() });
                        return Ok(());
                    }
                    r => r?,
                }
            }
            r => r?,
        };
        // A reply runs nothing. One that promises work ("I will…", "Let me proceed") left the
        // owner waiting on nothing three times over; ask once more with only the working moves.
        let mv = match mv {
            Move::Reply { ref text, .. } if promises_work(text) => {
                p.user.push_str("\n\nYour reply said you would do something, but a reply runs nothing. Do it now: start or housekeep.");
                p.allowed = vec!["start", "housekeep"];
                match self.model.next_move(&p) {
                    Err(ModelError::BadJson(_)) => mv,
                    r => r?,
                }
            }
            mv => mv,
        };
        let keep = asks_to_keep(text);
        match mv {
            Move::Reply { text, remember } => {
                let remember = remember.filter(|_| keep);
                self.emit(Event::Said { text });
                if let Some(r) = remember {
                    self.store.add_instruction(&r)?;
                    self.emit(Event::Said { text: format!("(Noted for the future: {r})") });
                }
                Ok(())
            }
            Move::Start { project, new_project: _, description, goal, creative, understood, skills, remember, folder: named } => {
                let name = sanitize_project_name(&project);
                let existing = self.store.get_project(&name)?;
                let is_new = existing.is_none();
                // A folder the user named ("my site is in ~/work/site") is where the project is,
                // new to us or moved: it is registered there (the owner, 2026-09-23).
                let named = named.map(|f| match f.trim().strip_prefix("~/") {
                    Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
                    None => f.trim().to_string(),
                }).filter(|f| Path::new(f).is_absolute());
                // An existing project keeps the folder it was created in. Recomputing it from the
                // root re-homed the project on every start, overwriting the stored folder (1b bug).
                let folder = match (&named, &existing) {
                    (Some(f), _) => PathBuf::from(f),
                    (None, Some(p)) => PathBuf::from(&p.folder),
                    (None, None) => self.projects_root()?.join(&name),
                };
                // A folder that is already there is not this project's unless the user named it:
                // a new project never piles its files into someone else's. A known project's
                // folder that has gone is made again — every command would fail in it otherwise.
                let made = if is_new && named.is_none() && folder.exists() {
                    Err(std::io::Error::new(std::io::ErrorKind::AlreadyExists, format!("a folder already exists at {}; choose another project name", folder.display())))
                } else { std::fs::create_dir_all(&folder) };
                if let Err(e) = made {
                    self.emit(Event::Said { text: format!("I could not start *{name}*: {e}") });
                    return Ok(());
                }
                let desc = existing.map(|p| p.description).unwrap_or(description);
                let folder = folder.display().to_string();
                self.store.upsert_project(&name, &folder, &desc)?;
                // I6: `remember` on a `start` move was saved silently — only the `Reply` arm told
                // the user. Same "(Noted for the future: …)" line here, computed before the value
                // moves into `add_instruction`.
                let remember = remember.filter(|_| keep);
                let note = remember.as_ref().map(|r| format!("(Noted for the future: {r})"));
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                let mut job = Job::new(&name, &folder, &goal, creative, &understood);
                job.new_project = is_new;
                // The model's `goal` is its paraphrase; this is what the user actually said.
                job.request = text.to_string();
                self.give_tips(&mut job, &skills);
                self.store.save_job(&job)?;
                // Understood first, then the note, then the turns — the order today's lines have.
                self.emit(Event::Understood { job_id: job.id.clone(), name: display_name(&job).to_string(), text: understood, housekeeping: job.housekeeping });
                if let Some(n) = note { self.emit(Event::Said { text: n }); }
                self.run_turns(job)
            }
            // The machine itself: no project row, no blueprint, one shared scratch folder.
            Move::Housekeep { goal, understood, remember } => {
                let remember = remember.filter(|_| keep);
                let note = remember.as_ref().map(|r| format!("(Noted for the future: {r})"));
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                std::fs::create_dir_all(&self.housekeeping_dir)?;
                let mut job = Job::new_housekeeping(&self.housekeeping_dir.display().to_string(), &goal, &understood);
                job.request = text.to_string();
                self.give_tips(&mut job, &[]);
                self.store.save_job(&job)?;
                self.emit(Event::Understood { job_id: job.id.clone(), name: display_name(&job).to_string(), text: understood, housekeeping: job.housekeeping });
                if let Some(n) = note { self.emit(Event::Said { text: n }); }
                self.run_turns(job)
            }
            other => {
                self.emit(Event::Said { text: format!("(I answered out of turn — {other:?} — please say that again.)") });
                Ok(())
            }
        }
    }

    const MAX_STEPS: usize = 200;
    const MAX_FAILS_PER_STEP: usize = 3;
    const MAX_REJECTIONS: u32 = 2;
    /// `done` moves the blueprint gate may hold back before the job gives up (see `Job::done_gated`).
    const MAX_DONE_GATED: u32 = 4;
    const MAX_REPLANS: u32 = 5;

    /// Mark a job finished (done/failed/cancelled) and say in one line what it came to.
    fn finish(&mut self, mut job: Job, state: State, text: String) -> Result<(), EngineError> {
        // A picture of this job's screen must not open the next job's first turn.
        self.image = None;
        job.state = state;
        job.outcome_text = text.clone();
        self.store.save_job(&job)?;
        // Before `write_or_wipe_last_run`: that writes LAST_RUN.md into this same folder on
        // Failed/Cancelled, and the engine's own note is not a file the job changed.
        let files = changed_files(&self.workspace(&job), job.started_at);
        self.write_or_wipe_last_run(&job);
        let job_id = job.id.clone();
        let ev = match state {
            State::Done => {
                let check = job.steps.last().map(|s| format!("{}: {}", describe(&s.action), if s.ok { "ok" } else { "failed" }));
                Event::Done { job_id, text, check, files, windows: windows_worked(&job) }
            }
            State::Cancelled => Event::Stopped { job_id, text, files },
            // `finish` is only ever called with done/cancelled/failed; anything else ended badly.
            _ => Event::Failed { job_id, text, files },
        };
        self.emit(ev);
        if state != State::Cancelled { self.learn(&job, state == State::Done); }
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

    /// The bounded last-run note (1b spec §6.6): written from the record when a job ends
    /// badly, wiped when a job in the project is proven done. Never more than one file.
    fn write_or_wipe_last_run(&self, job: &Job) {
        // A housekeeping job has no project folder to leave a note in.
        if job.housekeeping { return; }
        let path = self.workspace(job).join("LAST_RUN.md");
        match job.state {
            State::Done => { let _ = std::fs::remove_file(&path); }
            State::Failed | State::Cancelled => {
                let last_fail = job.steps.iter().rev().find(|s| !s.ok);
                let note = format!(
                    "goal: {}\nended: {}\nfailed at plan step: {}\nlast error: {}\n",
                    job.goal,
                    job.outcome_text,
                    last_fail.map(|s| s.plan_step.to_string()).unwrap_or_else(|| "-".into()),
                    last_fail.map(|s| s.detail.clone()).unwrap_or_else(|| "-".into()),
                );
                if let Err(e) = std::fs::write(&path, note) { eprintln!("core: could not write LAST_RUN.md: {e}"); }
            }
            _ => {}
        }
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

    /// `true` means the job stopped here (it gave up) and the caller must return.
    fn reject(&mut self, job: &mut Job, why: &str) -> Result<bool, EngineError> {
        job.rejections += 1;
        job.note_to_model = Some(format!("your last move was rejected: {why}"));
        if job.rejections >= Self::MAX_REJECTIONS {
            let text = format!("I gave up on {}: I kept answering in a way the system could not accept ({why}).", display_name(job));
            self.finish(job.clone(), State::Failed, text)?;
            return Ok(true);
        }
        self.store.save_job(job)?;
        Ok(false)
    }

    /// I5: matching by file name alone let `docs/BLUEPRINT.md` satisfy the gate, even though
    /// `read_blueprint` only ever reads the root file — so `done` could unblock on a blueprint
    /// the model never actually consulted. Exact path match instead (after trimming one leading
    /// `./`, the only harmless alias for the root).
    fn is_blueprint(action: &Action) -> Option<bool> {
        match action {
            Action::WriteFile { path, .. } | Action::EditFile { path, .. } => {
                Some(path.strip_prefix("./").unwrap_or(path) == "BLUEPRINT.md")
            }
            _ => None,
        }
    }

    /// Whether a step that just succeeded leaves `failed` — one key out of `failed_actions` —
    /// worth another try, because the world it failed in is gone.
    ///
    /// A blueprint write or edit changed the disk, which every action reads, so it clears them
    /// all. A window action changed only the window: the ids a `look` hands out are the very
    /// thing "control 1 was never handed out" complained of, and a press or an `open_app` moves
    /// a window on — but none of that says anything about a `run_command` that failed, so those keep 1d's "that exact action already failed" memory in a mixed job.
    /// A `read` changes nothing and clears nothing.
    fn cleared_by(succeeded: &Action, failed: &str) -> bool {
        if Self::is_blueprint(succeeded).is_some() { return true; }
        // A command that worked changed the machine (an install, a mkdir): a command that failed
        // before may work now, as it would for anyone retrying after fixing the cause.
        if let Action::RunCommand { .. } = succeeded { return serde_json::from_str::<Action>(failed).is_ok_and(|f| !on_the_desktop(&f)); }
        if matches!(succeeded, Action::Read { .. }) || !on_the_desktop(succeeded) { return false; }
        serde_json::from_str::<Action>(failed).is_ok_and(|f| on_the_desktop(&f))
    }

    /// True when the user's stop has landed: the job is finished as Cancelled and reported.
    fn stopped(&mut self, job: &Job) -> Result<bool, EngineError> {
        if !self.stop.swap(false, Ordering::SeqCst) { return Ok(false); }
        let message = format!("Stopped the job in {}.", display_name(job));
        self.finish(job.clone(), State::Cancelled, message)?;
        Ok(true)
    }

    /// Run one action through the executor's door and record what happened on the job.
    /// `true` means the job must stop here (stopped / gave up) and the caller must return.
    ///
    /// `is_check`: a `done` move's proof action is exempt from the identical-action dedup
    /// below — a check is meant to be re-run verbatim after a fix elsewhere (that's the whole
    /// point of a check), unlike an `act` step repeating the same failed command with nothing
    /// changed. Both still count toward MAX_FAILS_PER_STEP, which caps a stuck check the same
    /// way it caps a stuck act.
    fn perform(&mut self, job: &mut Job, plan_step: usize, action: Action, is_check: bool) -> Result<bool, EngineError> {
        if self.stopped(job)? { return Ok(true); }
        let key = serde_json::to_string(&action).unwrap_or_default();
        // The very same action twice in a row, and it worked the first time: nothing changed in
        // between, so the second is the model not reading its own step list — the system prompt's
        // "a step that already succeeded is done". The live 1d run wrote the same `/etc` file nine
        // times this way and then had no steps left for the rest of the job. Only an *immediate*
        // repeat is caught, so re-running a command after something else has changed stays
        // legitimate, and a `done` check is exempt entirely: re-running one verbatim is its point.
        // A second press of the same button, or the same text typed again, is ordinary desktop
        // work (a repeated digit, a second Next): exempt like a check is (2a §5). *A second* is
        // all §5 claims, and all this exempts: the live 2a run pressed Main Menu eight times in a
        // row without looking once, toggling the one popover open and shut until the job ran out
        // of replans, and every press answered "pressed Main Menu" as if it had got somewhere.
        let trailing = job.steps.iter().rev().take_while(|s| s.ok && serde_json::to_string(&s.action).unwrap_or_default() == key).count();
        // A second look at the screen is waiting for a page or a program to come up (the owner's
        // run, 2026-09-19); a third in a row is not looking at what the first two showed.
        // A window look the same: the refusal below said "look at the window", which refused
        // every look after it until the job gave up.
        let repeatable = matches!(action, Action::Press { .. } | Action::Type { .. } | Action::ScreenLook { .. } | Action::Look { .. }) && trailing < 2;
        if !is_check && !repeatable && trailing >= 1 {
            // Pointed at the eye that can see what the action did: `look` sees no web page, so
            // "look at the window" after a screen click sent the owner's run round in circles.
            return self.reject(job, if matches!(action, Action::ScreenLook { .. }) {
                "you have looked at the screen twice and it is the same: act on what it shows (enlarge a square, click a spot) or replan"
            } else if matches!(action, Action::Look { .. }) {
                "you have looked at this window twice and it is the same: act on what it shows (press, type, read), look at the screen if the page is missing, or replan"
            } else if matches!(action, Action::ScreenClick { .. } | Action::ScreenType { .. }) {
                "that exact action already worked; screen_look to see what it did, then take the next step"
            } else if on_the_desktop(&action) {
                "that exact action already worked; look at the window to see what it did, then take the next step"
            } else {
                "that exact action just succeeded — its result is in the steps above; move on to the next step"
            });
        }
        // The same exemption, for the same reason, on the failing side. A desktop action's world
        // is the window, not the step list: it moves on between steps, so one that failed says
        // little about the next time — the live 2a run's `read control 1` failed for want of a
        // look and worked the moment one happened. Repeating it is an ordinary failed step, kept
        // in hand by MAX_FAILS_PER_STEP; three runs died instead on this rejection, whose budget
        // is two and fatal, one failed step into the job.
        if !is_check && !on_the_desktop(&action) && job.failed_actions.contains(&key) {
            let earlier = job.steps.iter().rev().find(|s| serde_json::to_string(&s.action).unwrap_or_default() == key).map(|s| s.detail.clone()).unwrap_or_default();
            return self.reject(job, &format!("that exact action already failed with: {earlier} — work around it or replan"));
        }
        let exec = self.executor_for(job)?;
        // `SetSetting` never reaches a worker (executor::lane -> Lane::Engine): the engine applies
        // it and records it with `log_only`. A failure counts like any other failed step. So does
        // a project job reaching for another home: it never runs.
        let local = match &action {
            _ if rewrites_last(job, &action) => Some(Outcome::err(
                "not done: you just wrote this same file, and writing it again does nothing new. write_file only makes a file; to list a folder run_command ls -la <folder>, to delete run_command rm -rf <path>, to make a folder run_command mkdir -p <folder>")),
            _ if !job.housekeeping && strays(job, &action) => Some(Outcome::err(format!(
                "not done: this project's folder is {} and is already made; write its files there with relative names (BLUEPRINT.md, src/main.py); projects_root is not this job's to change", job.folder))),
            Action::SetSetting { key: name, value } => Some(self.apply_setting(name, value)?),
            _ => None,
        };
        if let Some(outcome) = local {
            // Same invariant as `Executor::execute`: the setting is already written, so a logging
            // failure must not abort the turn and lose the step.
            if let Err(e) = exec.log_only(&job.id, &action, &format!("{}: {}", if outcome.ok { "ok" } else { "error" }, outcome.detail)) {
                eprintln!("core: failed to log a setting change: {e}");
            }
            job.rejections = 0;
            job.done_gated = 0;
            job.note_to_model = None;
            if outcome.ok { job.replans = 0; }
            job.steps.push(StepRecord { plan_step, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
            self.emit(Event::Step { job_id: job.id.clone(), plan_step, text: describe(&action), ok: outcome.ok });
            if !outcome.ok {
                job.failed_actions.push(key);
                let fails = job.steps[job.plan_from.min(job.steps.len())..].iter().filter(|s| s.plan_step == plan_step && !s.ok).count();
                if fails >= Self::MAX_FAILS_PER_STEP {
                    let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", display_name(job), outcome.detail);
                    self.finish(job.clone(), State::Failed, text)?;
                    return Ok(true);
                }
            }
            self.store.save_job(job)?;
            return Ok(false);
        }
        self.tick(&job.id, step_line(job, plan_step, &crate::event::doing(&action)));
        let mut outcome = exec.execute(&job.id, &action)?;
        self.image = outcome.image.take();
        job.rejections = 0;
        // Like `rejections`: the counter bounds a model that is getting nowhere, so an
        // action that actually ran clears it. Without this it is a lifetime count, and a
        // long job that trips the blueprint gate once after each of four code changes —
        // answering it correctly every time — would fail on the fourth.
        job.done_gated = 0;
        job.note_to_model = None;
        // A step that worked is progress: the replan bound counts replans in a row without
        // one (the owner's Django job, 2026-09-19, gave up after six replans spread over a
        // job that was getting somewhere).
        if outcome.ok { job.replans = 0; }
        job.steps.push(StepRecord { plan_step, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
        self.emit(Event::Step { job_id: job.id.clone(), plan_step, text: describe(&action), ok: outcome.ok });
        if outcome.ok {
            // What the scan cannot see (a tool with no window) is recorded as it is installed.
            if let Action::RunCommand { argv } = &action {
                if let Err(e) = crate::machine::record(self.store.conn(), argv) { eprintln!("engine: install not recorded ({e})"); }
            }
            // A housekeeping job has no blueprint, so neither counter means anything to it.
            if !job.housekeeping {
                match Self::is_blueprint(&action) {
                    Some(true) => job.last_blueprint_update = job.steps.len(),
                    Some(false) => job.last_code_change = job.steps.len(),
                    None => {}
                }
            }
            // The world is not the one those failures happened in, so re-running them is
            // legitimate (spike finding) — but only the ones this step's world covers.
            job.failed_actions.retain(|k| !Self::cleared_by(&action, k));
        } else {
            // Recorded even for a check (only the identical-action *lookup* above is
            // check-exempt): a later `act` proposing this same action must still see why
            // it already failed.
            job.failed_actions.push(key);
            let fails = job.steps[job.plan_from.min(job.steps.len())..].iter().filter(|s| s.plan_step == plan_step && !s.ok).count();
            if fails >= Self::MAX_FAILS_PER_STEP {
                let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", display_name(job), outcome.detail);
                self.finish(job.clone(), State::Failed, text)?;
                return Ok(true);
            }
        }
        self.store.save_job(job)?;
        Ok(false)
    }

    /// Turn after turn until the job is done, failed, or needs the user (1b spec §3–§4).
    fn run_turns(&mut self, mut job: Job) -> Result<(), EngineError> {
        // Asleep, the machine takes the job down with it: held while the turns run, let go when
        // the job ends or waits on the person.
        let _awake = executor::awake::hold("the AI is working on a job");
        loop {
            if self.stopped(&job)? { return Ok(()); }
            if job.steps.len() >= Self::MAX_STEPS {
                let text = format!("I gave up on {}: {} steps without finishing.", display_name(&job), Self::MAX_STEPS);
                return self.finish(job, State::Failed, text);
            }
            let last_run = if job.housekeeping { None } else { std::fs::read_to_string(self.workspace(&job).join("LAST_RUN.md")).ok() };
            let mut p = prompt::job_turn(&self.store.instructions()?, &job, self.read_blueprint(&job).as_deref(), last_run.as_deref());
            p.user = format!("{}\n{}\n\n{}", self.machine_block(), self.places()?, p.user);
            p.image = self.image.take();
            let step = job.steps.last().map(|s| s.plan_step).unwrap_or(1);
            let what = match job.plan.get(step.saturating_sub(1)) {
                Some(t) => format!("{} — working out the next move", short(t)),
                None => "working out how to do this".to_string(),
            };
            self.tick(&job.id, step_line(&job, step, &what));
            let mv = match self.model.next_move(&p) {
                // A runner that let the model write outside the grammar: a rejected move like any
                // other, told back to the model and bounded by the same two tries.
                Err(ModelError::BadJson(e)) => {
                    if self.reject(&mut job, &not_a_move(&p.allowed, &e))? { return Ok(()); }
                    continue;
                }
                r => r?,
            };
            let rejected = match (job.state, mv) {
                (State::Asking, Move::Ask { questions, options }) | (State::Working, Move::Ask { questions, options }) | (State::Planning, Move::Ask { questions, options }) if !job.creative => {
                    if questions.is_empty() {
                        Some("ask needs at least one question".to_string())
                    } else if questions.iter().any(|q| asks_permission(q)) {
                        Some("never ask for permission: you have full access, take the step itself".to_string())
                    } else {
                        job.pending_questions = questions.clone();
                        job.pending_options = options.clone();
                        job.state = State::WaitingAnswer;
                        job.rejections = 0;
                        job.note_to_model = None;
                        self.store.save_job(&job)?;
                        self.emit(Event::NeedsAnswer { job_id: job.id.clone(), questions, options });
                        return Ok(());
                    }
                }
                (_, Move::Ask { .. }) if job.creative => Some("this job is in creative mode: decide yourself instead of asking".to_string()),
                (_, Move::Ask { .. }) => Some("ask only before planning or while working".to_string()),
                (State::Asking, Move::Plan { steps }) | (State::Planning, Move::Plan { steps }) => {
                    if steps.is_empty() {
                        Some("plan needs at least one step".to_string())
                    } else {
                        job.plan = steps; job.state = State::Working; job.rejections = 0; job.note_to_model = None; job.plan_from = job.steps.len();
                        self.store.save_job(&job)?;
                        self.emit(Event::Plan { job_id: job.id.clone(), steps: job.plan.clone() });
                        None
                    }
                }
                (State::Working, Move::Plan { .. }) => Some("you already have a plan; use replan to change it".to_string()),
                (State::Working, Move::Replan { steps, why }) => {
                    job.replans += 1;
                    if job.replans > Self::MAX_REPLANS {
                        let text = format!("I gave up on {}: the plan kept changing ({} replans) without progress.", display_name(&job), job.replans);
                        return self.finish(job, State::Failed, text);
                    }
                    // Two replans that are not a change of plan, both seen in the live 2a run: one
                    // carrying the action the model wanted to take — `{"kind":"look",…}` as its
                    // single step — and one handing back the plan it already had. Each costs a
                    // replan, like every other, and the note says which move it wanted; neither
                    // becomes the plan. Not a rejection: that budget is two and fatal, and these
                    // are a model one nudge away from the right move, not one answering illegally.
                    let wrong_move = if steps.iter().any(|s| serde_json::from_str::<Action>(s).is_ok()) {
                        Some("that is an action, not a plan step; send it with act")
                    } else if steps == job.plan {
                        Some("that is the plan you already have; take its next step with act")
                    } else { None };
                    if let Some(note) = wrong_move {
                        job.note_to_model = Some(note.to_string());
                        self.store.save_job(&job)?;
                        None
                    } else {
                        job.plan = steps; job.rejections = 0; job.plan_from = job.steps.len();
                        job.note_to_model = Some(format!("plan revised because: {why}"));
                        self.store.save_job(&job)?;
                        self.emit(Event::Plan { job_id: job.id.clone(), steps: job.plan.clone() });
                        None
                    }
                }
                // A step the plan does not have ticks nothing on the card while the work goes on
                // unseen (the owner's run, 2026-09-23: "Step 5 of 5: working out how to do this").
                // Not a rejection (two are fatal): a 9B still counting from the old plan after a
                // replan is one nudge away. Bounded like a replan, which is what it costs.
                (State::Working, Move::Act { step, .. }) if step == 0 || step > job.plan.len() => {
                    job.replans += 1;
                    if job.replans > Self::MAX_REPLANS {
                        let text = format!("I gave up on {}: the plan kept changing ({} replans) without progress.", display_name(&job), job.replans);
                        return self.finish(job, State::Failed, text);
                    }
                    job.note_to_model = Some(format!("plan step {step} is not in the plan (1-{}): name the step this action serves, or replan to add one", job.plan.len()));
                    self.store.save_job(&job)?;
                    None
                }
                (State::Working, Move::Act { step, action }) => {
                    if self.perform(&mut job, step, action, false)? { return Ok(()); }
                    None
                }
                // A check proves the work, it does not do it: the owner's run said done with a
                // click on the Firefox icon as its check, "I will now click it", and the job ended
                // with the browser never opened.
                (State::Working, Move::Done { check: Action::Press { .. } | Action::Type { .. } | Action::OpenApp { .. } | Action::ScreenClick { .. } | Action::ScreenType { .. } | Action::WriteFile { .. } | Action::EditFile { .. } | Action::SetSetting { .. }, .. }) =>
                    Some("a check proves the work is done, it does not do the work: do that with act first, then say done with a check that only looks (run_command, read_file, screen_look, look, read)".to_string()),
                (State::Working, Move::Done { summary, check }) => {
                    // The absolute gate: a new project must leave a real BLUEPRINT.md behind,
                    // whether or not this job happened to change code — the counters only
                    // compare steps to each other, so a job that never wrote a file passed the
                    // old rule with no blueprint at all. Housekeeping has no project and no
                    // blueprint, so neither rule applies to it.
                    //
                    // Order: the step-counter rule first. Both rules are true at once whenever a
                    // new project has changed code and has no root blueprint yet; the counter
                    // rule is the older contract (I5's test pins its wording), and either message
                    // asks for the same next move. The absolute rule still catches every case the
                    // counters cannot see — including a new project that wrote no file at all.
                    let gate = if job.housekeeping {
                        None
                    } else if job.last_code_change > job.last_blueprint_update {
                        Some("update BLUEPRINT.md for what you changed before saying done".to_string())
                    } else if job.new_project && !self.workspace(&job).join("BLUEPRINT.md").exists() {
                        Some("create BLUEPRINT.md for this new project before saying done".to_string())
                    } else {
                        None
                    };
                    if let Some(why) = gate {
                        // A `done` the gate holds back is a legal move refused by policy, not an
                        // answer the system could not read — and what it asks for is one concrete extra step. The grammar
                        // budget of two leaves room for a single retry, and the live 1d run lost
                        // two jobs to a second `done` arriving before the blueprint write. Its
                        // own bound, so a model that only ever says done still ends.
                        job.done_gated += 1;
                        if job.done_gated >= Self::MAX_DONE_GATED {
                            let text = format!("I gave up on {}: {why}.", display_name(&job));
                            return self.finish(job, State::Failed, text);
                        }
                        job.note_to_model = Some(format!("your last move was rejected: {why}. Do that with an act now, then say done again with a check."));
                        self.store.save_job(&job)?;
                        None
                    } else {
                        let key_step = job.plan.len().max(1);
                        if self.perform(&mut job, key_step, check, true)? { return Ok(()); }
                        if job.steps.last().map(|s| s.ok).unwrap_or(false) {
                            return self.finish(job, State::Done, summary);
                        }
                        job.note_to_model = Some("your check failed — read its output above, fix the work, then say done again with a check".to_string());
                        self.store.save_job(&job)?;
                        None
                    }
                }
                (State::Working, Move::GiveUp { reason, missing }) => {
                    return self.finish(job.clone(), State::Failed, format!("I gave up on {}: {reason}. Missing: {missing}.", display_name(&job)));
                }
                (_, Move::Act { .. }) | (_, Move::Done { .. }) | (_, Move::Replan { .. }) | (_, Move::GiveUp { .. }) => Some("give a plan first".to_string()),
                (_, Move::Plan { .. }) => Some("not now".to_string()),
                (_, Move::Reply { .. }) | (_, Move::Start { .. }) | (_, Move::Housekeep { .. }) | (_, Move::Learn { .. }) => Some("a job is running: use ask, plan, act, replan, done or give_up".to_string()),
            };
            if let Some(why) = rejected {
                if self.reject(&mut job, &why)? { return Ok(()); }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moves::Move;
    use crate::testing::engine_with;

    fn start(project: &str, creative: bool) -> Move {
        Move::Start { project: project.into(), new_project: true, description: "prime printer".into(), goal: "print ten primes".into(), creative, understood: format!("Starting a new project {project}"), skills: vec![], remember: None, folder: None }
    }

    #[test]
    fn chat_is_just_a_reply_and_no_job() {
        let (mut e, rec, _) = engine_with(vec![Move::Reply { text: "A prime is…".into(), remember: None }], "chat");
        let out = e.handle("what's a prime?").unwrap();
        assert_eq!(out, vec!["A prime is…".to_string()]);
        assert!(e.open_job().unwrap().is_none());
        assert!(rec.calls.borrow().is_empty());
    }

    /// The owner's Blender chat: "do it", "proceed" — and each time a reply that said "I will…"
    /// and ran nothing. A reply that promises work is asked again, with only the working moves.
    #[test]
    fn a_reply_that_promises_work_is_asked_again_as_work() {
        let (mut e, _, _) = engine_with(vec![
            Move::Reply { text: "Understood. I will now proceed with the following steps: 1) Register Blender.".into(), remember: None },
            Move::Housekeep { goal: "note how to open Blender".into(), understood: "Noting Blender".into(), remember: None },
        ], "promise-reply");
        let _ = e.handle("proceed");
        let prompts = e.model.prompts.borrow();
        assert_eq!(prompts[1].allowed, vec!["start", "housekeep"]);
        assert!(!e.store.recent_messages(4).unwrap().iter().any(|(_, t)| t.contains("I will now proceed")), "the promise was said");
        assert!(e.open_job().unwrap().is_some(), "no job started");
    }

    #[test]
    fn remember_saves_a_standing_instruction_and_it_reaches_the_next_prompt() {
        let (mut e, _, _) = engine_with(vec![
            Move::Reply { text: "Noted.".into(), remember: Some("always use python3".into()) },
            Move::Reply { text: "ok".into(), remember: None },
        ], "remember");
        e.handle("from now on always use python3").unwrap();
        e.handle("hi").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[1].user.contains("always use python3"));
    }

    #[test]
    fn start_creates_project_folder_and_job_and_says_what_it_understood() {
        // The trailing Ask is unused by this task's stub loop; once Task 8 lands, the real
        // loop consumes it and pauses the job as waiting_answer — the assertions hold both ways.
        let (mut e, _, root) = engine_with(vec![start("Primes Printer", false), Move::Ask { questions: vec!["Which language?".into()], options: vec![] }], "start");
        let out = e.handle("make me a primes script").unwrap();
        assert_eq!(out[0], "Starting a new project Primes Printer");
        let job = e.open_job().unwrap().expect("a job is open");
        assert_eq!(job.project, "primes-printer");
        assert!(job.is_open());
        assert!(root.join("primes-printer").is_dir());
        assert_eq!(e.store.list_projects().unwrap()[0].name, "primes-printer");
    }

    #[test]
    fn names_and_stop_words() {
        assert_eq!(sanitize_project_name("Primes Printer!"), "primes-printer");
        assert_eq!(sanitize_project_name("///"), "project");
        assert!(is_stop("stop")); assert!(is_stop("leave it")); assert!(!is_stop("don't stop"));
        assert!(is_stop("Stop!")); assert!(is_stop("stop.")); assert!(!is_stop("stop asking"));
    }

    #[test]
    fn stop_cancels_an_open_job_through_handle() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "cancel");
        e.handle("make p").unwrap();
        let out = e.handle("Stop.").unwrap();
        assert!(out.iter().any(|l| l.contains("Stopped")), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        // The trailing Ask is now consumed by the real loop (job pauses waiting_answer),
        // so "Stop." cancels on a second handle() call — one model call per handle().
        assert_eq!(e.model.prompts.borrow().len(), 2);
    }

    #[test]
    fn stop_with_nothing_running_is_answered_by_the_engine_and_never_by_the_model() {
        // The front door is a `start`-capable grammar: handing it "stop" is how a 9B starts a
        // job called stop. The word is fixed in every state, so no model is asked.
        let (mut e, _, _) = engine_with(vec![], "idle-stop");
        let ev = e.handle_events("stop").unwrap();
        assert_eq!(ev, vec![Event::Said { text: "Nothing is running now.".into() }], "{ev:?}");
        assert!(e.model.prompts.borrow().is_empty(), "stop reached the model");
    }

    #[test]
    fn stop_while_a_job_waits_stops_it_once_and_leaves_no_flag_armed() {
        // Both doors at once: the service arms the flag AND queues the word. The job is waiting
        // for an answer, so the flag has nothing to land between — the word ends the job, and it
        // must clear the flag on its way out or the next job would start with a stop pending.
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["which language?".into()], options: vec![] }], "waiting-stop");
        e.handle("make p").unwrap();
        assert!(matches!(e.open_job().unwrap().unwrap().state, State::WaitingAnswer));
        let flag = e.stop_flag();
        flag.store(true, Ordering::SeqCst);
        let ev = e.handle_events("stop").unwrap();
        assert_eq!(ev.iter().filter(|x| matches!(x, Event::Stopped { .. })).count(), 1, "{ev:?}");
        assert!(!flag.load(Ordering::SeqCst), "the stop flag outlived the stop it belongs to");
        assert!(e.open_job().unwrap().is_none());
    }

    use executor::action::Action;
    use executor::worker::Outcome;

    fn write(path: &str) -> Action { Action::WriteFile { path: path.into(), contents: "x".into() } }
    fn run(cmd: &str) -> Action { Action::RunCommand { argv: vec![cmd.into()] } }
    fn plan() -> Move { Move::Plan { steps: vec!["write it".into(), "run it".into()] } }
    fn act(step: usize, a: Action) -> Move { Move::Act { step, action: a } }
    fn done(check: Action) -> Move { Move::Done { summary: "finished".into(), check } }

    /// A creative job that writes the blueprint, writes code, updates the blueprint, and proves it.
    fn happy_path() -> Vec<Move> {
        vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(1, write("primes.py")), act(1, write("BLUEPRINT.md")), done(run("python3"))]
    }

    /// The owner, 2026-09-20: "the local LLM keeps working and i do not know what it does."
    /// Every wait — the model's turn, a long command, the learning turn — says what it is.
    #[test]
    fn the_status_line_says_which_step_and_what_is_happening() {
        let (e, _, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            act(2, run("sudo apt-get install -y blender")), done(run("true")),
        ], "doing");
        let seen = std::rc::Rc::new(std::cell::RefCell::new(vec![]));
        let keep = seen.clone();
        let mut e = e.with_sink(Box::new(move |ev| keep.borrow_mut().push(ev.clone())));
        e.handle("install blender").unwrap();
        let lines: Vec<String> = seen.borrow().iter().filter_map(|e| match e { Event::Busy { text, .. } => Some(text.clone()), _ => None }).collect();
        assert!(lines.iter().any(|l| l == "Working out what you are asking for…"), "{lines:?}");
        assert!(lines.iter().any(|l| l == "Step 2 of 2: running sudo apt-get install -y blender"), "{lines:?}");
        assert!(lines.iter().any(|l| l.starts_with("Step 1 of 2: write it — working out the next move")), "{lines:?}");
        assert!(lines.iter().any(|l| l.contains("what to remember")), "{lines:?}");
    }

    /// A status line is not a message: the front door's window of the last four must still be
    /// the person's own words.
    #[test]
    fn the_status_line_is_never_kept_as_a_message() {
        let (mut e, _, _) = engine_with(happy_path(), "doing-history");
        let ev = e.handle_events("make it, decide yourself").unwrap();
        assert!(!ev.iter().any(|e| matches!(e, Event::Busy { .. })), "{ev:?}");
        let msgs = e.store.recent_messages(4).unwrap();
        assert!(!msgs.iter().any(|(_, t)| t.contains("working out")), "{msgs:?}");
    }

    /// Asking for leave to act is refused; asking the user what they want is the job.
    #[test]
    fn only_asking_for_permission_is_refused() {
        for q in ["Okay to delete the folder?", "May I install it?", "Should I proceed with the build?"] {
            assert!(asks_permission(q), "{q}");
        }
        for q in ["Which language should I use?", "Would you like me to add a dark theme?",
                  "How would you like to handle the missing logo?", "How many players?"] {
            assert!(!asks_permission(q), "{q}");
        }
    }

    #[test]
    fn full_job_runs_through_the_executor_and_ends_done() {
        let (mut e, rec, _) = engine_with(happy_path(), "happy");
        let out = e.handle("make it, decide yourself").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        assert_eq!(rec.calls.borrow().len(), 4, "3 acts + the check all went through the executor");
    }

    #[test]
    fn creative_start_goes_straight_to_planning() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "creative");
        e.handle("just make it, decide yourself").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[1].user.contains("Legal moves now: plan"), "no asking stage in creative mode: {}", prompts[1].user);
    }

    #[test]
    fn existing_project_is_reused_not_recreated() {
        let again = Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "add menu".into(), creative: true, understood: "Continuing p".into(), skills: vec![], remember: None, folder: None };
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
            again, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "reuse");
        e.handle("make p").unwrap();
        assert!(e.open_job().unwrap().is_none());
        let folder = e.store.get_project("p").unwrap().unwrap().folder;
        // Moving the root for *new* projects must not move a project that already has a folder.
        let other = crate::testing::temp_root("reuse-other");
        e.store.set_setting("projects_root", &other.display().to_string()).unwrap();
        e.handle("add a menu to p").unwrap();
        assert_eq!(e.store.list_projects().unwrap().len(), 1, "same project row, not a second one");
        assert_eq!(e.store.list_projects().unwrap()[0].description, "prime printer", "the original description is kept");
        assert_eq!(e.store.get_project("p").unwrap().unwrap().folder, folder, "the stored folder is read back, never recomputed from the root");
    }

    #[test]
    fn new_projects_land_under_the_projects_root_setting() {
        let (mut e, _, root) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "projects-root");
        let other = crate::testing::temp_root("projects-root-other");
        e.store.set_setting("projects_root", &other.display().to_string()).unwrap();
        e.handle("make p").unwrap();
        let row = e.store.get_project("p").unwrap().unwrap();
        assert_eq!(row.folder, other.join("p").display().to_string());
        assert!(other.join("p").is_dir());
        assert!(!root.join("p").exists(), "the constructor's root is only the default, not the answer");
    }

    #[test]
    fn stop_cancels_the_open_job_without_asking_the_model() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "stop");
        e.handle("make p").unwrap();
        assert_eq!(e.open_job().unwrap().unwrap().state, State::WaitingAnswer);
        let out = e.handle("stop").unwrap();
        assert!(out[0].contains("Stopped"));
        assert!(e.open_job().unwrap().is_none());
        assert_eq!(e.model.prompts.borrow().len(), 2, "cancel is deterministic, no model call");
    }

    #[test]
    fn asks_then_answer_resumes_and_answer_is_in_the_prompt() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false),
            Move::Ask { questions: vec!["Which language?".into()], options: vec![] },
            plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "ask");
        let out = e.handle("make it").unwrap();
        assert!(out.iter().any(|l| l.contains("Which language?")), "{out:?}");
        assert_eq!(e.open_job().unwrap().unwrap().state, State::WaitingAnswer);
        let out = e.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("Which language? -> python"));
    }

    #[test]
    fn ask_in_creative_mode_is_rejected_then_model_complies() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true), Move::Ask { questions: vec!["?".into()], options: vec![] }, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "creative-ask");
        let out = e.handle("decide yourself").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: this job is in creative mode"), "rejection note reaches the model: {}", prompts[2].user);
    }

    #[test]
    fn act_before_plan_and_done_without_blueprint_update_are_rejected() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true),
            act(1, write("a.py")),                 // rejected: no plan yet
            plan(),
            act(1, write("BLUEPRINT.md")),
            act(1, write("a.py")),
            done(run("true")),                     // rejected: blueprint older than the code change
            act(1, write("BLUEPRINT.md")),
            done(run("true")),
        ], "order");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: give a plan first"), "act-before-plan note: {}", prompts[2].user);
        assert!(prompts[6].user.contains("rejected: update BLUEPRINT.md"), "blueprint note: {}", prompts[6].user);
    }

    #[test]
    fn failing_check_sends_the_model_back_to_work() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("python3")), done(run("python3")),
        ], "check");
        rec.outcomes.borrow_mut().extend([
            Outcome::ok("ok"),                    // blueprint write
            Outcome::err("Traceback… NameError"), // first check fails
            Outcome::ok("2 3 5 7"),               // second check passes
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.contains("NameError"), "the check's failure reason is fed back: {}", prompts[4].user);
    }

    #[test]
    fn identical_retry_of_a_failed_action_is_refused_with_the_reason() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            act(1, run("gcc")), act(1, run("gcc")),   // second one is identical → refused, not run
            act(1, run("cc")), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "retry");
        rec.outcomes.borrow_mut().extend([
            Outcome::ok("ok"),
            Outcome::err("gcc: not found"),
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let calls = rec.calls.borrow();
        assert_eq!(calls.iter().filter(|a| **a == run("gcc")).count(), 1, "the identical retry never reached the executor");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[5].user.contains("gcc: not found"), "{}", prompts[5].user);
    }

    #[test]
    fn same_command_is_allowed_again_after_a_file_was_fixed() {
        // Spike finding: run fails → edit the file → the same run is the right move, not a repeat.
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            act(2, run("python3")),
            act(2, Action::EditFile { path: "primes.py".into(), find: "prnt".into(), replace: "print".into() }),
            act(2, run("python3")),
            act(2, write("BLUEPRINT.md")), done(run("python3")),
        ], "retry-after-fix");
        rec.outcomes.borrow_mut().extend([
            Outcome::ok("ok"),
            Outcome::err("NameError: prnt"),
            Outcome::ok("edited"),
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert_eq!(rec.calls.borrow().iter().filter(|a| **a == run("python3")).count(), 3, "failed run, re-run after the fix, and the check");
    }

    #[test]
    fn three_different_failures_on_one_step_give_up() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, run("a")), act(1, run("b")), act(1, run("c")), act(1, run("d")),
        ], "three");
        for _ in 0..4 { rec.outcomes.borrow_mut().push_back(Outcome::err("boom")); }
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), 3);
        assert!(e.open_job().unwrap().is_none());
    }

    #[test]
    fn two_rejected_moves_in_a_row_fail_the_job() {
        let (mut e, _, _) = engine_with(vec![start("p", true), act(1, run("x")), act(1, run("x"))], "reject");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
    }

    /// The owner's LM Studio run (2026-09-19): a thinking Qwen answered `{"understood":…,"act":{…}}`
    /// with no `move`, the turn ended on "something went wrong" and the job sat open until he typed.
    /// An answer that is not a move is a rejected move: said back to the model, tried again.
    #[test]
    fn an_unreadable_answer_is_sent_back_to_the_model_and_the_job_goes_on() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false), Move::Ask { questions: vec!["Which language?".into()], options: vec![] },
            plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "unreadable-once");
        e.handle("make p").unwrap();
        e.model.unreadable.set(1);
        let out = e.handle("Python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        let retry = &prompts[3].user;
        assert!(retry.contains("not a move") && retry.contains("\"move\""), "the retry says what was wrong: {retry}");
    }

    #[test]
    fn two_unreadable_answers_in_a_row_end_the_job_and_say_why() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "unreadable-twice");
        e.handle("make p").unwrap();
        e.model.unreadable.set(2);
        let out = e.handle("yes").unwrap();
        assert!(out.last().unwrap().contains("gave up"), "{out:?}");
        assert!(e.open_job().unwrap().is_none(), "never left half-open");
    }

    #[test]
    fn the_front_door_asks_again_once_when_the_answer_is_not_a_move() {
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "Hello.".into(), remember: None }], "unreadable-door");
        e.model.unreadable.set(1);
        assert_eq!(e.handle("hi").unwrap(), vec!["Hello.".to_string()]);
        assert!(e.model.prompts.borrow()[1].user.contains("not a move"));
        e.model.unreadable.set(2);
        let out = e.handle("hi again").unwrap();
        assert!(out[0].contains("could not read"), "a plain line, not an error: {out:?}");
    }

    #[test]
    fn step_cap_fails_the_job() {
        let mut moves = vec![start("p", true), plan()];
        for i in 0..Engine::<crate::model::FakeModel>::MAX_STEPS + 5 { moves.push(act(1, write(&format!("f{i}")))); }
        let (mut e, rec, _) = engine_with(moves, "cap");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), Engine::<crate::model::FakeModel>::MAX_STEPS);
    }

    #[test]
    fn replan_replaces_the_plan() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), Move::Replan { steps: vec!["other way".into()], why: "first way failed".into() },
            act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "replan");
        e.handle("go").unwrap();
        let prompts = e.model.prompts.borrow();
        assert!(prompts[3].user.contains("1. other way"));
        assert!(!prompts[3].user.contains("write it"));
    }

    #[test]
    fn give_up_reports_reason_and_missing() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), Move::GiveUp { reason: "no compiler".into(), missing: "gcc".into() }], "giveup");
        let out = e.handle("go").unwrap();
        let last = out.last().unwrap();
        assert!(last.contains("no compiler") && last.contains("gcc"), "{last}");
    }

    #[test]
    fn a_done_held_back_by_the_blueprint_gate_has_room_for_more_than_one_retry() {
        // The live 1d run lost two jobs here: the model said done twice before writing the
        // blueprint and the grammar budget of two ended the job. A gated done is a legal move
        // refused by policy, so it has its own, larger bound.
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("main.py")),
            done(run("true")), done(run("true")),
            act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "gate-retry");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        // And the counter is not a lifetime one: a gate hit, a step that really ran, another gate
        // hit, and so on does not accumulate — otherwise a long job that answers the gate
        // correctly every time still fails on the fourth code change.
        let (mut e, _, _) = engine_with(vec![
            start("r", true), plan(), act(1, write("main.py")),
            done(run("true")), done(run("true")), act(1, write("BLUEPRINT.md")),
            act(2, write("other.py")),
            done(run("true")), done(run("true")), act(2, write("BLUEPRINT.md")),
            act(2, write("third.py")),
            done(run("true")), done(run("true")), act(2, write("BLUEPRINT.md")),
            done(run("true")),
        ], "gate-resets");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "the gate counter accumulated across progress: {out:?}");
        // And it still ends on a model that never writes the blueprint at all.
        let (mut e, _, _) = engine_with(vec![
            start("q", true), plan(), act(1, write("main.py")),
            done(run("true")), done(run("true")), done(run("true")), done(run("true")), done(run("true")),
        ], "gate-bound");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("gave up") && out.last().unwrap().contains("BLUEPRINT.md"), "{out:?}");
    }

    #[test]
    fn the_same_action_twice_in_a_row_after_it_worked_is_rejected_not_run_again() {
        // The live 1d run's waste: the 9B re-issued a write that had just succeeded, nine times
        // over, and the job ran out of steps before it was finished.
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(1, write("BLUEPRINT.md")),
            act(2, run("true")), done(run("true")),
        ], "repeat");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert_eq!(rec.calls.borrow().iter().filter(|a| **a == write("BLUEPRINT.md")).count(), 1, "{:?}", rec.calls.borrow());
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.contains("just succeeded"), "the model is told why: {}", prompts[4].user);
    }

    #[test]
    fn writing_the_same_file_again_does_no_step() {
        // The owner's run, 2026-09-23: "list the folder" and then "delete the folder" were each a
        // write_file of the folder's own path, with new contents, and every one ticked its step.
        let file = |c: &str| Action::WriteFile { path: "projects".into(), contents: c.into() };
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), act(1, file("a")), act(2, file("b")), act(2, run("true")), done(run("true")),
        ], "rewrite");
        let ev = events_of(&mut e, "clear all project work");
        assert!(ev.iter().any(|v| matches!(v, Event::Step { plan_step: 2, ok: false, .. })), "the second write is a failed step: {ev:?}");
        assert_eq!(rec.calls.borrow().iter().filter(|a| matches!(a, Action::WriteFile { .. })).count(), 1, "{:?}", rec.calls.borrow());
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.contains("you just wrote this same file"), "the model is told why: {}", prompts[4].user);
        assert!(prompts[4].user.contains("run_command rm -rf"), "and what does the job: {}", prompts[4].user);
    }

    #[test]
    fn an_act_for_a_step_the_plan_does_not_have_is_rejected() {
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), act(3, run("true")), act(2, run("true")), done(run("true")),
        ], "step-range");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), 2, "only the act in range and the check ran: {:?}", rec.calls.borrow());
        assert!(e.model.prompts.borrow()[3].user.contains("plan step 3 is not in the plan (1-2)"));
    }

    #[test]
    fn a_job_is_told_where_the_projects_are() {
        // The owner's run, 2026-09-23: never told, the 9B guessed /home/user/projects.
        let (mut e, _, root) = engine_with(vec![
            start("kept", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
            housekeep(), plan(), act(1, run("true")), done(run("true")),
        ], "places");
        e.handle("make kept").unwrap();
        e.handle("clear all project work").unwrap();
        let prompts = e.model.prompts.borrow();
        let turn = &prompts.iter().rev().find(|p| p.user.contains("Housekeeping on the machine")).unwrap().user;
        assert!(turn.contains(&format!("projects live in {}", root.display())), "{turn}");
        assert!(turn.contains(&format!("kept ({})", root.join("kept").display())), "{turn}");
    }

    #[test]
    fn a_project_the_user_says_is_in_a_folder_is_registered_there() {
        // The owner, 2026-09-23: "if I tell it I have this project at that folder, it should
        // register it as a working project too". The folder may already hold the user's files.
        let root = crate::testing::temp_root("named-folder-where");
        let theirs = root.join("work").join("site");
        std::fs::create_dir_all(&theirs).unwrap();
        std::fs::write(theirs.join("index.html"), "hi").unwrap();
        let mv = Move::Start { project: "site".into(), new_project: true, description: "their site".into(), goal: "g".into(),
            creative: true, understood: "Working on site".into(), skills: vec![], remember: None, folder: Some(theirs.display().to_string()) };
        let (mut e, _, _) = engine_with(vec![mv, plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "named-folder");
        let out = e.handle("my site is in the work folder, fix its title").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert_eq!(e.store.get_project("site").unwrap().unwrap().folder, theirs.display().to_string());
        assert!(theirs.join("BLUEPRINT.md").exists() && theirs.join("index.html").exists());
    }

    #[test]
    fn a_command_that_worked_lets_a_failed_one_run_again() {
        // A normal user's retry: python fails for want of a module, pip installs it, python again.
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), act(1, run("app")), act(1, run("pip")), act(1, run("app")), done(run("true")),
        ], "retry-after-fix");
        rec.outcomes.borrow_mut().extend([Outcome::err("ModuleNotFoundError"), Outcome::ok("installed"), Outcome::ok("running"), Outcome::ok("ok")]);
        let out = e.handle("run my app").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert_eq!(rec.calls.borrow().iter().filter(|a| **a == run("app")).count(), 2, "{:?}", rec.calls.borrow());
    }

    #[test]
    fn a_new_plan_counts_its_own_failures() {
        // Old step 2 failed twice; the new plan's step 2 is another step, and one failure there
        // is not the third.
        let (mut e, _, _) = engine_with(vec![
            housekeep(), plan(), act(2, run("x1")), act(2, run("x2")),
            Move::Replan { steps: vec!["another way".into(), "check it".into()], why: "that did not work".into() },
            act(2, run("x3")), act(2, run("x4")), done(run("true")),
        ], "plan-from");
        let ev = events_of(&mut e, "do it");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
    }

    #[test]
    fn places_names_home_root_and_each_project() {
        let row = |n: &str| ProjectRow { name: n.into(), folder: format!("/data/projects/{n}"), description: String::new(), touched_at: 0 };
        let s = places("/home/u", Path::new("/data/projects"), Path::new("/data/housekeeping"), &[row("a"), row("b")]);
        assert!(s.contains("home is /home/u; projects live in /data/projects"), "{s}");
        assert!(s.contains("projects so far: a (/data/projects/a), b (/data/projects/b)"), "{s}");
        assert!(s.ends_with("scratch folder for housekeeping is /data/housekeeping."), "{s}");
        assert!(!places("/home/u", Path::new("/p"), Path::new("/s"), &[]).contains("so far"));
    }

    /// 2a §5: a repeated digit or a second Next is normal desktop work — 1d's "that exact action
    /// just succeeded" refusal exempts `press` and `type`. *A second* is what §5 claims and what
    /// this exempts: a third identical press in a row is caught, because the live 2a run pressed
    /// Main Menu eight times without looking once, toggling the same popover open and shut.
    #[test]
    fn the_same_press_twice_in_a_row_runs_twice_and_a_third_time_is_refused() {
        // Housekeeping jobs: no blueprint gate to satisfy, so the scripted moves are just the
        // two repeats and the check.
        let hk = |goal: &str| Move::Housekeep { goal: goal.into(), understood: "Pressing twice".into(), remember: None };
        let press = Action::Press { control: 3, name: "1".into() };
        let (mut e, rec, _) = engine_with(vec![hk("press twice"), plan(), act(1, press.clone()), act(1, press.clone()), done(run("python3"))], "press-twice");
        e.handle("press twice").unwrap();
        assert_eq!(rec.desktop_calls.borrow().len(), 2, "both presses reached the desktop hand");
        let run_twice = Action::RunCommand { argv: vec!["ls".into()] };
        let (mut e2, rec2, _) = engine_with(vec![hk("run twice"), plan(), act(1, run_twice.clone()), act(1, run_twice.clone()), done(run("python3"))], "run-twice");
        e2.handle("press twice").unwrap();
        assert_eq!(rec2.calls.borrow().iter().filter(|a| **a == run_twice).count(), 1, "a repeated command is still refused");
        let (mut e3, rec3, _) = engine_with(vec![
            hk("press thrice"), plan(), act(1, press.clone()), act(1, press.clone()), act(1, press.clone()),
            act(1, Action::Look { window: Some("Calculator".into()), find: None }), act(1, press.clone()),
            done(run("python3")),
        ], "press-thrice");
        e3.handle("press thrice").unwrap();
        assert_eq!(rec3.desktop_calls.borrow().iter().filter(|a| **a == press).count(), 3, "the third was refused, the one after the look ran");
        let prompts = e3.model.prompts.borrow();
        assert!(prompts.iter().any(|p| p.user.contains("look at the window to see what it did")), "the refusal says to look: {}", prompts.last().unwrap().user);
    }

    #[test]
    fn failed_job_leaves_a_last_run_note_and_the_next_job_reads_it_first() {
        let (mut e, rec, root) = engine_with(vec![
            start("p", true), plan(), act(1, run("python3")),
            Move::GiveUp { reason: "the script crashes".into(), missing: "a working loop".into() },
            // next job in the same project
            Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "make it work".into(), creative: true, understood: "Continuing p".into(), skills: vec![], remember: None, folder: None },
            plan(), act(1, write("BLUEPRINT.md")), done(run("python3")),
        ], "lastrun");
        rec.outcomes.borrow_mut().push_back(Outcome::err("exit 1; stderr: NameError: prmes"));
        e.handle("make p").unwrap();
        let note = std::fs::read_to_string(root.join("p").join("LAST_RUN.md")).expect("LAST_RUN.md written by the loop");
        assert!(note.contains("NameError: prmes") && note.contains("the script crashes") && note.contains("a working loop"), "{note}");
        let out = e.handle("make it work").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        // index 6, not 5: the failed job's own learning turn is prompt 4, one more than before.
        assert!(prompts[6].user.contains("LAST RUN") && prompts[6].user.contains("NameError: prmes"), "{}", prompts[6].user);
        assert!(!root.join("p").join("LAST_RUN.md").exists(), "wiped once a job in the project is proven done");
    }

    #[test]
    fn a_waiting_job_resumes_from_the_store_after_a_restart() {
        let (mut e, _, root) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Language?".into()], options: vec![] }], "restart");
        e.handle("make it").unwrap();
        let store = std::mem::replace(&mut e.store, crate::store::Store::open_in_memory().unwrap());
        drop(e);
        // New engine, same store: the answer must land on the saved job.
        let rec = crate::testing::Recorder::default();
        let housekeeping = root.join("housekeeping");
        let mut e2 = Engine::new(store, crate::model::FakeModel::new(vec![plan(), act(1, write("BLUEPRINT.md")), done(run("true"))]), root, None,
            crate::testing::scripted_workers(&rec), housekeeping);
        let out = e2.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(e2.open_job().unwrap().is_none());
    }

    #[test]
    fn replans_spread_over_a_job_that_makes_progress_never_add_up_to_giving_up() {
        let mut moves = vec![Move::Housekeep { goal: "set up".into(), understood: "Setting up".into(), remember: None }, plan()];
        for i in 0..4 { moves.push(Move::Replan { steps: vec![format!("first try {i}")], why: "x".into() }); }
        moves.push(act(1, run("worked")));
        for i in 0..4 { moves.push(Move::Replan { steps: vec![format!("second try {i}")], why: "x".into() }); }
        moves.push(done(run("true")));
        let (mut e, _, _) = engine_with(moves, "replan-progress");
        let out = e.handle("set it up").unwrap();
        assert!(out.last().unwrap().contains("finished"), "8 replans, but never 6 in a row: {out:?}");
    }

    #[test]
    fn a_model_that_only_ever_replans_eventually_gives_up() {
        let mut moves = vec![start("p", true), plan()];
        for i in 0..6 { moves.push(Move::Replan { steps: vec![format!("attempt {i}")], why: format!("attempt {i} failed") }); }
        let (mut e, _, _) = engine_with(moves, "replan-cap");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        // front door + plan + 6 replans; no learning turn: a failed job with no tips has nothing to mark
        assert!(e.model.prompts.borrow().len() <= 8, "{}", e.model.prompts.borrow().len());
    }

    #[test]
    fn empty_ask_is_rejected_then_a_real_question_goes_through() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false), Move::Ask { questions: vec![], options: vec![] }, Move::Ask { questions: vec!["Which language?".into()], options: vec![] },
        ], "empty-ask");
        let out = e.handle("go").unwrap();
        assert!(out.iter().any(|l| l.contains("Which language?")), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: ask needs at least one question"), "{}", prompts[2].user);
    }

    #[test]
    fn a_failed_checks_reason_blocks_an_identical_act_until_something_changes() {
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            done(run("python3")),                       // check fails
            act(2, run("python3")),                      // identical to the failed check -> refused
            act(2, Action::EditFile { path: "primes.py".into(), find: "prnt".into(), replace: "print".into() }),
            act(2, write("BLUEPRINT.md")),                // keep the blueprint gate satisfied
            done(run("python3")),                        // now passes
        ], "check-then-act");
        rec.outcomes.borrow_mut().extend([
            Outcome::ok("ok"),               // blueprint write
            Outcome::err("NameError: prnt"), // check fails
            Outcome::ok("edited"),           // edit succeeds
            Outcome::ok("2 3 5 7"),          // check passes
        ]);
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[5].user.contains("rejected: that exact action already failed with: NameError: prnt"), "{}", prompts[5].user);
    }

    /// The live 2a run: the model typed before looking (refused — "control 1 was never handed
    /// out"), looked, typed again and got it, then proposed that first type once more and was
    /// told its own action had already failed. A look hands out the ids the refusal was about,
    /// so the world it failed in is gone.
    #[test]
    fn a_look_clears_the_failures_that_happened_before_the_ids_existed() {
        let typing = Action::Type { control: 1, text: "reviewed".into(), replace: false };
        let (mut e, rec, _) = engine_with(vec![
            Move::Housekeep { goal: "take the editor".into(), understood: "Taking it".into(), remember: None }, plan(),
            act(1, typing.clone()),                                                  // fails: nothing looked at yet
            act(1, Action::Look { window: Some("Text Editor".into()), find: None }),  // the ids exist now
            act(1, typing.clone()),                                                  // the same action, and allowed
            done(Action::Read { control: 1, from_line: None, lines: None }),
        ], "look-clears-failures");
        rec.desktop_outcomes.borrow_mut().extend([
            Outcome::err("control 1 was never handed out; look at a window to get the ids of its controls"),
            Outcome::ok("controls of Text Editor:\n1 [text] \"first line\""),
            Outcome::ok("typed 8 characters into control 1"),
            Outcome::ok("(lines 1-2 of 2)\nfirst line\nreviewed"),
        ]);
        let ev = e.handle_events("take my editor").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert_eq!(rec.desktop_calls.borrow().iter().filter(|a| **a == typing).count(), 2, "the second type reached the hand");
        assert!(e.model.prompts.borrow().iter().all(|p| !p.user.contains("already failed")), "nothing was held against it");
    }

    /// The live 2a run: a refused press, and the 9B replanned with the action it wanted to take
    /// as its one plan step, over and over, until the replan budget killed the job. It is told
    /// what the move for that is, and the plan it already had is left standing.
    #[test]
    fn a_replan_that_is_really_an_action_is_told_so_and_the_job_goes_on() {
        let look = Action::Look { window: Some("gnome-calculator".into()), find: None };
        let (mut e, _, _) = engine_with(vec![
            Move::Housekeep { goal: "12 times 34".into(), understood: "Calculating".into(), remember: None }, plan(),
            Move::Replan { steps: vec![serde_json::to_string(&look).unwrap()], why: "the press failed".into() },
            act(1, look.clone()),
            done(Action::Read { control: 1, from_line: None, lines: None }),
        ], "replan-is-an-action");
        let ev = e.handle_events("what is 12 times 34").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts.iter().any(|p| p.user.contains("that is an action, not a plan step; send it with act")), "{}", prompts.last().unwrap().user);
        // Never a rejection: that budget is two and fatal, and the live run died of it the first
        // time this was refused rather than noted. The plan the job had is untouched. "rejected:"
        // (with the colon `reject()` always writes), not "rejected" bare — the learning turn's own
        // fixed vocabulary ("what the user liked or rejected") now sits in every passed job's
        // prompts and would otherwise false-positive this check.
        assert!(!prompts.iter().any(|p| p.user.contains("rejected:")), "it was rejected, not noted");
        assert_eq!(ev.iter().filter(|x| matches!(x, Event::Plan { .. })).count(), 1, "the plan never changed: {ev:?}");
    }

    /// The same budget, spent the other way: replanning to the plan already in hand.
    #[test]
    fn a_replan_to_the_plan_it_already_has_is_told_to_act() {
        let Move::Plan { steps } = plan() else { unreachable!() };
        let (mut e, _, _) = engine_with(vec![
            Move::Housekeep { goal: "tidy".into(), understood: "Tidying".into(), remember: None }, plan(),
            Move::Replan { steps: steps.clone(), why: "starting over".into() },
            act(1, Action::Look { window: None, find: None }),
            done(Action::Look { window: None, find: None }),
        ], "replan-same-plan");
        let ev = e.handle_events("tidy up").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts.iter().any(|p| p.user.contains("that is the plan you already have")), "no such note");
        // "rejected:" (the colon `reject()` always writes) — see the sibling test above for why
        // a bare "rejected" now also matches the learning turn's own fixed vocabulary.
        assert!(!prompts.iter().any(|p| p.user.contains("rejected:")), "it was rejected, not noted");
    }

    /// Three live 2a runs died here. The model read a control before looking at a window, was
    /// refused, and sent the same read again; the "already failed" rejection is fatal at two, so
    /// the job was over one failed step in. A window moves on between steps — the same read works
    /// the moment a look happens — so a desktop action may be tried again, as a failed step.
    #[test]
    fn a_desktop_action_that_failed_may_be_tried_again_and_is_never_rejected_for_it() {
        let read = Action::Read { control: 1, from_line: None, lines: None };
        let (mut e, rec, _) = engine_with(vec![
            Move::Housekeep { goal: "take the editor".into(), understood: "Taking it".into(), remember: None }, plan(),
            act(1, read.clone()),                                                    // no look yet
            act(1, read.clone()),                                                    // again, with nothing changed
            act(1, Action::Look { window: Some("Text Editor".into()), find: None }),
            act(1, read.clone()),
            done(read.clone()),
        ], "desktop-retry");
        rec.desktop_outcomes.borrow_mut().extend([
            Outcome::err("control 1 was never handed out; look at a window to get the ids of its controls"),
            Outcome::err("control 1 was never handed out; look at a window to get the ids of its controls"),
            Outcome::ok("controls of Text Editor:\n1 [text] \"first line\""),
            Outcome::ok("(lines 1-1 of 1)\nfirst line"),
            Outcome::ok("(lines 1-1 of 1)\nfirst line"),
        ]);
        let ev = e.handle_events("take my editor").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert_eq!(rec.desktop_calls.borrow().iter().filter(|a| **a == read).count(), 4, "every read reached the hand");
        assert!(e.model.prompts.borrow().iter().all(|p| !p.user.contains("already failed")), "a desktop retry was rejected");
    }

    /// The exemption is the five window actions and nothing else: a command that failed with
    /// nothing changed is still the 1d lesson it was.
    #[test]
    fn only_the_window_actions_escape_the_already_failed_rejection() {
        for a in [Action::Look { window: None, find: None }, Action::Press { control: 1, name: "Save".into() },
                  Action::Type { control: 1, text: "x".into(), replace: false },
                  Action::Read { control: 1, from_line: None, lines: None },
                  Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }] {
            assert!(on_the_desktop(&a), "{a:?}");
        }
        for a in [run("ls"), write("a.py"), Action::EditFile { path: "a.py".into(), find: "a".into(), replace: "b".into() },
                  run("mkdir -p /data/x")] {
            assert!(!on_the_desktop(&a), "{a:?}");
        }
    }

    /// The narrowing, through the front door. A window opening says nothing about a command
    /// that failed, so 1d's lesson survives a mixed job: the command that failed is still
    /// refused when it comes back unchanged, though a `look` succeeded in between.
    #[test]
    fn a_look_does_not_excuse_a_command_that_failed() {
        let bad = run("cat /nope");
        let look = Action::Look { window: Some("Text Editor".into()), find: None };
        let (mut e, rec, _) = engine_with(vec![
            Move::Housekeep { goal: "read the note".into(), understood: "Reading it".into(), remember: None }, plan(),
            act(1, bad.clone()),                 // fails
            act(1, look.clone()),                // a window opens; the command's world is where it was
            act(1, bad.clone()),                 // the same command, unchanged -> still refused
            done(look.clone()),
        ], "look-does-not-clear-a-command");
        rec.outcomes.borrow_mut().push_back(Outcome::err("cat: /nope: No such file or directory"));
        rec.desktop_outcomes.borrow_mut().extend([
            Outcome::ok("controls of Text Editor:\n1 [text] \"first line\""),
            Outcome::ok("controls of Text Editor:\n1 [text] \"first line\""),
        ]);
        let ev = e.handle_events("read my note").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert_eq!(rec.calls.borrow().iter().filter(|a| **a == bad).count(), 1, "the repeat reached the machine hand");
        assert!(e.model.prompts.borrow().iter().any(|p| p.user.contains("rejected: that exact action already failed with: cat: /nope: No such file or directory")),
                "the command was let through: {}", e.model.prompts.borrow().last().unwrap().user);
    }

    /// The other half of the same rule, narrowed by the review. A `read` proves nothing changed,
    /// so it clears nothing. A window action changed only the window, so it clears only the
    /// window actions' failures — a `run_command` that failed is untouched by a
    /// `look`, and keeps 1d's memory in a job that does both. A blueprint write still clears all.
    #[test]
    fn a_window_action_clears_only_the_window_actions_failures() {
        let key = |a: &Action| serde_json::to_string(a).unwrap();
        let cleared = |s: &Action, f: &Action| Engine::<crate::model::FakeModel>::cleared_by(s, &key(f));
        let desktop = [Action::Look { window: None, find: None }, Action::Press { control: 1, name: "Save".into() },
                       Action::Type { control: 1, text: "x".into(), replace: false },
                       Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }];
        let elsewhere = [run("ls"), run("mkdir -p /data/x")];
        for s in &desktop {
            for f in &desktop { assert!(cleared(s, f), "{s:?} should clear {f:?}"); }
            for f in &elsewhere { assert!(!cleared(s, f), "{s:?} must not clear {f:?}"); }
            // Its own half includes the `read` it does not itself clear anything for.
            assert!(cleared(s, &Action::Read { control: 1, from_line: None, lines: None }), "{s:?}");
        }
        // A write or edit of the blueprint changed the disk every action reads: all of them.
        for s in [write("BLUEPRINT.md"), Action::EditFile { path: "a.py".into(), find: "a".into(), replace: "b".into() }] {
            for f in desktop.iter().chain(elsewhere.iter()) { assert!(cleared(&s, f), "{s:?} should clear {f:?}"); }
        }
        // A `read` changes nothing anyone can see: it clears nothing.
        let read = Action::Read { control: 1, from_line: None, lines: None };
        for f in desktop.iter().chain(elsewhere.iter()) { assert!(!cleared(&read, f), "{f:?}"); }
        // A command that worked changed the machine: the commands may run again, the windows' own
        // failures stay theirs.
        for f in &elsewhere { assert!(cleared(&run("ls"), f), "{f:?}"); }
        for f in &desktop { assert!(!cleared(&run("ls"), f), "{f:?}"); }
    }

    #[test]
    fn empty_plan_is_rejected() {
        let (mut e, _, _) = engine_with(vec![
            start("p", true), Move::Plan { steps: vec![] }, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "empty-plan");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[2].user.contains("rejected: plan needs at least one step"), "{}", prompts[2].user);
    }

    #[test]
    fn blueprint_gate_matches_only_the_exact_root_path() {
        // I5: `docs/BLUEPRINT.md` must not satisfy the gate — `read_blueprint` only ever reads
        // the root file, so unblocking `done` on a nested file means the model never actually
        // consulted what it thinks it did.
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(),
            act(1, write("a.py")),                    // a code change
            act(1, write("docs/BLUEPRINT.md")),        // looks like a blueprint update but isn't
            done(run("true")),                          // rejected: gate still not satisfied
            act(1, write("BLUEPRINT.md")),              // the real root file
            done(run("true")),                          // now passes
        ], "blueprint-depth");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts.iter().any(|p| p.user.contains("rejected: update BLUEPRINT.md")), "{prompts:?}");
    }

    #[test]
    fn remember_on_start_is_noted_to_the_user_and_persisted() {
        // I6: the `Reply` arm told the user "(Noted for the future: …)" on a remember; the
        // `Start` arm saved it silently. Same line must appear here too.
        let mv = Move::Start {
            project: "p".into(), new_project: true, description: "d".into(), goal: "g".into(),
            creative: true, understood: "Starting p".into(), skills: vec![], remember: Some("always use python3".into()), folder: None,
        };
        let (mut e, _, _) = engine_with(vec![mv, plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "start-remember");
        let out = e.handle("go, and always use python3").unwrap();
        assert!(out.iter().any(|l| l.contains("Noted for the future: always use python3")), "{out:?}");
        assert!(e.store.instructions().unwrap().contains(&"always use python3".to_string()));
    }

    /// I7: builds an `Engine` with a real (file-backed) action log so it survives across the
    /// several `executor_for` calls one job makes, and hands the test the path to read directly.
    fn engine_with_log(moves: Vec<Move>, tag: &str) -> (Engine<crate::model::FakeModel>, PathBuf) {
        let rec = crate::testing::Recorder::default();
        let root = crate::testing::temp_root(tag);
        let log_path = std::env::temp_dir().join(format!("ai-os-core-{tag}-log-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&log_path);
        let log_path_str = log_path.to_string_lossy().to_string();
        let housekeeping = root.join("housekeeping");
        std::fs::create_dir_all(&housekeeping).unwrap();
        let e = Engine::new(
            crate::store::Store::open_in_memory().unwrap(),
            crate::model::FakeModel::new(moves),
            root,
            Some(log_path_str),
            crate::testing::scripted_workers(&rec),
            housekeeping,
        );
        (e, log_path)
    }

    fn housekeep() -> Move {
        Move::Housekeep {
            goal: "prepare /data/work for all projects".into(),
            understood: "Housekeeping: I'll create the folder and make it the projects root".into(),
            remember: None,
        }
    }

    #[test]
    fn a_project_writing_its_blueprint_elsewhere_is_sent_back_to_its_folder() {
        let stray = write("/opt/p/BLUEPRINT.md");
        let root_move = Action::SetSetting { key: "projects_root".into(), value: "/opt".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, root_move), act(1, stray.clone()), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "stray");
        let out = e.handle("make p").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!rec.calls.borrow().contains(&stray), "the stray write never ran");
        assert!(e.store.get_setting("projects_root").unwrap().is_none(), "a project job does not move where projects live");
    }

    #[test]
    fn housekeeping_job_has_no_project_no_blueprint_gate_and_no_last_run() {
        let mkdir = run("mkdir -p /data/work");
        let (mut e, rec, root) = engine_with(vec![
            housekeep(), plan(), act(1, mkdir.clone()), act(1, write("notes.txt")), done(run("true")),
        ], "hk");
        let out = e.handle("prepare a folder for all my projects").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!root.join("LAST_RUN.md").exists() && !e.housekeeping_dir().join("LAST_RUN.md").exists());
        assert!(rec.calls.borrow().contains(&mkdir), "the folder is made by a plain command");
        assert!(e.store.list_projects().unwrap().is_empty(), "no project row");
    }

    #[test]
    fn a_housekeeping_job_that_gives_up_still_leaves_no_last_run() {
        // The Done half above is vacuous: `write_or_wipe_last_run` only ever *writes* for a job
        // that ended failed or cancelled, so only a housekeeping job that gives up can prove the
        // `if job.housekeeping { return; }` guard is what keeps the note out of the scratch folder.
        let (mut e, _, root) = engine_with(vec![
            housekeep(), plan(), Move::GiveUp { reason: "no disk left".into(), missing: "space on /data".into() },
        ], "hk-giveup");
        let out = e.handle("prepare a folder").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert!(out.last().unwrap().contains("I gave up on housekeeping:"), "a housekeeping job has no project name to print: {out:?}");
        assert!(!e.housekeeping_dir().join("LAST_RUN.md").exists(), "a housekeeping job has no project folder to leave a note in");
        assert!(!root.join("LAST_RUN.md").exists());
        assert!(!PathBuf::from("LAST_RUN.md").exists(), "and none in the process's own directory either");
    }

    #[test]
    fn set_setting_moves_new_projects_and_rejects_bad_values() {
        // `temp_root_path` only names the folder; `engine_with` is what creates it.
        let (mut e, _, root) = engine_with(vec![
            housekeep(), plan(), act(1, Action::SetSetting { key: "projects_root".into(), value: crate::testing::temp_root_path("setting").join("work").display().to_string() }), done(run("true")),
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "setting");
        e.handle("move projects").unwrap();
        e.handle("make p").unwrap();
        assert!(root.join("work/p/BLUEPRINT.md").exists(), "new project under the new root");

        let (mut e2, _, _) = engine_with(vec![
            housekeep(), plan(), act(1, Action::SetSetting { key: "colour".into(), value: "blue".into() }),
            Move::GiveUp { reason: "x".into(), missing: "y".into() },
        ], "badkey");
        e2.handle("set colour").unwrap();
        let prompts = e2.model.prompts.borrow();
        // prompt[i] elicits move[i]; the failed act is move[2], so its reason reaches move[3]'s
        // prompt (the brief's index was one turn early).
        assert!(prompts[3].user.contains("unknown setting") && prompts[3].user.contains("projects_root"), "{}", prompts[3].user);
    }

    #[test]
    fn projects_root_takes_any_absolute_folder_and_refuses_a_relative_one() {
        // Full access: the home folder is as good a root as /data.
        let (mut e, _, _) = engine_with(vec![
            housekeep(), plan(), act(1, Action::SetSetting { key: "projects_root".into(), value: "work".into() }),
            act(1, Action::SetSetting { key: "projects_root".into(), value: "/home/ai".into() }), done(run("true")),
        ], "root-home");
        e.handle("put projects in my home").unwrap();
        assert_eq!(e.store.get_setting("projects_root").unwrap().as_deref(), Some("/home/ai"));
        assert!(e.model.prompts.borrow()[3].user.contains("projects_root must be an absolute path"), "the relative one was refused");
    }

    #[test]
    fn done_on_a_new_project_without_a_blueprint_file_is_rejected() {
        let (mut e, _, root) = engine_with(vec![
            start("p", true), plan(), act(1, run("sed")), done(run("true")), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "absgate");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.contains("rejected: create BLUEPRINT.md"), "{}", prompts[4].user);
        assert!(root.join("p/BLUEPRINT.md").exists(), "the scripted worker really wrote it");
    }

    #[test]
    fn housekeep_out_of_turn_is_rejected_like_start() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false), Move::Ask { questions: vec!["Which language?".into()], options: vec![] },
            housekeep(),                                   // out of turn: a job is already running
            plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "hk-out-of-turn");
        e.handle("make it").unwrap();
        let out = e.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[3].user.contains("rejected: a job is running"), "{}", prompts[3].user);
    }

    #[test]
    fn a_job_saved_before_folders_were_recorded_is_failed_not_resumed() {
        // Pre-1c rows have no `folder`, so every action would run in the process's own
        // directory. Fail them on sight, without asking the model.
        let (mut e, rec, _) = engine_with(vec![], "no-folder");
        let mut job = Job::new("p", "", "goal", true, "Starting p");
        job.state = State::Working;
        e.store.save_job(&job).unwrap();
        let out = e.handle("carry on").unwrap();
        assert!(out[0].contains("predates the folder record"), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        assert!(e.model.prompts.borrow().is_empty(), "no model call");
        assert!(rec.calls.borrow().is_empty());
        assert!(!PathBuf::from("LAST_RUN.md").exists(), "a folderless job must not drop a note in the current directory");
    }

    #[test]
    fn a_project_never_adopts_a_folder_that_is_already_there() {
        // A project name alone must not claim a folder someone else made.
        // After `engine_with`: it wipes and recreates the temp root.
        let (mut e, rec, root) = engine_with(vec![start("bin", true), plan()], "adopt");
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::write(root.join("bin").join("keep.sh"), "mine").unwrap();
        let out = e.handle("make bin").unwrap();
        assert_eq!(out.len(), 1, "{out:?}");
        assert!(out[0].contains("a folder already exists at") && out[0].contains("choose another project name"), "{out:?}");
        assert!(e.open_job().unwrap().is_none(), "no job was started");
        assert_eq!(std::fs::read_to_string(root.join("bin").join("keep.sh")).unwrap(), "mine");
        assert!(rec.calls.borrow().is_empty());
    }

    /// Full access: an action that used to wait for a yes (a Delete press, a write to /etc, a
    /// root command) just runs, and the job finishes without a question.
    #[test]
    fn nothing_is_ever_held_for_a_yes() {
        let delete = Action::Press { control: 2, name: "Delete".into() };
        let etc = write("/etc/aios-test");
        let root = Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "cowsay".into()] };
        let (mut e, log_path) = engine_with_log(vec![
            housekeep(), plan(), act(1, delete), act(1, etc), act(1, root), done(run("true")),
        ], "never-ask");
        let ev = e.handle_events("clean up").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert!(!ev.iter().any(|x| matches!(x, Event::NeedsAnswer { .. })), "{ev:?}");
        let conn = rusqlite::Connection::open(&log_path).unwrap();
        let ran: i64 = conn.query_row("SELECT COUNT(*) FROM actions WHERE outcome LIKE 'ok:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(ran, 4, "the three and the check all ran");
        let _ = std::fs::remove_file(&log_path);
    }

    use aios_proto::Event;
    use crate::testing::events_of;

    #[test]
    fn every_prompt_starts_with_the_machine() {
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "Hello!".into(), remember: None }], "machine-block");
        e.handle("hi").unwrap();
        let user = e.model.prompts.borrow()[0].user.clone();
        assert!(user.starts_with("This machine: ") && user.contains("never install it again"), "{user}");
    }

    #[test]
    fn what_the_ai_installs_is_recorded() {
        let root = Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "cowsay".into()] };
        let (mut e, log_path) = engine_with_log(vec![housekeep(), plan(), act(1, root), done(run("true"))], "records-install");
        e.handle_events("install cowsay").unwrap();
        assert_eq!(crate::machine::installed(e.store.conn()).unwrap(), vec![("cowsay".to_string(), "apt".to_string())]);
        let _ = std::fs::remove_file(&log_path);
    }

    #[test]
    fn a_note_the_user_did_not_ask_for_is_not_kept() {
        // The owner's run, 2026-09-23: "Hi" was answered with the car-rental project, noted as a
        // standing instruction by a job that was about the skill book.
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "Hello!".into(), remember: Some("car-rental-broker: a website".into()) }], "no-note");
        let ev = events_of(&mut e, "Hi");
        assert_eq!(ev, vec![Event::Said { text: "Hello!".into() }]);
        assert!(e.store.instructions().unwrap().is_empty());
    }

    #[test]
    fn chat_emits_said() {
        let (mut e, _, _) = engine_with(vec![Move::Reply { text: "A prime is…".into(), remember: Some("use python3".into()) }], "ev-said");
        let ev = events_of(&mut e, "what's a prime? and always use python3");
        assert_eq!(ev, vec![Event::Said { text: "A prime is…".into() }, Event::Said { text: "(Noted for the future: use python3)".into() }]);
    }

    #[test]
    fn a_job_emits_understood_plan_steps_and_done_with_the_check() {
        let (mut e, _, _) = engine_with(happy_path(), "ev-job");
        let ev = events_of(&mut e, "make it, decide yourself");
        let job_id = ev[0].job_id().unwrap().to_string();
        assert!(e.store.load_job(&job_id).unwrap().is_some());
        assert!(matches!(&ev[0], Event::Understood { name, text, housekeeping: false, .. } if name == "p" && text == "Starting a new project p"));
        assert!(matches!(&ev[1], Event::Plan { steps, .. } if steps == &vec!["write it".to_string(), "run it".to_string()]));
        let steps: Vec<&Event> = ev.iter().filter(|e| matches!(e, Event::Step { .. })).collect();
        assert_eq!(steps.len(), 4, "3 acts + the check: {ev:?}");
        assert!(matches!(steps[0], Event::Step { plan_step: 1, text, ok: true, .. } if text == "wrote BLUEPRINT.md"));
        assert!(matches!(steps[3], Event::Step { text, .. } if text == "ran python3"));
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { text, check: Some(c), .. } if text == "finished" && c == "ran python3: ok"));
    }

    #[test]
    fn events_stream_to_the_sink_as_they_happen() {
        use std::cell::RefCell; use std::rc::Rc;
        let seen: Rc<RefCell<Vec<String>>> = Rc::default();
        let s2 = seen.clone();
        let (e, _, _) = engine_with(happy_path(), "ev-sink");
        let mut e = e.with_sink(Box::new(move |ev| s2.borrow_mut().push(serde_json::to_string(ev).unwrap())));
        let ev = e.handle_events("make it").unwrap();
        // The status line (`tick`) goes to the sink and nowhere else, so it is not among the
        // events returned: everything else must be, in the same order.
        let flow: Vec<String> = seen.borrow().iter().filter(|s| !s.starts_with("{\"kind\":\"busy\"")).cloned().collect();
        assert_eq!(flow.len(), ev.len());
        assert!(flow[0].starts_with("{\"kind\":\"understood\""));
    }

    #[test]
    fn questions_are_events() {
        let py = || vec![vec!["Python".to_string(), "Rust".to_string()]];
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Which language?".into()], options: py() }], "ev-ask");
        let ev = events_of(&mut e, "make it");
        assert!(matches!(ev.last().unwrap(), Event::NeedsAnswer { questions, options, .. } if questions == &vec!["Which language?".to_string()] && options == &py()));
        assert_eq!(e.state().unwrap().unwrap().waiting, aios_proto::Waiting::Answer { questions: vec!["Which language?".into()], options: py() }, "a reconnecting window gets the buttons back");
        assert_eq!(crate::event::lines(ev.last().unwrap()), vec!["Question: Which language? (Python / Rust)".to_string()]);
    }

    #[test]
    fn stop_and_give_up_are_events_and_lines_match_the_old_prose() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "ev-stop");
        e.handle("make p").unwrap();
        let ev = events_of(&mut e, "stop");
        assert!(matches!(&ev[0], Event::Stopped { text, .. } if text == "Stopped the job in p."));
        assert_eq!(crate::event::lines(&ev[0]), vec!["Stopped the job in p.".to_string()]);

        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), Move::GiveUp { reason: "no compiler".into(), missing: "gcc".into() }], "ev-giveup");
        let ev = events_of(&mut e, "make p");
        assert!(matches!(crate::testing::before_learned(&ev), Event::Failed { text, .. } if text == "I gave up on p: no compiler. Missing: gcc."));
    }

    #[test]
    fn describe_names_every_action_in_plain_words() {
        use crate::event::describe;
        assert_eq!(describe(&write("a.py")), "wrote a.py");
        assert_eq!(describe(&Action::EditFile { path: "a.py".into(), find: "x".into(), replace: "y".into() }), "edited a.py");
        assert_eq!(describe(&Action::ReadFile { path: "a.py".into(), from_line: None, lines: None }), "read a.py");
        assert_eq!(describe(&Action::RunCommand { argv: vec!["python3".into(), "a.py".into()] }), "ran python3 a.py");
        assert_eq!(describe(&Action::SetSetting { key: "projects_root".into(), value: "/data/work".into() }), "set projects_root = /data/work");
    }

    #[test]
    fn done_lists_the_files_the_job_changed() {
        // A new project refuses a folder that already exists, so the old files are planted
        // AFTER a first job created the folder, and the second job is the one measured.
        let again = Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "add more".into(), creative: true, understood: "Continuing p".into(), skills: vec![], remember: None, folder: None };
        let (mut e, _, root) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
            again, plan(), act(1, write("BLUEPRINT.md")), act(1, write("primes.py")), act(1, write("BLUEPRINT.md")), done(run("python3")),
        ], "files");
        e.handle("make p").unwrap();
        let old = root.join("p"); std::fs::create_dir_all(old.join("node_modules")).unwrap();
        std::fs::write(old.join("old.txt"), "x").unwrap();
        std::fs::write(old.join("node_modules").join("lib.js"), "x").unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(120);
        for p in [old.join("old.txt"), old.join("node_modules").join("lib.js"), old.join("BLUEPRINT.md")] {
            std::fs::File::open(&p).unwrap().set_modified(past).unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(1100)); // started_at is whole seconds
        let ev = events_of(&mut e, "add primes to p");
        let Event::Done { files, .. } = crate::testing::before_learned(&ev) else { panic!("{ev:?}") };
        let names: Vec<&str> = files.iter().map(|f| f.path.rsplit('/').next().unwrap()).collect();
        assert_eq!(names, vec!["BLUEPRINT.md", "primes.py"], "{files:?}");
        assert!(files.iter().all(|f| f.path.starts_with(old.to_str().unwrap())), "absolute paths");
        assert!(matches!(files[1].kind, aios_proto::FileKind::Text));
    }

    #[test]
    fn a_failed_job_does_not_list_the_engines_own_last_run_note() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), Move::GiveUp { reason: "r".into(), missing: "m".into() }], "files-lastrun");
        let ev = events_of(&mut e, "make p");
        let Event::Failed { files, .. } = crate::testing::before_learned(&ev) else { panic!("{ev:?}") };
        assert!(files.iter().all(|f| !f.path.ends_with("LAST_RUN.md")), "{files:?}");
    }

    /// 2a §7: the windows a job worked are the ones it looked into, each once, from the steps
    /// that succeeded — nothing is stored on the job for it. The `open_app` in the script is a
    /// desktop-entry id, not a window name: listing it too named one window twice.
    #[test]
    fn done_lists_the_windows_the_job_looked_into() {
        let look = |w: &str| Action::Look { window: Some(w.into()), find: None };
        let (mut e, _, _) = engine_with(vec![
            Move::Housekeep { goal: "take the editor".into(), understood: "Taking the editor".into(), remember: None }, plan(),
            act(1, Action::OpenApp { name: "org.gnome.Calculator".into(), visible: false }),
            act(1, look("Text Editor")), act(1, Action::Press { control: 1, name: "Save".into() }), act(1, look("Text Editor")),
            act(1, look("")),   // "list the windows", written the 9B's way: not a window worked

            done(Action::Read { control: 2, from_line: None, lines: None }),
        ], "windows-worked");
        let ev = e.handle_events("take my editor").unwrap();
        let Event::Done { windows, .. } = crate::testing::before_learned(&ev) else { panic!("{ev:?}") };
        assert_eq!(windows, &vec!["Text Editor".to_string()]);
    }

    #[test]
    fn a_question_that_only_asks_for_permission_is_sent_back() {
        for q in ["Is it okay to proceed with installing the necessary packages?", "May I install Django?",
                  "Shall I go ahead?", "Do you want me to proceed?", "OK to delete the old files?"] {
            assert!(asks_permission(q), "{q}");
        }
        for q in ["Which framework do you prefer, Django or Flask?", "Should the first version look to the customer side?",
                  "What should the site be called?", "Do you want a dark theme?"] {
            assert!(!asks_permission(q), "{q}");
        }
    }

    #[test]
    fn a_check_that_clicks_is_sent_back() {
        let click = Action::ScreenClick { cell: 1, spot: 1, name: "Firefox".into(), double: false };
        let (mut e, rec, _) = engine_with(vec![screen_job(), plan(), act(1, Action::ScreenLook { cell: Some(1) }),
            done(click.clone()), done(Action::ScreenLook { cell: None })], "check-clicks");
        e.handle_events("open firefox").unwrap();
        assert!(!rec.desktop_calls.borrow().contains(&click), "the click never ran as a check");
        assert!(e.model.prompts.borrow()[4].user.contains("a check proves the work is done"));
    }

    #[test]
    fn the_screen_may_be_looked_at_twice_while_something_loads() {
        let look = || act(1, Action::ScreenLook { cell: None });
        let (mut e, rec, _) = engine_with(vec![screen_job(), plan(), look(), look(), look(), done(Action::ScreenLook { cell: None })], "look-twice");
        e.handle_events("wait for it").unwrap();
        assert_eq!(rec.desktop_calls.borrow().len(), 3, "two looks and the check: the third look in a row was sent back");
        assert!(e.model.prompts.borrow()[5].user.contains("you have looked at the screen twice"));
    }

    /// The owner's run (2026-09-19): a repeated window look was refused with "look at the window",
    /// so every look after it was refused too and the job gave up. A second look waits for the
    /// window to fill; a third is sent back with words that do not ask for a fourth.
    #[test]
    fn a_window_may_be_looked_at_twice_and_the_third_is_not_told_to_look() {
        let look = || act(1, Action::Look { window: Some("Firefox".into()), find: None });
        let (mut e, rec, _) = engine_with(vec![screen_job(), plan(), look(), look(), look(), done(Action::ScreenLook { cell: None })], "window-look-twice");
        e.handle_events("wait for firefox").unwrap();
        assert_eq!(rec.desktop_calls.borrow().len(), 3, "two looks and the check: the third look in a row was sent back");
        let p = &e.model.prompts.borrow()[5].user;
        assert!(p.contains("you have looked at this window twice") && !p.contains("look at the window to see"), "{p}");
    }

    fn screen_job() -> Move { Move::Housekeep { goal: "use the screen".into(), understood: "Using the screen".into(), remember: None } }

    /// 2b: a look's picture is shown to the very next turn and to no other — one image is what
    /// fits the budget, and an old one would show the model a screen that is gone.
    #[test]
    fn a_screen_look_shows_its_picture_to_the_next_turn_only() {
        let (mut e, rec, _) = engine_with(vec![screen_job(), plan(),
            act(1, Action::ScreenLook { cell: None }), act(1, Action::ScreenLook { cell: Some(3) }),
            done(Action::ScreenLook { cell: None })], "screen-picture");
        rec.desktop_outcomes.borrow_mut().push_back(Outcome { image: Some(b"one".to_vec()), ..Outcome::ok("the screen") });
        e.handle_events("click the thing").unwrap();
        let prompts = e.model.prompts.borrow();
        let pictures: Vec<Option<&[u8]>> = prompts.iter().map(|p| p.image.as_deref()).collect();
        let first = pictures.iter().position(Option::is_some).expect("a picture reached the model");
        assert_eq!(pictures[first], Some(&b"one"[..]));
        assert_eq!(pictures.iter().filter(|p| p.is_some()).count(), 1, "{pictures:?}");
        assert!(matches!(prompts[first - 1].image, None), "not before the look ran");
    }

    #[test]
    fn a_project_job_with_no_desktop_steps_lists_no_windows() {
        let (mut e, _, _) = engine_with(happy_path(), "no-windows");
        let ev = e.handle_events("make p").unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { windows, .. } if windows.is_empty()), "{ev:?}");
    }

    #[test]
    fn changed_files_is_capped_and_sorted() {
        let root = crate::testing::temp_root("files-cap");
        for i in 0..25 { std::fs::write(root.join(format!("f{i:02}.txt")), "x").unwrap(); }
        let files = changed_files(&root, 0);
        assert_eq!(files.len(), 20);
        assert!(files[0].path.ends_with("f00.txt"));
        assert!(files[19].path.ends_with("f19.txt"));
    }

    #[test]
    fn a_raised_stop_flag_cancels_the_job_between_steps() {
        // The model would write two files; the flag is raised by the first step's worker call.
        let (e, rec, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(1, write("b.py")), done(run("true"))], "flag");
        let flag = e.stop_flag();
        let f2 = flag.clone();
        // ScriptedWorker records calls; raise the flag once the first call has landed by
        // scripting the outcome queue: the outcome itself cannot run code, so use a sink.
        let calls = rec.calls.clone();
        let mut e = e.with_sink(Box::new(move |ev| if matches!(ev, Event::Step { .. }) && calls.borrow().len() == 1 { f2.store(true, std::sync::atomic::Ordering::SeqCst) }));
        let ev = e.handle_events("make p").unwrap();
        assert!(matches!(ev.last().unwrap(), Event::Stopped { text, .. } if text == "Stopped the job in p."), "{ev:?}");
        assert_eq!(rec.calls.borrow().len(), 1, "the second write never ran");
        assert!(e.open_job().unwrap().is_none());
        assert!(!flag.load(std::sync::atomic::Ordering::SeqCst), "the flag is cleared once honoured");
    }

    #[test]
    fn resume_carries_on_a_job_left_mid_work() {
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "resume");
        let id = e.handle_events("make p").unwrap()[0].job_id().unwrap().to_string();
        // Rewind the saved job to Working with the plan but no steps, like a crash after planning.
        let mut job = e.store.load_job(&id).unwrap().unwrap();
        job.state = State::Working; job.steps.clear(); job.outcome_text.clear();
        e.store.save_job(&job).unwrap();
        e.model = crate::model::FakeModel::new(vec![act(1, write("BLUEPRINT.md")), done(run("true"))]);
        let ev = e.resume().unwrap();
        assert!(matches!(crate::testing::before_learned(&ev), Event::Done { .. }), "{ev:?}");
        assert!(e.open_job().unwrap().is_none());
        assert!(e.resume().unwrap().is_empty(), "nothing open, nothing to resume");
    }

    #[test]
    fn resume_fails_a_job_that_predates_the_folder_record() {
        let (mut e, _, _) = engine_with(vec![], "resume-no-folder");
        let mut job = Job::new("p", "/data/projects/p", "g", true, "u");
        job.state = State::Working;
        job.folder = String::new();
        e.store.save_job(&job).unwrap();
        let ev = e.resume().unwrap();
        assert_eq!(ev.len(), 2, "{ev:?}");
        assert!(matches!(&ev[0], Event::Failed { text, .. } if text == "This job predates the folder record; please start it again."), "{ev:?}");
        assert!(matches!(&ev[1], Event::Learned { lines, pending: false, .. } if lines.is_empty()), "the rail's spinner stops: {ev:?}");
        assert!(e.open_job().unwrap().is_none());
    }

    #[test]
    fn state_mirrors_the_open_job_and_what_it_waits_for() {
        use aios_proto::Waiting;
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Which language?".into()], options: vec![] }], "state-ans");
        assert!(e.state().unwrap().is_none());
        e.handle("make p").unwrap();
        let st = e.state().unwrap().unwrap();
        assert_eq!((st.name.as_str(), st.housekeeping, st.understood.as_str()), ("p", false, "Starting a new project p"));
        assert_eq!(st.waiting, Waiting::Answer { questions: vec!["Which language?".into()], options: vec![] });

        // A job left mid-work: run one to completion, then rewind the saved record to Working
        // with its steps (the same rewind the resume test uses).
        let (mut e, _, _) = engine_with(vec![start("p", true), plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "state-steps");
        let id = e.handle_events("make p").unwrap()[0].job_id().unwrap().to_string();
        let mut job = e.store.load_job(&id).unwrap().unwrap();
        job.state = State::Working; job.steps.truncate(1); job.outcome_text.clear();
        e.store.save_job(&job).unwrap();
        let st = e.state().unwrap().unwrap();
        assert_eq!(st.steps.len(), 1);
        assert_eq!((st.steps[0].plan_step, st.steps[0].text.as_str(), st.steps[0].ok), (1, "wrote BLUEPRINT.md", true));
        assert_eq!(st.waiting, Waiting::None);
    }

    fn learn_open_site() -> Move {
        Move::Learn { entries: vec![crate::moves::LearnEntry { notebook: "this computer".into(), topic: "open a website".into(), kind: "technique".into(), text: "open_app firefox with the address".into(), steps: vec![1], links: vec![] }], used: vec![], wrong: vec![], remove: vec![] }
    }
    fn hk(goal: &str) -> Move { Move::Housekeep { goal: goal.into(), understood: "Opening it".into(), remember: None } }
    fn open_ff() -> Action { Action::OpenApp { name: "firefox".into(), visible: true } }

    #[test]
    fn a_passed_job_learns_and_the_next_job_starts_with_the_tip() {
        let (mut e, _, _) = engine_with(vec![
            hk("open example.org"), plan(), act(1, open_ff()), done(Action::ScreenLook { cell: None }), learn_open_site(),
            hk("open the weather website"),
        ], "learn-next");
        let ev = crate::testing::events_of(&mut e, "open the browser and go to example.org");
        let done_at = ev.iter().position(|x| matches!(x, Event::Done { .. })).expect("done");
        let learned_at = ev.iter().position(|x| matches!(x, Event::Learned { .. })).expect("learned");
        assert!(done_at < learned_at, "the Done card never waits for the learning");
        let Event::Learned { lines, pending, .. } = &ev[learned_at] else { unreachable!() };
        assert_eq!((lines.clone(), *pending), (vec!["Learned: open a website (this computer)".to_string()], false));
        let _ = e.handle("open the weather website");
        let last = e.model.prompts.borrow().last().unwrap().user.clone();
        assert!(last.contains("- this computer/open a website: open_app firefox with the address"), "{last}");
    }

    #[test]
    fn a_learning_turn_the_model_cannot_answer_never_hurts_the_job() {
        let (mut e, _, _) = engine_with(vec![hk("open it"), plan(), act(1, open_ff()), done(Action::ScreenLook { cell: None })], "learn-none");
        let ev = crate::testing::events_of(&mut e, "open it");
        let done_at = ev.iter().position(|x| matches!(x, Event::Done { .. })).expect("done");
        // Exactly one Learned, empty: the spinner needs it even when there was nothing to learn.
        assert_eq!(ev.iter().filter(|x| matches!(x, Event::Learned { .. })).count(), 1);
        let learned_at = ev.iter().position(|x| matches!(x, Event::Learned { .. })).expect("learned");
        assert!(done_at < learned_at, "still after Done");
        let Event::Learned { lines, pending, .. } = &ev[learned_at] else { unreachable!() };
        assert!(lines.is_empty() && !pending);
        assert_eq!(e.model.prompts.borrow().last().unwrap().allowed, vec!["learn"], "the learning turn was asked");
    }

    #[test]
    fn a_failed_job_shown_no_tips_asks_the_model_nothing_after() {
        let (mut e, _, _) = engine_with(vec![hk("open it"), plan(), Move::GiveUp { reason: "no".into(), missing: "x".into() }], "learn-failed-bare");
        let ev = crate::testing::events_of(&mut e, "open it");
        assert!(matches!(ev.last(), Some(Event::Learned { lines, pending: false, .. }) if lines.is_empty()), "{ev:?}");
        assert!(e.model.prompts.borrow().iter().all(|p| p.allowed != vec!["learn"]), "nothing to mark, no learning turn");
    }

    #[test]
    fn a_stopped_job_has_no_learning_turn() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()], options: vec![] }], "learn-stop");
        e.handle("make p").unwrap();
        e.handle("stop").unwrap();
        assert!(e.model.prompts.borrow().iter().all(|p| p.allowed != vec!["learn"]));
    }

    #[test]
    fn keep_and_discard_are_fixed_words_not_model_calls() {
        let install = Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "gimp".into()] };
        let learn_install = Move::Learn { entries: vec![crate::moves::LearnEntry { notebook: "this computer".into(), topic: "get gimp".into(), kind: "technique".into(), text: "install gimp".into(), steps: vec![1], links: vec![] }], used: vec![], wrong: vec![], remove: vec![] };
        let (mut e, _, _) = engine_with(vec![hk("get gimp"), plan(), act(1, install), done(run("true")), learn_install], "learn-keep");
        let ev = crate::testing::events_of(&mut e, "install gimp");
        assert!(ev.iter().any(|x| matches!(x, Event::Learned { pending: true, .. })), "{ev:?}");
        assert!(crate::notes::list(e.store.conn(), "this computer").unwrap().is_empty());
        let before = e.model.prompts.borrow().len();
        assert!(e.handle("Keep what you learned.").unwrap()[0].contains("Kept"));
        assert_eq!(e.model.prompts.borrow().len(), before);
        assert_eq!(crate::notes::list(e.store.conn(), "this computer").unwrap().len(), 1);
        assert!(e.handle("discard what you learned").unwrap()[0].contains("nothing waiting"));
    }

    #[test]
    fn start_records_its_skills_and_the_skill_tips() {
        let ball = Move::Start { project: "ball".into(), new_project: true, description: "d".into(), goal: "a ball".into(), creative: false, understood: "Making a ball".into(), skills: vec!["Blender".into(), " blender ".into(), "this computer".into()], remember: None, folder: None };
        let (mut e, _, _) = engine_with(vec![ball, Move::Ask { questions: vec!["?".into()], options: vec![] }], "learn-skills");
        crate::notes::put(e.store.conn(), &crate::notes::Note { notebook: "blender".into(), topic: "make a round object".into(), kind: "technique".into(), text: "add a uv sphere".into(), ..Default::default() }, None).unwrap();
        e.handle("make a ball in blender").unwrap();
        let job = e.open_job().unwrap().unwrap();
        assert_eq!(job.skills, vec!["blender".to_string()], "normalised, once, and this computer is never a skill");
        assert!(job.notes_block.contains("blender/make a round object"), "{}", job.notes_block);
        assert_eq!(job.shown_notes, vec!["blender/make a round object".to_string()]);
    }
}
