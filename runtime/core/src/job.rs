use executor::action::Action;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};

/// I2: two `Job::new` calls in the same millisecond (never mind the same second) must never
/// collide — a colliding id makes `save_job` upsert the second job over the first and merges
/// both jobs' executor-log rows under one `job_id`. Process-wide and monotonic is enough: ids
/// only need to be unique within one running executor, never across restarts or machines.
static JOB_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Asking, Planning,
    /// A job saved while it waited for an OK (before full access) carries on working.
    #[serde(alias = "waiting_approval")]
    Working,
    WaitingAnswer, Done, Failed, Cancelled,
}

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Asking => "asking", State::Planning => "planning", State::Working => "working",
            State::WaitingAnswer => "waiting_answer",
            State::Done => "done", State::Failed => "failed", State::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StepRecord { pub plan_step: usize, pub action: Action, pub ok: bool, pub detail: String }

/// The whole job record (1b spec §3): saved after every turn, so any state resumes from disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub project: String,
    pub goal: String,
    pub creative: bool,
    pub understood: String,
    pub state: State,
    pub answers: Vec<(String, String)>,
    pub pending_questions: Vec<String>,
    /// The buttons offered with each pending question; a job saved before them has none.
    #[serde(default)]
    pub pending_options: Vec<Vec<String>>,
    pub plan: Vec<String>,
    pub steps: Vec<StepRecord>,
    pub failed_actions: Vec<String>,
    pub rejections: u32,
    /// Bounded like `rejections`/MAX_FAILS_PER_STEP: a model that only ever replans never
    /// produces output otherwise (engine::MAX_REPLANS).
    #[serde(default)]
    pub replans: u32,
    pub note_to_model: Option<String>,
    pub last_code_change: usize,
    pub last_blueprint_update: usize,
    pub outcome_text: String,
    /// True for a housekeeping job (machine-level: folders, settings, tools) — no project, no
    /// blueprint. `#[serde(default)]`: a job saved before this field existed must still
    /// deserialise (I3).
    #[serde(default)]
    pub housekeeping: bool,
    /// The job's own workspace — the folder every one of its actions runs in. Carried on the
    /// job rather than derived from `project`, so moving the projects root (or a project) never
    /// silently redirects a job that is already running. `#[serde(default)]`: a job
    /// saved before this field existed must still deserialise (I3).
    #[serde(default)]
    pub folder: String,
    /// True when this job's `Start` created the project folder: a new project must leave a
    /// BLUEPRINT.md behind.
    #[serde(default)]
    pub new_project: bool,
    /// What the user actually typed to start this job, word for word. `goal` is the model's own
    /// paraphrase and loses what it did not think mattered — the live 1c run watched "where all
    /// my projects will live from now on" become "for project storage", and a job cannot act on
    /// what it never saw. `#[serde(default)]`: a job saved before this field existed must still deserialise (I3).
    #[serde(default)]
    pub request: String,
    /// When the job began (unix seconds): the Done card lists what changed since then.
    /// `#[serde(default)]`: a job saved before 1d has no such key.
    #[serde(default)]
    pub started_at: u64,
    /// How many `done` moves the blueprint gate has held back on this job. Its own counter, not
    /// the grammar `rejections` budget of two: a gated `done` is a legal move refused by policy
    /// and the fix is one concrete extra step, so two tries is not enough room — the live 1d run
    /// lost two jobs to a second `done` arriving before the blueprint write. Bounded all the
    /// same (engine::MAX_DONE_GATED), so a model that only ever says done still ends.
    /// `#[serde(default)]`: a job saved before this field existed must still deserialise (I3).
    #[serde(default)]
    pub done_gated: u32,
    /// The craft notebooks this job belongs to (Phase 3 §2). `serde(default)`: I3.
    #[serde(default)]
    pub skills: Vec<String>,
    /// The tips shown on every turn, fixed when the job starts (Phase 3 §3). `serde(default)`: I3.
    #[serde(default)]
    pub notes_block: String,
    /// The keys (`notebook/topic`) of those tips, so the learning turn can mark only what was shown.
    #[serde(default)]
    pub shown_notes: Vec<String>,
    /// The first step taken under the current plan: a replan starts the failure counts and the
    /// card's ticks afresh, since its step 2 is not the old step 2. `serde(default)`: I3.
    #[serde(default)]
    pub plan_from: usize,
    /// The projects whose notes this turn has already been shown (one-loop design §2).
    #[serde(default)]
    pub projects_seen: Vec<String>,
}

impl Job {
    pub fn new(project: &str, folder: &str, goal: &str, creative: bool, understood: &str) -> Job {
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let counter = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        Job {
            id: format!("{project}-{millis}-{counter}"),
            project: project.into(), goal: goal.into(), creative, understood: understood.into(),
            state: if creative { State::Planning } else { State::Asking },
            answers: vec![], pending_questions: vec![], pending_options: vec![], plan: vec![], steps: vec![],
            failed_actions: vec![], rejections: 0, replans: 0,
            note_to_model: None, last_code_change: 0, last_blueprint_update: 0,
            outcome_text: String::new(), housekeeping: false,
            folder: folder.into(), new_project: false, request: String::new(),
            started_at: (millis / 1000) as u64,
            done_gated: 0,
            skills: vec![], notes_block: String::new(), shown_notes: vec![], plan_from: 0, projects_seen: vec![],
        }
    }

    /// A machine-level job: no project, no blueprint, and a folder the caller hands it
    /// (Task 8 wires `/data/housekeeping`). Never creative — housekeeping touches the machine,
    /// so the model gets its chance to ask first.
    pub fn new_housekeeping(folder: &str, goal: &str, understood: &str) -> Job {
        let mut job = Job::new("", folder, goal, false, understood);
        job.housekeeping = true;
        job
    }

    /// One user message's worth of work (one-loop design §1). The job record carries it because
    /// the learning turn reads a job; `plan` holds the to-do list, `steps` the actions.
    pub fn turn(folder: &str, request: &str) -> Job {
        let mut j = Job::new("turn", folder, request, false, "");
        j.housekeeping = true;
        j.request = request.into();
        j.state = State::Working;
        j
    }

    pub fn is_open(&self) -> bool { !matches!(self.state, State::Done | State::Failed | State::Cancelled) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_jobs_made_back_to_back_never_collide() {
        let a = Job::new("p", "/data/projects/p", "g", true, "u");
        let b = Job::new("p", "/data/projects/p", "g", true, "u");
        assert_ne!(a.id, b.id, "same project, same millisecond is possible — the counter must still separate them");
    }

    /// I3: a `Job` saved before `replans` existed has no such key in its JSON.
    #[test]
    fn job_without_replans_key_still_deserialises() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        assert!(value.as_object_mut().unwrap().remove("replans").is_some());
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(back.replans, 0);
    }

    /// A job saved before full access, waiting for an OK and carrying the old fields, loads and
    /// carries on working.
    #[test]
    fn a_job_that_waited_for_an_ok_loads_as_working() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        let obj = value.as_object_mut().unwrap();
        obj.insert("state".into(), "waiting_approval".into());
        obj.insert("declined_actions".into(), serde_json::json!([]));
        obj.insert("pending_reason".into(), "network".into());
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(back.state, State::Working);
    }

    /// I3 again, for the three keys this task adds: a job saved before `folder`,
    /// `housekeeping` and `new_project` existed has none of them in its JSON.
    #[test]
    fn job_without_the_new_keys_still_deserialises() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        let obj = value.as_object_mut().unwrap();
        assert!(obj.remove("folder").is_some());
        assert!(obj.remove("housekeeping").is_some());
        assert!(obj.remove("new_project").is_some());
        let back: Job = serde_json::from_value(value).unwrap();
        assert!(back.folder.is_empty());
        assert!(!back.housekeeping);
        assert!(!back.new_project);
    }

    #[test]
    fn started_at_is_set_and_optional_on_the_wire() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        assert!(job.started_at > 1_700_000_000);
        let mut value = serde_json::to_value(&job).unwrap();
        value.as_object_mut().unwrap().remove("started_at");
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(back.started_at, 0);
    }

    #[test]
    fn a_job_saved_before_skills_still_deserialises() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        let obj = value.as_object_mut().unwrap();
        for k in ["skills", "notes_block", "shown_notes"] { assert!(obj.remove(k).is_some(), "{k}"); }
        let back: Job = serde_json::from_value(value).unwrap();
        assert!(back.skills.is_empty() && back.notes_block.is_empty() && back.shown_notes.is_empty());
    }
}
