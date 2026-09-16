use crate::job::{Job, State};
use crate::model::{Model, ModelError};
use crate::moves::Move;
use crate::prompt;
use crate::store::{Store, StoreError};
use executor::executor::Executor;
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

    pub fn open_job(&self) -> Option<Job> { self.store.open_job().ok().flatten() }

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
        if let Some(mut job) = self.open_job() {
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
                if let Some(r) = remember { self.store.add_instruction(&r)?; }
                let job = Job::new(&name, &goal, creative, &understood);
                self.store.save_job(&job)?;
                let mut out = vec![understood];
                out.extend(self.run_turns(job)?);
                Ok(out)
            }
            other => Ok(vec![format!("(I answered out of turn — {other:?} — please say that again.)")]),
        }
    }

    /// Mark a job finished (done/failed/cancelled) and report the one line that explains it.
    fn finish(&mut self, mut job: Job, state: State, message: String) -> Result<Vec<String>, EngineError> {
        job.state = state;
        job.outcome_text = message.clone();
        self.store.save_job(&job)?;
        Ok(vec![message])
    }

    /// Task 8 fills this in.
    fn run_turns(&mut self, _job: Job) -> Result<Vec<String>, EngineError> { Ok(vec![]) }
    fn resume_after_approval(&mut self, _job: Job, _approved: bool, _text: &str) -> Result<Vec<String>, EngineError> { Ok(vec![]) }
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
        assert!(e.open_job().is_none());
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
        let job = e.open_job().expect("a job is open");
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
        assert!(e.open_job().is_none());
        assert_eq!(e.model.prompts.borrow().len(), 1);
    }
}
