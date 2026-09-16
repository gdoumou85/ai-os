use crate::job::{Job, State, StepRecord};
use crate::model::{Model, ModelError};
use crate::moves::Move;
use crate::prompt;
use crate::store::{Store, StoreError};
use executor::action::Action;
use executor::executor::{ExecOutcome, Executor};
use executor::log::{ActionLog, LogError};
use executor::worker::Worker;
use std::path::{Path, PathBuf};

pub type WorkerFactory = Box<dyn Fn(&Path) -> Box<dyn Worker>>;

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
    projects_root: PathBuf,
    log_path: Option<String>,
    workers: WorkerFactory,
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

impl<M: Model> Engine<M> {
    pub fn new(store: Store, model: M, projects_root: PathBuf, log_path: Option<String>, workers: WorkerFactory) -> Self {
        Self { store, model, projects_root, log_path, workers }
    }

    /// I3: a store error here used to be swallowed (`.ok().flatten()`) — a job left unreadable
    /// (a corrupt row, a locked file) looked exactly like "no job open", so the engine would
    /// silently start a fresh one over it. Propagate instead.
    pub fn open_job(&self) -> Result<Option<Job>, EngineError> { Ok(self.store.open_job()?) }

    fn workspace(&self, project: &str) -> PathBuf { self.projects_root.join(project) }

    /// One executor per project folder: classification and enforcement share the same
    /// workspace path by construction (parent §11 item 3).
    pub(crate) fn executor_for(&self, project: &str) -> Result<Executor<Box<dyn Worker>>, EngineError> {
        let ws = self.workspace(project);
        let log = match &self.log_path { Some(p) => ActionLog::open(p)?, None => ActionLog::open_in_memory()? };
        Ok(Executor::new((self.workers)(&ws), log, ws))
    }

    fn create_project_folder(&self, project: &str) -> Result<(), EngineError> {
        let ws = self.workspace(project);
        std::fs::create_dir_all(&ws)?;
        // Shared with the sandbox user (1b spec §7). Best effort: in tests there is no such group.
        // ponytail: shells out to chgrp/chmod; fine for one folder per project.
        let _ = std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status();
        let _ = std::process::Command::new("chmod").arg("2770").arg(&ws).status();
        Ok(())
    }

    pub(crate) fn read_blueprint(&self, project: &str) -> Option<String> {
        std::fs::read_to_string(self.workspace(project).join("BLUEPRINT.md")).ok()
    }

    /// The only entry point: one user message in, the lines to show the user out.
    pub fn handle(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        self.store.push_message("user", text)?;
        let out = self.handle_inner(text)?;
        for line in &out { self.store.push_message("assistant", line)?; }
        Ok(out)
    }

    fn handle_inner(&mut self, text: &str) -> Result<Vec<String>, EngineError> {
        if let Some(mut job) = self.open_job()? {
            if is_stop(text) {
                let message = format!("Stopped the job in {}.", job.project);
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
                if existing.is_none() { self.create_project_folder(&name)?; }
                let desc = existing.map(|p| p.description).unwrap_or(description);
                self.store.upsert_project(&name, &self.workspace(&name).display().to_string(), &desc)?;
                // I6: `remember` on a `start` move was saved silently — only the `Reply` arm told
                // the user. Same "(Noted for the future: …)" line here, computed before the value
                // moves into `add_instruction`.
                let note = remember.as_ref().map(|r| format!("(Noted for the future: {r})"));
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                let job = Job::new(&name, &goal, creative, &understood);
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

    /// The bounded last-run note (1b spec §6.6): written from the record when a job ends
    /// badly, wiped when a job in the project is proven done. Never more than one file.
    fn write_or_wipe_last_run(&self, job: &Job) {
        let path = self.workspace(&job.project).join("LAST_RUN.md");
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

    fn reject(&self, job: &mut Job, why: &str) -> Result<Option<Vec<String>>, EngineError> {
        job.rejections += 1;
        job.note_to_model = Some(format!("your last move was rejected: {why}"));
        if job.rejections >= Self::MAX_REJECTIONS {
            let text = format!("I gave up on {}: I kept answering in a way the system could not accept ({why}).", job.project);
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
        let exec = self.executor_for(&job.project)?;
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
                if outcome.ok {
                    match Self::is_blueprint(&action) {
                        Some(true) => job.last_blueprint_update = job.steps.len(),
                        Some(false) => job.last_code_change = job.steps.len(),
                        None => {}
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
                        let text = format!("I gave up on {}: plan step {plan_step} failed {fails} different ways. Last reason: {}", job.project, outcome.detail);
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
            let text = format!("I gave up on {}: {} steps without finishing.", job.project, Self::MAX_STEPS);
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
                let text = format!("I gave up on {}: {} steps without finishing.", job.project, Self::MAX_STEPS);
                return self.finish(job, State::Failed, text);
            }
            let last_run = std::fs::read_to_string(self.workspace(&job.project).join("LAST_RUN.md")).ok();
            let p = prompt::job_turn(&self.store.instructions()?, &job, self.read_blueprint(&job.project).as_deref(), last_run.as_deref());
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
                        let text = format!("I gave up on {}: the plan kept changing ({} replans) without progress.", job.project, job.replans);
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
                    if job.last_code_change > job.last_blueprint_update {
                        Some("update BLUEPRINT.md for what you changed before saying done".to_string())
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
                    return self.finish(job.clone(), State::Failed, format!("I gave up on {}: {reason}. Missing: {missing}.", job.project));
                }
                (_, Move::Act { .. }) | (_, Move::Done { .. }) | (_, Move::Replan { .. }) | (_, Move::GiveUp { .. }) => Some("give a plan first".to_string()),
                (_, Move::Plan { .. }) => Some("not now".to_string()),
                (_, Move::Reply { .. }) | (_, Move::Start { .. }) => Some("a job is running: use ask, plan, act, replan, done or give_up".to_string()),
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
        e.handle("add a menu to p").unwrap();
        assert_eq!(e.store.list_projects().unwrap().len(), 1, "same project row, not a second one");
        assert_eq!(e.store.list_projects().unwrap()[0].description, "prime printer", "the original description is kept");
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
            Outcome { ok: true, detail: "ok".into() },                 // blueprint write
            Outcome { ok: false, detail: "Traceback… NameError".into() }, // first check fails
            Outcome { ok: true, detail: "2 3 5 7".into() },             // second check passes
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
            Outcome { ok: true, detail: "ok".into() },
            Outcome { ok: false, detail: "gcc: not found".into() },
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
            Outcome { ok: true, detail: "ok".into() },
            Outcome { ok: false, detail: "NameError: prnt".into() },
            Outcome { ok: true, detail: "edited".into() },
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
        for _ in 0..4 { rec.outcomes.borrow_mut().push_back(Outcome { ok: false, detail: "boom".into() }); }
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
        rec.outcomes.borrow_mut().push_back(Outcome { ok: false, detail: "exit 1; stderr: NameError: prmes".into() });
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
        let r2 = rec.clone();
        let mut e2 = Engine::new(store, crate::model::FakeModel::new(vec![plan(), act(1, write("BLUEPRINT.md")), done(run("true"))]), root, None,
            Box::new(move |_| Box::new(crate::testing::ScriptedWorker(r2.clone())) as Box<dyn Worker>));
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
            Outcome { ok: true, detail: "ok".into() },                    // blueprint write
            Outcome { ok: false, detail: "NameError: prnt".into() },      // check fails
            Outcome { ok: true, detail: "edited".into() },                // edit succeeds
            Outcome { ok: true, detail: "2 3 5 7".into() },                // check passes
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
        let (mut e, rec, _) = engine_with(vec![], "cap-approval");
        let mut job = Job::new("p", "goal", true, "Starting p");
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
        let r2 = rec.clone();
        let root = crate::testing::temp_root(tag);
        let log_path = std::env::temp_dir().join(format!("ai-os-core-{tag}-log-{}.sqlite", std::process::id()));
        let _ = std::fs::remove_file(&log_path);
        let log_path_str = log_path.to_string_lossy().to_string();
        let e = Engine::new(
            crate::store::Store::open_in_memory().unwrap(),
            crate::model::FakeModel::new(moves),
            root,
            Some(log_path_str),
            Box::new(move |_ws| Box::new(crate::testing::ScriptedWorker(r2.clone())) as Box<dyn Worker>),
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
