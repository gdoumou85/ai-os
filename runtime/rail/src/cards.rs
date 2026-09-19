//! Cards from events (1d design §4.2). Pure data: the window renders it, the test reads it.
use aios_proto::{ChangedFile, Event, FileKind, JobState, Waiting};

#[derive(Debug, Clone, PartialEq)]
pub struct Button { pub label: String, pub say: String }

#[derive(Debug, Clone, PartialEq)]
pub struct StepLine { pub text: String, pub done: bool, pub ok: bool, pub detail: Option<String> }

#[derive(Debug, Clone, PartialEq)]
pub enum CardKind {
    You, Said,
    Building { name: String, understood: String, steps: Vec<StepLine>, collapsed: bool },
    NeedsAnswer { questions: Vec<String> },
    NeedsOk { what: String, why: String },
    Done { text: String, check: Option<String>, files: Vec<ChangedFile>, windows: Vec<String> },
    Failed { text: String, files: Vec<ChangedFile> },
    Stopped { text: String, files: Vec<ChangedFile> },
    Undone { lines: Vec<aios_proto::UndoLine>, notes: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub kind: CardKind,
    pub text: String,
    pub buttons: Vec<Button>,
    /// Paths with an Open button, in order. Always from the service, never from user text.
    pub opens: Vec<String>,
    pub thumbnails: Vec<String>,
    pub job_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change { Added(usize), Updated(usize), Line(String) }

#[derive(Default)]
pub struct Cards { pub list: Vec<Card>, building: Option<usize> }

fn btn(label: &str, say: &str) -> Button { Button { label: label.into(), say: say.into() } }

fn result_card(kind: CardKind, text: &str, files: &[ChangedFile], job_id: &str) -> Card {
    Card {
        kind, text: text.into(), buttons: vec![btn("Undo", "undo")],
        opens: files.iter().map(|f| f.path.clone()).collect(),
        thumbnails: files.iter().filter(|f| f.kind == FileKind::Image).map(|f| f.path.clone()).collect(),
        job_id: Some(job_id.into()),
    }
}

impl Cards {
    fn push(&mut self, c: Card) -> Vec<Change> { self.list.push(c); vec![Change::Added(self.list.len() - 1)] }

    fn open_building(&mut self, job_id: &str, name: &str, understood: &str) -> usize {
        self.list.push(Card { kind: CardKind::Building { name: name.into(), understood: understood.into(), steps: vec![], collapsed: false }, text: understood.into(), buttons: vec![btn("Stop", "stop")], opens: vec![], thumbnails: vec![], job_id: Some(job_id.into()) });
        let i = self.list.len() - 1; self.building = Some(i); i
    }

    fn close_building(&mut self) -> Vec<Change> {
        let Some(i) = self.building.take() else { return vec![] };
        if let CardKind::Building { collapsed, .. } = &mut self.list[i].kind { *collapsed = true; }
        self.list[i].buttons.clear();
        vec![Change::Updated(i)]
    }

    pub fn apply(&mut self, ev: &Event) -> Vec<Change> {
        match ev {
            Event::You { text } => self.push(Card { kind: CardKind::You, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Said { text } => self.push(Card { kind: CardKind::Said, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Understood { job_id, name, text, .. } => { let i = self.open_building(job_id, name, text); vec![Change::Added(i)] }
            Event::Plan { steps, .. } => match self.building {
                Some(i) => { if let CardKind::Building { steps: s, .. } = &mut self.list[i].kind { *s = steps.iter().map(|t| StepLine { text: t.clone(), done: false, ok: false, detail: None }).collect(); } vec![Change::Updated(i)] }
                None => vec![],
            },
            Event::Step { plan_step, text, ok, .. } => match self.building {
                Some(i) => {
                    if let CardKind::Building { steps, .. } = &mut self.list[i].kind {
                        if let Some(s) = steps.get_mut(plan_step.saturating_sub(1)) { s.done = true; s.ok = *ok; s.detail = Some(text.clone()); }
                    }
                    vec![Change::Updated(i)]
                }
                None => vec![],
            },
            Event::NeedsAnswer { job_id, questions } => self.push(Card { kind: CardKind::NeedsAnswer { questions: questions.clone() }, text: questions.join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::NeedsOk { job_id, what, why } => self.push(Card { kind: CardKind::NeedsOk { what: what.clone(), why: why.clone() }, text: format!("{what}\n{why}"), buttons: vec![btn("Yes", "yes"), btn("No", "no")], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::Done { job_id, text, check, files, windows } => {
                let mut ch = self.close_building();
                let shown = match aios_proto::window_note(windows) { Some(n) => format!("{text}\n{n}"), None => text.clone() };
                ch.extend(self.push(result_card(CardKind::Done { text: text.clone(), check: check.clone(), files: files.clone(), windows: windows.clone() }, &shown, files, job_id)));
                ch
            }
            Event::Failed { job_id, text, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Failed { text: text.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Stopped { job_id, text, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Stopped { text: text.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Undone { job_id, lines, notes, .. } => self.push(Card { kind: CardKind::Undone { lines: lines.clone(), notes: notes.clone() }, text: lines.iter().map(|l| l.text.clone()).collect::<Vec<_>>().join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }),
            Event::Busy { text, .. } | Event::Error { text } => vec![Change::Line(text.clone())],
            Event::State { job: None } => vec![],
            Event::State { job: Some(st) } => self.rebuild(st),
        }
    }

    /// The Clear button: every card goes but the running job's, and whatever came after it — its
    /// Stop button and a Yes/No still waiting on the person must stay reachable.
    pub fn clear(&mut self) {
        let from = self.building.unwrap_or(self.list.len());
        self.list.drain(..from);
        self.building = self.building.map(|_| 0);
    }

    /// A reopened rail: the open job as one Building card (steps ticked so far) plus its
    /// waiting card, if any. Earlier finished jobs are not replayed (1d §4.2).
    fn rebuild(&mut self, st: &JobState) -> Vec<Change> {
        // A second State for the job already on screen is a reconnect, not a new job: refill that
        // card. Opening another one would strand the first, Stop button and all.
        let open = self.building.filter(|&i| self.list[i].job_id.as_deref() == Some(st.id.as_str()));
        let (i, mut ch) = match open {
            Some(i) => (i, vec![Change::Updated(i)]),
            None => { let i = self.open_building(&st.id, &st.name, &st.understood); (i, vec![Change::Added(i)]) }
        };
        if let CardKind::Building { steps, .. } = &mut self.list[i].kind {
            *steps = st.plan.iter().map(|t| StepLine { text: t.clone(), done: false, ok: false, detail: None }).collect();
            for s in &st.steps { if let Some(l) = steps.get_mut(s.plan_step.saturating_sub(1)) { l.done = true; l.ok = s.ok; l.detail = Some(s.text.clone()); } }
        }
        ch.extend(self.waiting_card(st));
        ch
    }

    /// The card for what the job is waiting on, unless it is already the last card on screen —
    /// a reconnect resends the same State and would otherwise ask the same question twice.
    fn waiting_card(&mut self, st: &JobState) -> Vec<Change> {
        let (kind, ev) = match &st.waiting {
            Waiting::None => return vec![],
            Waiting::Answer { questions } => (CardKind::NeedsAnswer { questions: questions.clone() },
                Event::NeedsAnswer { job_id: st.id.clone(), questions: questions.clone() }),
            Waiting::Ok { what, why } => (CardKind::NeedsOk { what: what.clone(), why: why.clone() },
                Event::NeedsOk { job_id: st.id.clone(), what: what.clone(), why: why.clone() }),
        };
        if self.list.last().map(|c| &c.kind) == Some(&kind) { return vec![] }
        self.apply(&ev)
    }
}
