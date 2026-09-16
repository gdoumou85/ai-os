use executor::action::Action;
use serde::{Deserialize, Serialize};

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
    pub rejections: u32,
    pub note_to_model: Option<String>,
    pub last_code_change: usize,
    pub last_blueprint_update: usize,
    pub outcome_text: String,
}

impl Job {
    pub fn new(project: &str, goal: &str, creative: bool, understood: &str) -> Job {
        let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        Job {
            id: format!("{project}-{secs}"),
            project: project.into(), goal: goal.into(), creative, understood: understood.into(),
            state: if creative { State::Planning } else { State::Asking },
            answers: vec![], pending_questions: vec![], plan: vec![], steps: vec![],
            pending_action: None, pending_reason: String::new(), failed_actions: vec![],
            rejections: 0, note_to_model: None, last_code_change: 0, last_blueprint_update: 0,
            outcome_text: String::new(),
        }
    }
    pub fn is_open(&self) -> bool { !matches!(self.state, State::Done | State::Failed | State::Cancelled) }
}
