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
pub enum State { Asking, Planning, Working, WaitingAnswer, WaitingApproval, Done, Failed, Cancelled }

impl State {
    pub fn as_str(&self) -> &'static str {
        match self {
            State::Asking => "asking", State::Planning => "planning", State::Working => "working",
            State::WaitingAnswer => "waiting_answer", State::WaitingApproval => "waiting_approval",
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
    pub plan: Vec<String>,
    pub steps: Vec<StepRecord>,
    pub pending_action: Option<(usize, Action)>,
    pub pending_reason: String,
    pub failed_actions: Vec<String>,
    /// A human's "no" to a risky action. Unlike `failed_actions`, this is never cleared by a
    /// later file write — a decline is not a fixable failure, it must never expire.
    /// `serde(default)`: a job saved before this field existed must still deserialise (I3;
    /// standing convention from now on — every field added to `Job` gets this).
    #[serde(default)]
    pub declined_actions: Vec<String>,
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
    /// deserialise (I3; standing convention — see `declined_actions`).
    #[serde(default)]
    pub housekeeping: bool,
    /// The job's own workspace — the folder every one of its actions runs in. Carried on the
    /// job rather than derived from `project`, so moving the projects root (or a project) never
    /// silently redirects a job that is already running. `#[serde(default)]`: see
    /// `declined_actions` (I3).
    #[serde(default)]
    pub folder: String,
    /// True when this job's `Start` created the project folder — what Task 9 needs to tell
    /// whether undoing the job means removing the folder or only putting its files back.
    #[serde(default)]
    pub new_project: bool,
}

impl Job {
    pub fn new(project: &str, folder: &str, goal: &str, creative: bool, understood: &str) -> Job {
        let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let counter = JOB_COUNTER.fetch_add(1, Ordering::Relaxed);
        Job {
            id: format!("{project}-{millis}-{counter}"),
            project: project.into(), goal: goal.into(), creative, understood: understood.into(),
            state: if creative { State::Planning } else { State::Asking },
            answers: vec![], pending_questions: vec![], plan: vec![], steps: vec![],
            pending_action: None, pending_reason: String::new(), failed_actions: vec![],
            declined_actions: vec![], rejections: 0, replans: 0,
            note_to_model: None, last_code_change: 0, last_blueprint_update: 0,
            outcome_text: String::new(), housekeeping: false,
            folder: folder.into(), new_project: false,
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

    /// I3: a `Job` saved before `replans`/`declined_actions` existed has neither key in its JSON.
    /// Both fields must default rather than fail deserialisation.
    #[test]
    fn job_without_replans_or_declined_actions_keys_still_deserialises() {
        let job = Job::new("p", "/data/projects/p", "g", true, "u");
        let mut value = serde_json::to_value(&job).unwrap();
        let obj = value.as_object_mut().unwrap();
        assert!(obj.remove("replans").is_some());
        assert!(obj.remove("declined_actions").is_some());
        let back: Job = serde_json::from_value(value).unwrap();
        assert_eq!(back.replans, 0);
        assert!(back.declined_actions.is_empty());
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
}
