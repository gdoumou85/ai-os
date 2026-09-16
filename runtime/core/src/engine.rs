use crate::job::{Job, State, StepRecord};
use crate::model::{Model, ModelError};
use crate::moves::Move;
use crate::prompt;
use crate::snapshot;
use crate::store::{Store, StoreError};
use executor::action::Action;
use executor::executor::{ExecOutcome, Executor};
use executor::log::{ActionLog, LogError};
use executor::undo::UndoEntry;
use executor::worker::{Outcome, Worker};
use std::path::{Path, PathBuf};

/// One project folder in, the two hands that serve it out: (sandbox, admin).
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
    /// Where a job's read-only project snapshot goes — the files undo puts back.
    snapshots_dir: PathBuf,
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

// ponytail: fixed word lists, not the model — an approval must never depend on a 9B reading tone.
fn normalize(text: &str) -> String {
    text.trim().to_lowercase().trim_end_matches(['.', '!', '?', ',']).trim().to_string()
}

fn words(t: &str) -> impl Iterator<Item = &str> {
    t.split(|c: char| c.is_whitespace() || c == ',').filter(|w| !w.is_empty())
}

pub fn is_yes(text: &str) -> bool {
    const YES_WORDS: [&str; 11] = ["yes", "y", "ok", "okay", "go", "do it", "approve", "approved", "send it", "go ahead", "sure"];
    const NEGATIONS: [&str; 8] = ["no", "not", "don't", "dont", "never", "away", "stop", "cancel"];
    let t = normalize(text);
    if YES_WORDS.contains(&t.as_str()) { return true; }
    let result = match words(&t).next() {
        Some(first) if YES_WORDS.contains(&first) => !words(&t).any(|w| NEGATIONS.contains(&w)),
        _ => false,
    };
    result
}
pub fn is_stop(text: &str) -> bool {
    const STOP_PHRASES: [&str; 10] = [
        "stop", "cancel", "leave it", "abort", "never mind", "forget it",
        "stop it", "stop now", "please stop", "stop please",
    ];
    STOP_PHRASES.contains(&normalize(text).as_str())
}

/// "Put the last job back." Exact phrases only, like `is_stop` — which is also what refuses a
/// negation ("don't undo") and a sentence that merely contains the word ("undo is a word"):
/// neither is on the list, and undoing a job by accident is not a mistake we can offer back.
pub fn is_undo(text: &str) -> bool {
    const UNDO_PHRASES: [&str; 13] = [
        "undo", "undo that", "undo it", "undo the last job", "undo last job",
        "roll back", "rollback", "put it back", "revert",
        "please undo", "undo please", "undo it please", "undo that please",
    ];
    UNDO_PHRASES.contains(&normalize(text).as_str())
}

/// What to call a job in a line the user reads. A housekeeping job has no project, so
/// `job.project` is the empty string — "Stopped the job in ." is not a sentence.
fn display_name(job: &Job) -> &str {
    if job.housekeeping { "housekeeping" } else { &job.project }
}

impl<M: Model> Engine<M> {
    pub fn new(store: Store, model: M, default_root: PathBuf, log_path: Option<String>, workers: WorkerFactory, housekeeping_dir: PathBuf, snapshots_dir: PathBuf) -> Self {
        Self { store, model, default_root, log_path, workers, housekeeping_dir, snapshots_dir }
    }

    pub fn housekeeping_dir(&self) -> &Path { &self.housekeeping_dir }

    /// I3: a store error here used to be swallowed (`.ok().flatten()`) — a job left unreadable
    /// (a corrupt row, a locked file) looked exactly like "no job open", so the engine would
    /// silently start a fresh one over it. Propagate instead.
    pub fn open_job(&self) -> Result<Option<Job>, EngineError> { Ok(self.store.open_job()?) }

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

    /// One executor per project folder: classification and enforcement share the same
    /// workspace path by construction (parent §11 item 3).
    pub(crate) fn executor_for(&self, job: &Job) -> Result<Executor<Box<dyn Worker>>, EngineError> {
        let ws = self.workspace(job);
        let log = match &self.log_path { Some(p) => ActionLog::open(p)?, None => ActionLog::open_in_memory()? };
        let (sandbox, admin) = (self.workers)(&ws);
        Ok(Executor::new(sandbox, admin, log, ws))
    }

    pub(crate) fn read_blueprint(&self, job: &Job) -> Option<String> {
        std::fs::read_to_string(self.workspace(job).join("BLUEPRINT.md")).ok()
    }

    /// The only entry point: one user message in, the lines to show the user out.
    pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        self.store.push_message("user", text)?;
        let out = self.handle_inner(text)?;
        for line in &out { self.store.push_message("assistant", line)?; }
        Ok(out)
    }

    fn handle_inner(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        let open = self.open_job()?;
        // Undo is deterministic, like stop and the approvals: no model call either way. It is
        // one job at a time here too — a running job's changes are not finished being made, so
        // there is nothing coherent to put back yet.
        if is_undo(text) {
            return match open {
                Some(_) => Ok(vec!["Finish or stop the current job first, then say undo.".into()]),
                None => self.undo_last(),
            };
        }
        if let Some(mut job) = open {
            // A job saved before 1c recorded folders has none, so every one of its actions would
            // run in the process's own directory. Fail it on sight — no model call, and not
            // through `finish`, which would drop a LAST_RUN.md in that same directory.
            if job.folder.is_empty() {
                job.state = State::Failed;
                job.outcome_text = "This job predates the folder record; please start it again.".into();
                self.store.save_job(&job)?;
                return Ok(vec![job.outcome_text]);
            }
            if is_stop(text) {
                let message = format!("Stopped the job in {}.", display_name(&job));
                return self.finish(job, State::Cancelled, message);
            }
            match job.state {
                State::WaitingAnswer => {
                    let q = job.pending_questions.join(" / ");
                    job.answers.push((q, text.to_string()));
                    job.pending_questions.clear();
                    job.state = if job.plan.is_empty() { State::Planning } else { State::Working };
                    job.rejections = 0;
                    self.store.save_job(&job)?;
                    return self.run_turns(job);
                }
                State::WaitingApproval => {
                    let approved = is_yes(text);
                    return self.resume_after_approval(job, approved, text);
                }
                _ => {
                    // A job left mid-work (crash/restart): carry on with it.
                    return self.run_turns(job);
                }
            }
        }
        // Idle: the model decides — chat, or work.
        let p = prompt::front_door(&self.store.instructions()?, &self.store.list_projects()?, &self.store.recent_messages(4)?, text);
        match self.model.next_move(&p)? {
            Move::Reply { text, remember } => {
                let mut out = vec![text];
                if let Some(r) = remember { self.store.add_instruction(&r)?; out.push(format!("(Noted for the future: {r})")); }
                Ok(out)
            }
            Move::Start { project, new_project: _, description, goal, creative, understood, remember } => {
                let name = sanitize_project_name(&project);
                let existing = self.store.get_project(&name)?;
                let is_new = existing.is_none();
                // An existing project keeps the folder it was created in. Recomputing it from the
                // root re-homed the project on every start, overwriting the stored folder (1b bug).
                let folder = match &existing {
                    Some(p) => PathBuf::from(&p.folder),
                    None => self.projects_root()?.join(&name),
                };
                // A new project folder is a btrfs subvolume where the filesystem allows one —
                // that is what makes this project's files undoable at all.
                if is_new { snapshot::create_project_dir(&folder)?; }
                let desc = existing.map(|p| p.description).unwrap_or(description);
                let folder = folder.display().to_string();
                self.store.upsert_project(&name, &folder, &desc)?;
                // I6: `remember` on a `start` move was saved silently — only the `Reply` arm told
                // the user. Same "(Noted for the future: …)" line here, computed before the value
                // moves into `add_instruction`.
                let note = remember.as_ref().map(|r| format!("(Noted for the future: {r})"));
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                let mut job = Job::new(&name, &folder, &goal, creative, &understood);
                job.new_project = is_new;
                self.store.save_job(&job)?;
                // The files as they were before this job touched them. A plain-folder project
                // (pre-1c, or any non-btrfs machine) gets no snapshot and no row — undo says so
                // rather than pretending the files were covered.
                if let Some(snap) = snapshot::take(Path::new(&folder), &self.snapshots_dir, &job.id)? {
                    self.store.add_undo(&job.id, &UndoEntry::ProjectSnapshot { folder: folder.clone(), snapshot: snap.display().to_string() })?;
                }
                let mut out = vec![understood];
                if let Some(n) = note { out.push(n); }
                out.extend(self.run_turns(job)?);
                Ok(out)
            }
            // The machine itself: no project row, no blueprint, one shared scratch folder.
            Move::Housekeep { goal, understood, remember } => {
                let note = remember.as_ref().map(|r| format!("(Noted for the future: {r})"));
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                std::fs::create_dir_all(&self.housekeeping_dir)?;
                let job = Job::new_housekeeping(&self.housekeeping_dir.display().to_string(), &goal, &understood);
                self.store.save_job(&job)?;
                let mut out = vec![understood];
                if let Some(n) = note { out.push(n); }
                out.extend(self.run_turns(job)?);
                Ok(out)
            }
            other => Ok(vec![format!("(I answered out of turn — {other:?} — please say that again.)")]),
        }
    }

    const MAX_STEPS: usize = 25;
    const MAX_FAILS_PER_STEP: usize = 3;
    const MAX_REJECTIONS: u32 = 2;
    const MAX_REPLANS: u32 = 5;

    /// Mark a job finished (done/failed/cancelled) and report the one line that explains it.
    fn finish(&self, mut job: Job, state: State, text: String) -> Result<Vec<String>, EngineError> {
        job.state = state;
        job.outcome_text = text.clone();
        self.store.save_job(&job)?;
        self.write_or_wipe_last_run(&job);
        Ok(vec![text])
    }

    /// "Undo that": put back everything the most recent finished job changed, newest change
    /// first, and say in plain words what was put back — and what could not be.
    ///
    /// A reversal that fails is reported and marked applied all the same: it must not be retried
    /// blindly the next time the user says undo, and every other row still runs.
    fn undo_last(&mut self) -> Result<Vec<String>, EngineError> {
        let job = match self.store.last_undoable_job()? {
            Some(j) => j,
            None => return Ok(vec!["Nothing left to undo.".into()]),
        };
        let rows = self.store.unapplied_undo(&job.id)?;
        // One executor for the whole reversal, like one job's worth of work: `executor_for`
        // opens the action log, which is not something to do once per row.
        let exec = self.executor_for(&job)?;
        let mut out = vec![format!("Undoing the last job ({}):", display_name(&job))];
        for (row_id, entry) in rows {
            // Settings and snapshots are the engine's own: no worker holds either. Everything
            // else was done by the privileged hand, so the privileged hand puts it back.
            let engine_lane = matches!(entry, UndoEntry::ProjectSnapshot { .. } | UndoEntry::Setting { .. });
            let result = match &entry {
                // `snap`, not `snapshot`: the module of that name is what does the work.
                UndoEntry::ProjectSnapshot { folder, snapshot: snap } => {
                    snapshot::restore(Path::new(folder), Path::new(snap)).map_err(|e| e.to_string())
                }
                UndoEntry::Setting { key, previous } => match previous {
                    Some(value) => self.store.set_setting(key, value).map(|_| ()).map_err(|e| e.to_string()),
                    None => self.store.delete_setting(key).map_err(|e| e.to_string()),
                },
                other => match exec.reverse(&job.id, other)? {
                    o if o.ok => Ok(()),
                    o => Err(o.detail),
                },
            };
            // `Executor::reverse` logs the admin lane itself; the engine's own two kinds would
            // otherwise leave no trace at all of having been put back.
            if engine_lane {
                let (tag, detail) = match &result { Ok(()) => ("ok", "put back".to_string()), Err(d) => ("error", d.clone()) };
                if let Err(e) = exec.log_text(&job.id, &format!("undo: {entry:?}: {tag}: {detail}")) {
                    eprintln!("core: failed to log an undo: {e}");
                }
            }
            out.push(match result {
                Ok(()) => entry.describe(),
                Err(detail) => format!("Could not undo: {} — left as is ({detail})", entry.describe()),
            });
            // The change is already put back, so a store that cannot record it is the end of the
            // run — but the user still gets every line earned so far rather than an error page.
            if let Err(e) = self.store.mark_undo_applied(row_id) {
                out.push(format!("Undo stopped early: {e}"));
                return Ok(out);
            }
        }
        out.push("Not covered: unsaved work in open programs; files written outside the project.".into());
        // A plain project folder (it predates 1c, or the filesystem is not btrfs) never had a
        // snapshot taken, so its files were not covered. Asked of the folder itself rather than
        // of the rows: an undo run that has already put the snapshot back has no row left to
        // read, and would otherwise start claiming the files were never covered.
        if !job.housekeeping && !snapshot::is_subvolume(Path::new(&job.folder)) {
            out.push(format!("Files in *{}* were not covered: the project predates undo.", job.project));
        }
        Ok(out)
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
    /// worker can apply it. One known key so far; an unknown one names what is known.
    fn apply_setting(&self, key: &str, value: &str) -> Result<Outcome, EngineError> {
        if key != "projects_root" {
            return Ok(Outcome::err(format!("unknown setting '{key}'; known settings: projects_root")));
        }
        // The constructor's own root counts as a root: a test (and a machine with a custom
        // AI_OS_PROJECTS) lives outside /data and /home/ai, and its own root is not an escape.
        let default_root = self.default_root.display().to_string();
        let mut roots: Vec<&str> = executor::rules::AI_ROOTS.to_vec();
        roots.push(&default_root);
        if !executor::rules::under_any(value, &roots) {
            return Ok(Outcome::err(format!("projects_root must be an absolute path under /data, /home/ai, or the projects root ({default_root})")));
        }
        let previous = self.store.set_setting(key, value)?;
        Ok(Outcome::ok(format!("setting {key} = {value}")).with_undo(UndoEntry::Setting { key: key.into(), previous }))
    }

    fn reject(&self, job: &mut Job, why: &str) -> Result<Option<Vec<String>>, EngineError> {
        job.rejections += 1;
        job.note_to_model = Some(format!("your last move was rejected: {why}"));
        if job.rejections >= Self::MAX_REJECTIONS {
            let text = format!("I gave up on {}: I kept answering in a way the system could not accept ({why}).", display_name(job));
            return Ok(Some(self.finish(job.clone(), State::Failed, text)?));
        }
        self.store.save_job(job)?;
        Ok(None)
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

    /// Run one action through the executor's door and record what happened on the job.
    /// Returns the lines to show the user if the job must stop here (waiting for OK / gave up).
    ///
    /// `is_check`: a `done` move's proof action is exempt from the identical-action dedup
    /// below — a check is meant to be re-run verbatim after a fix elsewhere (that's the whole
    /// point of a check), unlike an `act` step repeating the same failed command with nothing
    /// changed. Both still count toward MAX_FAILS_PER_STEP, which caps a stuck check the same
    /// way it caps a stuck act.
    fn perform(&self, job: &mut Job, plan_step: usize, action: Action, approved: bool, is_check: bool) -> Result<Option<Vec<String>>, EngineError> {
        let key = serde_json::to_string(&action).unwrap_or_default();
        // A human's "no" never expires — not cleared by a later file write, unlike failed_actions.
        if job.declined_actions.contains(&key) {
            return self.reject(job, "the user declined that action; do not repeat it");
        }
        if !is_check && !approved && job.failed_actions.contains(&key) {
            let earlier = job.steps.iter().rev().find(|s| serde_json::to_string(&s.action).unwrap_or_default() == key).map(|s| s.detail.clone()).unwrap_or_default();
            return self.reject(job, &format!("that exact action already failed with: {earlier} — work around it or replan"));
        }
        let exec = self.executor_for(job)?;
        // `SetSetting` never reaches a worker (executor::lane -> Lane::Engine): the engine applies
        // it and records it with `log_only`. A failure counts like any other failed step.
        if let Action::SetSetting { key: name, value } = &action {
            let outcome = self.apply_setting(name, value)?;
            // Same invariant as `Executor::execute`: the setting is already written, so a logging
            // failure must not abort the turn — that would lose both the step and the undo row,
            // leaving a changed setting nothing can put back.
            if let Err(e) = exec.log_only(&job.id, &action, &format!("{}: {}", if outcome.ok { "ok" } else { "error" }, outcome.detail)) {
                eprintln!("core: failed to log a setting change: {e}");
            }
            job.rejections = 0;
            job.note_to_model = None;
            job.steps.push(StepRecord { plan_step, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
            if outcome.ok {
                if let Some(entry) = outcome.undo { self.store.add_undo(&job.id, &entry)?; }
            } else {
                job.failed_actions.push(key);
                let fails = job.steps.iter().filter(|s| s.plan_step == plan_step && !s.ok).count();
                if fails >= Self::MAX_FAILS_PER_STEP {
                    let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", display_name(job), outcome.detail);
                    return Ok(Some(self.finish(job.clone(), State::Failed, text)?));
                }
            }
            self.store.save_job(job)?;
            return Ok(None);
        }
        match exec.execute(&job.id, &action, approved)? {
            ExecOutcome::Blocked(reason) => {
                job.rejections = 0; // a legal act needing approval is a valid move, not a rejection
                job.pending_action = Some((plan_step, action));
                job.pending_reason = reason.clone();
                job.state = State::WaitingApproval;
                self.store.save_job(job)?;
                Ok(Some(vec![format!("Needs your OK: {reason}. Say yes to allow it, or anything else to refuse.")]))
            }
            ExecOutcome::Ran(outcome) => {
                job.rejections = 0;
                job.note_to_model = None;
                job.steps.push(StepRecord { plan_step, action: action.clone(), ok: outcome.ok, detail: outcome.detail.clone() });
                // Not gated on `outcome.ok`: a worker records an undo entry only when it really
                // changed something, and a change made by an action that then failed is exactly
                // the one the user most needs put back.
                if let Some(u) = outcome.undo.clone() { self.store.add_undo(&job.id, &u)?; }
                if outcome.ok {
                    // A housekeeping job has no blueprint, so neither counter means anything to it.
                    if !job.housekeeping {
                        match Self::is_blueprint(&action) {
                            Some(true) => job.last_blueprint_update = job.steps.len(),
                            Some(false) => job.last_code_change = job.steps.len(),
                            None => {}
                        }
                    }
                    // Something changed on disk, so an earlier failure may now succeed:
                    // re-running the same command after a fix is legitimate (spike finding).
                    if Self::is_blueprint(&action).is_some() { job.failed_actions.clear(); }
                } else {
                    // Recorded even for a check (only the identical-action *lookup* above is
                    // check-exempt): a later `act` proposing this same action must still see why
                    // it already failed.
                    job.failed_actions.push(key);
                    let fails = job.steps.iter().filter(|s| s.plan_step == plan_step && !s.ok).count();
                    if fails >= Self::MAX_FAILS_PER_STEP {
                        let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", display_name(job), outcome.detail);
                        return Ok(Some(self.finish(job.clone(), State::Failed, text)?));
                    }
                }
                self.store.save_job(job)?;
                Ok(None)
            }
        }
    }

    fn resume_after_approval(&mut self, mut job: Job, approved: bool, text: &str) -> Result<Vec<String>, EngineError> {
        let (plan_step, action) = match job.pending_action.take() { Some(p) => p, None => { job.state = State::Working; return self.run_turns(job); } };
        job.state = State::Working;
        if job.steps.len() >= Self::MAX_STEPS {
            let text = format!("I gave up on {}: {} steps without finishing.", display_name(&job), Self::MAX_STEPS);
            return self.finish(job, State::Failed, text);
        }
        if approved {
            if let Some(stop) = self.perform(&mut job, plan_step, action, true, false)? { return Ok(stop); }
        } else {
            job.note_to_model = Some(format!("the user declined that action ({}): \"{text}\". Do not repeat it; find another way or finish without it.", job.pending_reason));
            job.declined_actions.push(serde_json::to_string(&action).unwrap_or_default());
            self.store.save_job(&job)?;
        }
        self.run_turns(job)
    }

    /// Turn after turn until the job is done, failed, or needs the user (1b spec §3–§4).
    fn run_turns(&mut self, mut job: Job) -> Result<Vec<String>, EngineError> {
        loop {
            if job.steps.len() >= Self::MAX_STEPS {
                let text = format!("I gave up on {}: {} steps without finishing.", display_name(&job), Self::MAX_STEPS);
                return self.finish(job, State::Failed, text);
            }
            let last_run = if job.housekeeping { None } else { std::fs::read_to_string(self.workspace(&job).join("LAST_RUN.md")).ok() };
            let p = prompt::job_turn(&self.store.instructions()?, &job, self.read_blueprint(&job).as_deref(), last_run.as_deref());
            let mv = self.model.next_move(&p)?;
            let rejected = match (job.state, mv) {
                (State::Asking, Move::Ask { questions }) | (State::Working, Move::Ask { questions }) | (State::Planning, Move::Ask { questions }) if !job.creative => {
                    if questions.is_empty() {
                        Some("ask needs at least one question".to_string())
                    } else {
                        job.pending_questions = questions.clone();
                        job.state = State::WaitingAnswer;
                        job.rejections = 0;
                        job.note_to_model = None;
                        self.store.save_job(&job)?;
                        return Ok(questions.iter().map(|q| format!("Question: {q}")).collect());
                    }
                }
                (_, Move::Ask { .. }) if job.creative => Some("this job is in creative mode: decide yourself instead of asking".to_string()),
                (_, Move::Ask { .. }) => Some("ask only before planning or while working".to_string()),
                (State::Asking, Move::Plan { steps }) | (State::Planning, Move::Plan { steps }) => {
                    if steps.is_empty() {
                        Some("plan needs at least one step".to_string())
                    } else {
                        job.plan = steps; job.state = State::Working; job.rejections = 0; job.note_to_model = None;
                        self.store.save_job(&job)?; None
                    }
                }
                (State::Working, Move::Plan { .. }) => Some("you already have a plan; use replan to change it".to_string()),
                (State::Working, Move::Replan { steps, why }) => {
                    job.replans += 1;
                    if job.replans > Self::MAX_REPLANS {
                        let text = format!("I gave up on {}: the plan kept changing ({} replans) without progress.", display_name(&job), job.replans);
                        return self.finish(job, State::Failed, text);
                    }
                    job.plan = steps; job.rejections = 0;
                    job.note_to_model = Some(format!("plan revised because: {why}"));
                    self.store.save_job(&job)?; None
                }
                (State::Working, Move::Act { step, action }) => {
                    if let Some(stop) = self.perform(&mut job, step, action, false, false)? { return Ok(stop); }
                    None
                }
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
                        Some(why)
                    } else {
                        let key_step = job.plan.len().max(1);
                        match self.perform(&mut job, key_step, check, false, true)? {
                            Some(stop) => return Ok(stop),
                            None => {
                                if job.steps.last().map(|s| s.ok).unwrap_or(false) {
                                    return self.finish(job, State::Done, summary);
                                }
                                job.note_to_model = Some("your check failed — read its output above, fix the work, then say done again with a check".to_string());
                                self.store.save_job(&job)?;
                                None
                            }
                        }
                    }
                }
                (State::Working, Move::GiveUp { reason, missing }) => {
                    return self.finish(job.clone(), State::Failed, format!("I gave up on {}: {reason}. Missing: {missing}.", display_name(&job)));
                }
                (_, Move::Act { .. }) | (_, Move::Done { .. }) | (_, Move::Replan { .. }) | (_, Move::GiveUp { .. }) => Some("give a plan first".to_string()),
                (_, Move::Plan { .. }) => Some("not now".to_string()),
                (_, Move::Reply { .. }) | (_, Move::Start { .. }) | (_, Move::Housekeep { .. }) => Some("a job is running: use ask, plan, act, replan, done or give_up".to_string()),
            };
            if let Some(why) = rejected {
                if let Some(stop) = self.reject(&mut job, &why)? { return Ok(stop); }
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
        Move::Start { project: project.into(), new_project: true, description: "prime printer".into(), goal: "print ten primes".into(), creative, understood: format!("Starting a new project {project}"), remember: None }
    }

    #[test]
    fn chat_is_just_a_reply_and_no_job() {
        let (mut e, rec, _) = engine_with(vec![Move::Reply { text: "A prime is…".into(), remember: None }], "chat");
        let out = e.handle("what's a prime?").unwrap();
        assert_eq!(out, vec!["A prime is…".to_string()]);
        assert!(e.open_job().unwrap().is_none());
        assert!(rec.calls.borrow().is_empty());
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
        let (mut e, _, root) = engine_with(vec![start("Primes Printer", false), Move::Ask { questions: vec!["Which language?".into()] }], "start");
        let out = e.handle("make me a primes script").unwrap();
        assert_eq!(out[0], "Starting a new project Primes Printer");
        let job = e.open_job().unwrap().expect("a job is open");
        assert_eq!(job.project, "primes-printer");
        assert!(job.is_open());
        assert!(root.join("primes-printer").is_dir());
        assert_eq!(e.store.list_projects().unwrap()[0].name, "primes-printer");
    }

    #[test]
    fn names_and_yes_no() {
        assert_eq!(sanitize_project_name("Primes Printer!"), "primes-printer");
        assert_eq!(sanitize_project_name("///"), "project");
        assert!(is_yes("yes, send it")); assert!(is_yes("OK")); assert!(!is_yes("no way"));
        assert!(is_yes("yes.")); assert!(!is_yes("okay so no")); assert!(!is_yes("go away")); assert!(!is_yes("not yet"));
        assert!(is_stop("stop")); assert!(is_stop("leave it")); assert!(!is_stop("don't stop"));
        assert!(is_stop("Stop!")); assert!(is_stop("stop.")); assert!(!is_stop("stop asking"));
    }

    #[test]
    fn undo_words() {
        assert!(is_undo("undo"));
        assert!(is_undo("Undo that."));
        assert!(is_undo("roll back"));
        assert!(is_undo("Please undo!")); assert!(is_undo("undo that please"));
        assert!(!is_undo("don't undo"));
        assert!(!is_undo("undo is a word"));
    }

    #[test]
    fn stop_cancels_an_open_job_through_handle() {
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()] }], "cancel");
        e.handle("make p").unwrap();
        let out = e.handle("Stop.").unwrap();
        assert!(out.iter().any(|l| l.contains("Stopped")), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        // The trailing Ask is now consumed by the real loop (job pauses waiting_answer),
        // so "Stop." cancels on a second handle() call — one model call per handle().
        assert_eq!(e.model.prompts.borrow().len(), 2);
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
        let again = Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "add menu".into(), creative: true, understood: "Continuing p".into(), remember: None };
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
        let (mut e, _, _) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["?".into()] }], "stop");
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
            Move::Ask { questions: vec!["Which language?".into()] },
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
            start("p", true), Move::Ask { questions: vec!["?".into()] }, plan(), act(1, write("BLUEPRINT.md")), done(run("true")),
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

    #[test]
    fn step_cap_fails_the_job() {
        let mut moves = vec![start("p", true), plan()];
        for i in 0..30 { moves.push(act(1, write(&format!("f{i}")))); }
        let (mut e, rec, _) = engine_with(moves, "cap");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert_eq!(rec.calls.borrow().len(), 25);
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
    fn risky_action_waits_for_ok_and_runs_after_yes() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()), done(run("true")),
        ], "approve");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("Needs your OK"), "{out:?}");
        assert_eq!(e.open_job().unwrap().unwrap().state, State::WaitingApproval);
        assert_eq!(rec.calls.borrow().len(), 1, "the risky action did not run");
        let out = e.handle("yes, send it").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(rec.calls.borrow().contains(&post));
    }

    #[test]
    fn declined_risky_action_is_told_to_the_model() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()), done(run("true")),
        ], "decline");
        e.handle("go").unwrap();
        let out = e.handle("no, don't").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!rec.calls.borrow().contains(&post));
        let prompts = e.model.prompts.borrow();
        assert!(prompts[4].user.to_lowercase().contains("declined"), "{}", prompts[4].user);
    }

    #[test]
    fn failed_job_leaves_a_last_run_note_and_the_next_job_reads_it_first() {
        let (mut e, rec, root) = engine_with(vec![
            start("p", true), plan(), act(1, run("python3")),
            Move::GiveUp { reason: "the script crashes".into(), missing: "a working loop".into() },
            // next job in the same project
            Move::Start { project: "p".into(), new_project: false, description: "x".into(), goal: "make it work".into(), creative: true, understood: "Continuing p".into(), remember: None },
            plan(), act(1, write("BLUEPRINT.md")), done(run("python3")),
        ], "lastrun");
        rec.outcomes.borrow_mut().push_back(Outcome::err("exit 1; stderr: NameError: prmes"));
        e.handle("make p").unwrap();
        let note = std::fs::read_to_string(root.join("p").join("LAST_RUN.md")).expect("LAST_RUN.md written by the loop");
        assert!(note.contains("NameError: prmes") && note.contains("the script crashes") && note.contains("a working loop"), "{note}");
        let out = e.handle("make it work").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        let prompts = e.model.prompts.borrow();
        assert!(prompts[5].user.contains("LAST RUN") && prompts[5].user.contains("NameError: prmes"), "{}", prompts[5].user);
        assert!(!root.join("p").join("LAST_RUN.md").exists(), "wiped once a job in the project is proven done");
    }

    #[test]
    fn a_waiting_job_resumes_from_the_store_after_a_restart() {
        let (mut e, _, root) = engine_with(vec![start("p", false), Move::Ask { questions: vec!["Language?".into()] }], "restart");
        e.handle("make it").unwrap();
        let store = std::mem::replace(&mut e.store, crate::store::Store::open_in_memory().unwrap());
        drop(e);
        // New engine, same store: the answer must land on the saved job.
        let rec = crate::testing::Recorder::default();
        let housekeeping = root.join("housekeeping");
        let snapshots = root.join("snapshots");
        let mut e2 = Engine::new(store, crate::model::FakeModel::new(vec![plan(), act(1, write("BLUEPRINT.md")), done(run("true"))]), root, None,
            crate::testing::scripted_workers(&rec), housekeeping, snapshots);
        let out = e2.handle("python").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(e2.open_job().unwrap().is_none());
    }

    #[test]
    fn a_model_that_only_ever_replans_eventually_gives_up() {
        let mut moves = vec![start("p", true), plan()];
        for i in 0..6 { moves.push(Move::Replan { steps: vec![format!("attempt {i}")], why: format!("attempt {i} failed") }); }
        let (mut e, _, _) = engine_with(moves, "replan-cap");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert!(e.open_job().unwrap().is_none());
        // front door + plan + 6 replans
        assert!(e.model.prompts.borrow().len() <= 8, "{}", e.model.prompts.borrow().len());
    }

    #[test]
    fn empty_ask_is_rejected_then_a_real_question_goes_through() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false), Move::Ask { questions: vec![] }, Move::Ask { questions: vec!["Which language?".into()] },
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
    fn declined_action_never_becomes_allowed_again_even_after_a_file_change() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()),
            act(2, write("other.py")),
            act(2, write("BLUEPRINT.md")),                // keep the blueprint gate satisfied
            act(2, post.clone()),
            done(run("true")),
        ], "declined-forever");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("Needs your OK"), "{out:?}");
        let out = e.handle("no, don't").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!rec.calls.borrow().contains(&post), "a declined action must never reach the executor, even after a file changed and it was proposed again");
    }

    #[test]
    fn blocked_action_resets_rejections() {
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, _, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            Move::Ask { questions: vec!["?".into()] },   // rejected: creative mode (rejections -> 1)
            act(2, post.clone()),                          // blocked -> should reset rejections to 0
        ], "blocked-resets");
        let out = e.handle("go").unwrap();
        assert!(out.last().unwrap().contains("Needs your OK"), "{out:?}");
        assert_eq!(e.open_job().unwrap().unwrap().rejections, 0, "a legal act needing approval resets the rejection count");
    }

    #[test]
    fn step_cap_is_rechecked_on_the_approval_path() {
        let (mut e, rec, root) = engine_with(vec![], "cap-approval");
        let mut job = Job::new("p", &root.display().to_string(), "goal", true, "Starting p");
        job.plan = vec!["step".into()];
        job.state = State::WaitingApproval;
        for i in 0..25 { job.steps.push(StepRecord { plan_step: 1, action: write(&format!("f{i}")), ok: true, detail: "ok".into() }); }
        job.pending_action = Some((1, Action::HttpPost { url: "https://x".into(), body: "b".into() }));
        job.pending_reason = "network access".into();
        e.store.save_job(&job).unwrap();
        let out = e.handle("yes").unwrap();
        assert!(out.last().unwrap().to_lowercase().contains("gave up"), "{out:?}");
        assert!(rec.calls.borrow().is_empty(), "the capped job must not reach the executor");
        assert!(e.open_job().unwrap().is_none());
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
            creative: true, understood: "Starting p".into(), remember: Some("always use python3".into()),
        };
        let (mut e, _, _) = engine_with(vec![mv, plan(), act(1, write("BLUEPRINT.md")), done(run("true"))], "start-remember");
        let out = e.handle("go").unwrap();
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
        let root_snapshots = root.join("snapshots");
        std::fs::create_dir_all(&root_snapshots).unwrap();
        let e = Engine::new(
            crate::store::Store::open_in_memory().unwrap(),
            crate::model::FakeModel::new(moves),
            root,
            Some(log_path_str),
            crate::testing::scripted_workers(&rec),
            housekeeping,
            root_snapshots,
        );
        (e, log_path)
    }

    #[test]
    fn approved_flag_is_pinned_by_the_action_log_not_just_the_reply() {
        // I7: nothing else in the test harness can see the `approved` bool `perform` passes to
        // `Executor::execute` — the action log can. This pins that the risky action ran exactly
        // once, and only after a "blocked" row: if `approved` were ever passed `true`
        // unconditionally, there would be no "blocked" row at all.
        let post = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        let (mut e, log_path) = engine_with_log(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")), act(2, post.clone()), done(run("true")),
        ], "approval-log");
        e.handle("go").unwrap();
        let out = e.handle("yes, send it").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");

        let conn = rusqlite::Connection::open(&log_path).unwrap();
        let post_json = serde_json::to_string(&post).unwrap();
        let mut stmt = conn.prepare("SELECT outcome FROM actions WHERE action_json = ?1 ORDER BY id").unwrap();
        let outcomes: Vec<String> = stmt.query_map([&post_json], |r| r.get(0)).unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(outcomes.len(), 2, "{outcomes:?}");
        assert!(outcomes[0].starts_with("blocked:"), "{outcomes:?}");
        assert!(outcomes[1].starts_with("ok:"), "{outcomes:?}");
        let _ = std::fs::remove_file(&log_path);
    }

    fn housekeep() -> Move {
        Move::Housekeep {
            goal: "prepare /data/work for all projects".into(),
            understood: "Housekeeping: I'll create the folder and make it the projects root".into(),
            remember: None,
        }
    }

    #[test]
    fn housekeeping_job_has_no_project_no_blueprint_gate_and_no_last_run() {
        // `/data/work` rather than a temp path: `rules::classify` sends a `make_dir` outside
        // AI_ROOTS to the approval gate, and the admin recorder never touches the disk anyway.
        let (mut e, rec, root) = engine_with(vec![
            housekeep(), plan(), act(1, Action::MakeDir { path: "/data/work".into() }), act(1, write("notes.txt")), done(run("true")),
        ], "hk");
        let out = e.handle("prepare a folder for all my projects").unwrap();
        assert!(out.last().unwrap().contains("finished"), "{out:?}");
        assert!(!root.join("LAST_RUN.md").exists() && !e.housekeeping_dir().join("LAST_RUN.md").exists());
        assert_eq!(rec.admin_calls.borrow().len(), 1, "make_dir went to the admin lane");
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
        // What Task 9 needs to put the root back: the value the setting held before.
        let hk = e.store.last_undoable_job().unwrap().expect("the housekeeping job left something to put back");
        let entries: Vec<UndoEntry> = e.store.unapplied_undo(&hk.id).unwrap().into_iter().map(|(_, en)| en).collect();
        assert_eq!(entries, vec![UndoEntry::Setting { key: "projects_root".into(), previous: None }]);

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
            start("p", false), Move::Ask { questions: vec!["Which language?".into()] },
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

    /// `/data/work` rather than a temp path, for the same reason the housekeeping test gives:
    /// `rules::classify` sends a `make_dir` outside AI_ROOTS to the approval gate, and the admin
    /// recorder never touches the disk anyway. It is also a legal `projects_root` value.
    const WORK: &str = "/data/work";
    fn make_work() -> Move { act(1, Action::MakeDir { path: WORK.into() }) }
    fn dir_undo() -> Outcome { Outcome::ok("made").with_undo(UndoEntry::DirCreated { path: WORK.into() }) }

    #[test]
    fn undo_reverses_the_last_jobs_rows_newest_first_and_reports() {
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), make_work(),
            act(1, Action::SetSetting { key: "projects_root".into(), value: WORK.into() }),
            done(run("true")),
        ], "undo");
        // The admin lane's outcome carries the undo for the folder it made.
        rec.admin_outcomes.borrow_mut().push_back(dir_undo());
        e.handle("prep").unwrap();
        let out = e.handle("undo").unwrap();
        assert_eq!(rec.reversed.borrow().len(), 1, "the dir reversal went to the admin worker");
        assert!(out.iter().any(|l| l.contains("projects_root")) && out.iter().any(|l| l.contains("folder")), "{out:?}");
        assert_eq!(e.store.get_setting("projects_root").unwrap(), None, "setting reversed by the engine");
        // Newest first: the setting went back before the folder it pointed at was removed.
        let setting_at = out.iter().position(|l| l.contains("projects_root")).unwrap();
        let folder_at = out.iter().position(|l| l.contains("Removed the folder")).unwrap();
        assert!(setting_at < folder_at, "{out:?}");
        assert_eq!(out[0], "Undoing the last job (housekeeping):", "{out:?}");
        assert!(out.iter().any(|l| l.contains("Not covered: unsaved work in open programs")), "{out:?}");
        assert!(!out.iter().any(|l| l.contains("predates undo")), "housekeeping has no project files: {out:?}");
        assert!(e.handle("undo").unwrap()[0].contains("Nothing left to undo"));
    }

    #[test]
    fn undo_while_a_job_is_open_is_refused() {
        let (mut e, _, _) = engine_with(vec![
            start("p", false), Move::Ask { questions: vec!["Which language?".into()] },
        ], "undo-open");
        e.handle("make p").unwrap();
        let before = e.open_job().unwrap().expect("a job is open");
        let out = e.handle("undo").unwrap();
        assert!(out[0].contains("Finish or stop the current job first"), "{out:?}");
        assert_eq!(e.open_job().unwrap().as_ref(), Some(&before), "the job is untouched");
        assert_eq!(e.model.prompts.borrow().len(), 2, "refusing undo asks the model nothing");
    }

    #[test]
    fn undo_is_not_a_model_call() {
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), make_work(), done(run("true")),
        ], "undo-no-model");
        rec.admin_outcomes.borrow_mut().push_back(dir_undo());
        e.handle("prep").unwrap();
        let before = e.model.prompts.borrow().len();
        let out = e.handle("undo").unwrap();
        assert!(out.iter().any(|l| l.contains("Removed the folder")), "{out:?}");
        assert_eq!(e.model.prompts.borrow().len(), before, "undo is deterministic, like stop and approvals");
        // And the same holds for the "nothing to undo" path.
        e.handle("undo").unwrap();
        assert_eq!(e.model.prompts.borrow().len(), before);
    }

    #[test]
    fn a_failing_reversal_is_reported_and_the_rest_still_run() {
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), make_work(),
            act(1, Action::Install { packages: vec!["cowsay".into()] }),
            done(run("true")),
        ], "undo-partial");
        rec.admin_outcomes.borrow_mut().push_back(dir_undo());
        rec.admin_outcomes.borrow_mut().push_back(Outcome::ok("installed").with_undo(UndoEntry::PackagesAdded { packages: vec!["cowsay".into()] }));
        // Newest first, so the packages row is the one that fails.
        rec.reverse_outcomes.borrow_mut().push_back(Outcome::err("pacman: database is locked"));
        e.handle("prep").unwrap();
        let out = e.handle("undo").unwrap();
        assert!(out.iter().any(|l| l.contains("Could not undo") && l.contains("cowsay") && l.contains("database is locked")), "{out:?}");
        assert!(out.iter().any(|l| l.contains("Removed the folder")), "a failed reversal never stops the rest: {out:?}");
        assert_eq!(rec.reversed.borrow().len(), 2);
        assert!(e.handle("undo").unwrap()[0].contains("Nothing left to undo"), "a failed row is marked applied, never retried blindly");
    }

    #[test]
    fn cancelled_jobs_are_undoable() {
        let (mut e, rec, _) = engine_with(vec![
            housekeep(), plan(), make_work(), Move::Ask { questions: vec!["Anything else?".into()] },
        ], "undo-cancelled");
        rec.admin_outcomes.borrow_mut().push_back(dir_undo());
        e.handle("prep").unwrap();
        assert_eq!(e.open_job().unwrap().unwrap().state, State::WaitingAnswer);
        let stopped = e.handle("stop").unwrap();
        assert_eq!(stopped, vec!["Stopped the job in housekeeping.".to_string()], "a housekeeping job has no project name to print");
        let out = e.handle("undo").unwrap();
        assert_eq!(rec.reversed.borrow().len(), 1, "a cancelled job is undoable too");
        assert!(out.iter().any(|l| l.contains("Removed the folder")), "{out:?}");
    }

    #[test]
    fn second_undo_reaches_the_previous_job() {
        fn set_root(value: &str) -> Move { act(1, Action::SetSetting { key: "projects_root".into(), value: value.into() }) }
        let (mut e, _, _) = engine_with(vec![
            housekeep(), plan(), set_root("/data/work"), done(run("true")),
            housekeep(), plan(), set_root("/data/other"), done(run("true")),
        ], "undo-twice");
        e.handle("put the projects in /data/work").unwrap();
        e.handle("no, /data/other").unwrap();
        assert_eq!(e.store.get_setting("projects_root").unwrap().as_deref(), Some("/data/other"));

        let out = e.handle("undo").unwrap();
        assert!(out.iter().any(|l| l.contains("Setting projects_root back to /data/work")), "{out:?}");
        assert_eq!(e.store.get_setting("projects_root").unwrap().as_deref(), Some("/data/work"), "the newest job goes back first");
        let out = e.handle("undo").unwrap();
        assert!(out.iter().any(|l| l.contains("Cleared setting projects_root")), "{out:?}");
        assert_eq!(e.store.get_setting("projects_root").unwrap(), None, "a second undo reaches the job before that");
        assert!(e.handle("undo").unwrap()[0].contains("Nothing left to undo"));
    }

    #[test]
    fn undoing_a_project_job_says_the_files_were_not_covered() {
        // A temp root is never btrfs, so the job got no snapshot row — exactly the pre-1c
        // plain-folder case the user must be told about.
        let (mut e, rec, _) = engine_with(vec![
            start("p", true), plan(), act(1, write("BLUEPRINT.md")),
            act(1, Action::Install { packages: vec!["cowsay".into()] }),
            act(1, write("BLUEPRINT.md")), done(run("true")),
        ], "undo-plain-folder");
        rec.admin_outcomes.borrow_mut().push_back(Outcome::ok("installed").with_undo(UndoEntry::PackagesAdded { packages: vec!["cowsay".into()] }));
        e.handle("go").unwrap();
        let out = e.handle("undo").unwrap();
        assert_eq!(out[0], "Undoing the last job (p):", "{out:?}");
        assert!(out.iter().any(|l| l == "Files in *p* were not covered: the project predates undo."), "{out:?}");
    }

    #[test]
    fn happy_path_with_no_risky_action_leaves_zero_blocked_rows_in_the_log() {
        let (mut e, log_path) = engine_with_log(happy_path(), "approval-log-happy");
        e.handle("make it, decide yourself").unwrap();
        let conn = rusqlite::Connection::open(&log_path).unwrap();
        let blocked: i64 = conn.query_row("SELECT COUNT(*) FROM actions WHERE outcome LIKE 'blocked:%'", [], |r| r.get(0)).unwrap();
        assert_eq!(blocked, 0);
        let _ = std::fs::remove_file(&log_path);
    }
}
