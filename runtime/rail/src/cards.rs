//! Cards from events (1d design §4.2). Pure data: the window renders it, the test reads it.
use aios_proto::{ChangedFile, Event, FileKind, JobState, Waiting};

#[derive(Debug, Clone, PartialEq)]
pub struct Button { pub label: String, pub say: String }

#[derive(Debug, Clone, PartialEq)]
pub struct StepLine { pub text: String, pub done: bool, pub ok: bool, pub detail: Option<String> }

#[derive(Debug, Clone, PartialEq)]
pub enum CardKind {
    You, Said,
    Building { name: String, understood: String, steps: Vec<StepLine>, actions: Vec<StepLine>, collapsed: bool },
    /// `options[i]`: buttons for `questions[i]`, maybe none.
    NeedsAnswer { questions: Vec<String>, options: Vec<Vec<String>> },
    Done { text: String, check: Option<String>, files: Vec<ChangedFile>, learned: Vec<String> },
    Failed { text: String, files: Vec<ChangedFile> },
    Stopped { text: String, files: Vec<ChangedFile> },
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

/// Whether the AI is busy after this event, for the spinner (the owner, 2026-09-19: "there is not
/// any sort of sign that the AI is processing or it's stopped"). Your own message and any step of
/// a job mean it is working; anything that hands the turn back to the person means it is not.
/// `None`: this event says nothing either way.
pub fn busy_after(ev: &Event) -> Option<bool> {
    match ev {
        Event::You { .. } | Event::Understood { .. } | Event::Plan { .. } | Event::Step { .. } => Some(true),
        Event::Said { .. } | Event::NeedsAnswer { .. } | Event::Stopped { .. }
        | Event::Error { .. } | Event::Learned { .. } => Some(false),
        // A slow local model can take minutes for the learning turn after Done/Failed, so the
        // spinner keeps turning until the Learned that always follows (engine.rs `learn`) stops it.
        Event::Done { .. } | Event::Failed { .. } | Event::Busy { .. } | Event::State { .. } | Event::Skills { .. } | Event::Projects { .. } | Event::Cleared {} => None,
    }
}

#[derive(Default)]
pub struct Cards { pub list: Vec<Card>, building: Option<usize> }

/// The answer the person has clicked so far on a question card: the choice alone for one question,
/// each question with its choice for several. `true` once every question has one, and it is sent;
/// until then the text waits in the box, where a question with no buttons gets typed.
pub fn answer(questions: &[String], picks: &[Option<String>]) -> (String, bool) {
    if let [_] = questions { if let Some(Some(p)) = picks.first() { return (p.clone(), true) } }
    let text = questions.iter().zip(picks).filter_map(|(q, p)| p.as_ref().map(|p| format!("{q} {p}."))).collect::<Vec<_>>().join(" ");
    (text, picks.len() == questions.len() && picks.iter().all(Option::is_some))
}

fn btn(label: &str, say: &str) -> Button { Button { label: label.into(), say: say.into() } }

const KEEP: &str = "keep what you learned";
const DISCARD: &str = "discard what you learned";
/// The Stopped card's Clear: not words for the AI, the rail sends a `clear` request for it.
pub const CLEAR: &str = "clear the chat";

fn result_card(kind: CardKind, text: &str, files: &[ChangedFile], job_id: &str) -> Card {
    Card {
        kind, text: text.into(), buttons: vec![],
        opens: files.iter().map(|f| f.path.clone()).collect(),
        thumbnails: files.iter().filter(|f| f.kind == FileKind::Image).map(|f| f.path.clone()).collect(),
        job_id: Some(job_id.into()),
    }
}

impl Cards {
    fn push(&mut self, c: Card) -> Vec<Change> { self.list.push(c); vec![Change::Added(self.list.len() - 1)] }

    fn open_building(&mut self, job_id: &str, name: &str, understood: &str) -> usize {
        self.list.push(Card { kind: CardKind::Building { name: name.into(), understood: understood.into(), steps: vec![], actions: vec![], collapsed: false }, text: understood.into(), buttons: vec![btn("Stop", "stop")], opens: vec![], thumbnails: vec![], job_id: Some(job_id.into()) });
        let i = self.list.len() - 1; self.building = Some(i); i
    }

    fn close_building(&mut self) -> Vec<Change> {
        let Some(i) = self.building.take() else { return vec![] };
        if let CardKind::Building { collapsed, .. } = &mut self.list[i].kind { *collapsed = true; }
        self.list[i].buttons.clear();
        vec![Change::Updated(i)]
    }

    /// The card of the turn `job_id`, opened on its first to-do list or action (one-loop §3).
    /// The changes: an older card collapsed and this one added, or this one updated.
    fn building_for(&mut self, job_id: &str) -> (usize, Vec<Change>) {
        match self.building.filter(|&i| self.list[i].job_id.as_deref() == Some(job_id)) {
            Some(i) => (i, vec![Change::Updated(i)]),
            None => { let mut ch = self.close_building(); let i = self.open_building(job_id, "", ""); ch.push(Change::Added(i)); (i, ch) }
        }
    }

    pub fn apply(&mut self, ev: &Event) -> Vec<Change> {
        match ev {
            Event::You { text } => self.push(Card { kind: CardKind::You, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Said { text } => self.push(Card { kind: CardKind::Said, text: text.clone(), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: None }),
            Event::Understood { job_id, name, text, .. } => { let i = self.open_building(job_id, name, text); vec![Change::Added(i)] }
            Event::Plan { job_id, steps } => {
                let (i, ch) = self.building_for(job_id);
                if let CardKind::Building { steps: s, .. } = &mut self.list[i].kind {
                    *s = steps.iter().map(|t| {
                        let done = t.starts_with("[x] ");
                        let text = t.strip_prefix("[x] ").or_else(|| t.strip_prefix("[ ] ")).unwrap_or(t);
                        StepLine { text: text.into(), done, ok: done, detail: None }
                    }).collect();
                }
                ch
            }
            Event::Step { job_id, plan_step, text, ok } if *plan_step == 0 => {
                let (i, ch) = self.building_for(job_id);
                if let CardKind::Building { actions, .. } = &mut self.list[i].kind {
                    actions.push(StepLine { text: text.clone(), done: true, ok: *ok, detail: None });
                }
                ch
            }
            // An older engine on a reconnect still numbers its steps from 1: today's behaviour.
            Event::Step { plan_step, text, ok, .. } => match self.building {
                Some(i) => {
                    if let CardKind::Building { steps, .. } = &mut self.list[i].kind {
                        if let Some(s) = steps.get_mut(plan_step.saturating_sub(1)) { s.done = true; s.ok = *ok; s.detail = Some(text.clone()); }
                    }
                    vec![Change::Updated(i)]
                }
                None => vec![],
            },
            Event::NeedsAnswer { job_id, questions, options } => {
                let mut ch = self.close_building();
                ch.extend(self.push(Card { kind: CardKind::NeedsAnswer { questions: questions.clone(), options: options.clone() }, text: questions.join("\n"), buttons: vec![], opens: vec![], thumbnails: vec![], job_id: Some(job_id.clone()) }));
                ch
            }
            Event::Done { job_id, text, check, files, .. } => {
                let mut ch = self.close_building();
                ch.extend(self.push(result_card(CardKind::Done { text: text.clone(), check: check.clone(), files: files.clone(), learned: vec![] }, text, files, job_id)));
                ch
            }
            Event::Failed { job_id, text, files } => { let mut ch = self.close_building(); ch.extend(self.push(result_card(CardKind::Failed { text: text.clone(), files: files.clone() }, text, files, job_id))); ch }
            Event::Stopped { job_id, text, files } => {
                let mut ch = self.close_building();
                let mut card = result_card(CardKind::Stopped { text: text.clone(), files: files.clone() }, text, files, job_id);
                // Stop keeps the chat, to say it again differently; this starts clean.
                card.buttons.push(btn("Clear", CLEAR));
                ch.extend(self.push(card));
                ch
            }
            Event::Learned { job_id, lines, pending } => {
                // Keep/Discard act on whatever waits now, and every learning turn drops what the one
                // before left waiting: an older card's buttons would keep an entry nobody read there.
                let mut ch: Vec<Change> = vec![];
                for (i, c) in self.list.iter_mut().enumerate() {
                    let had = c.buttons.len();
                    c.buttons.retain(|b| b.say != KEEP && b.say != DISCARD);
                    if c.buttons.len() != had { ch.push(Change::Updated(i)); }
                }
                // Every learning turn now emits Learned, even when nothing was kept — that empty
                // one draws nothing (Task 8 ruling): the spinner alone is what it is for.
                if lines.is_empty() { return ch; }
                let keep = || [btn("Keep", KEEP), btn("Discard", DISCARD)];
                let at = self.list.iter().rposition(|c| c.job_id.as_deref() == Some(job_id.as_str()) && matches!(c.kind, CardKind::Done { .. }));
                match at {
                    Some(i) => {
                        if let CardKind::Done { learned, .. } = &mut self.list[i].kind { learned.extend(lines.iter().cloned()); }
                        if *pending { self.list[i].buttons.extend(keep()); }
                        if !ch.contains(&Change::Updated(i)) { ch.push(Change::Updated(i)); }
                    }
                    // No Done card to join — a failed job's "marked as not working", or a Done card
                    // cleared away: its own card, with Keep and Discard if something waits.
                    None => {
                        let buttons = if *pending { keep().to_vec() } else { vec![] };
                        ch.extend(self.push(Card { kind: CardKind::Said, text: lines.join("\n"), buttons, opens: vec![], thumbnails: vec![], job_id: None }));
                    }
                }
                ch
            }
            // The Skills screen and the Projects page are their own windows (main.rs), not cards.
            Event::Skills { .. } | Event::Projects { .. } => vec![],
            // The whole column is redrawn (main.rs), not one card.
            Event::Cleared {} => { self.clear(); vec![] }
            Event::Busy { text, .. } | Event::Error { text } => vec![Change::Line(text.clone())],
            Event::State { job: None } => vec![],
            Event::State { job: Some(st) } => self.rebuild(st),
        }
    }

    /// The Clear button: every card goes but the running job's own and the question still waiting
    /// on the person, if the last card is one. Keeping everything after the job's card kept a long
    /// job's whole back-and-forth, and Clear looked broken (the owner's run, 2026-09-19).
    pub fn clear(&mut self) {
        let Some(b) = self.building else { self.list.clear(); return };
        let waiting = self.list.len() - 1 > b
            && matches!(self.list.last().map(|c| &c.kind), Some(CardKind::NeedsAnswer { .. }));
        let kept: Vec<Card> = std::iter::once(self.list[b].clone()).chain(waiting.then(|| self.list.last().cloned()).flatten()).collect();
        self.list = kept;
        self.building = Some(0);
    }

    /// A job is on screen and still running: the title bar's Stop shows.
    pub fn running(&self) -> bool { self.building.is_some() }

    /// A reopened rail: the open turn as one Building card (its to-do list and actions so far)
    /// plus its waiting card, if any. Earlier finished turns are not replayed (1d §4.2).
    fn rebuild(&mut self, st: &JobState) -> Vec<Change> {
        // A second State for a turn already on screen is a reconnect, not a new one: refill that
        // card in place, even one a `NeedsAnswer` already closed while it waited — starting a
        // fresh one every poll would duplicate the turn on screen.
        let open = self.list.iter().position(|c| c.job_id.as_deref() == Some(st.id.as_str()) && matches!(c.kind, CardKind::Building { .. }));
        let (i, mut ch) = match open {
            Some(i) => (i, vec![Change::Updated(i)]),
            None => { let i = self.open_building(&st.id, &st.name, &st.understood); (i, vec![Change::Added(i)]) }
        };
        if let CardKind::Building { steps, actions, .. } = &mut self.list[i].kind {
            *steps = st.plan.iter().map(|t| {
                let done = t.starts_with("[x] ");
                let text = t.strip_prefix("[x] ").or_else(|| t.strip_prefix("[ ] ")).unwrap_or(t);
                StepLine { text: text.into(), done, ok: done, detail: None }
            }).collect();
            for s in &st.steps {
                if s.plan_step == 0 { continue; }
                if let Some(l) = steps.get_mut(s.plan_step - 1) { l.done = true; l.ok = s.ok; l.detail = Some(s.text.clone()); }
            }
            *actions = st.steps.iter().filter(|s| s.plan_step == 0).map(|s| StepLine { text: s.text.clone(), done: true, ok: s.ok, detail: None }).collect();
        }
        ch.extend(self.waiting_card(st));
        ch
    }

    /// The card for what the job is waiting on, unless it is already the last card on screen —
    /// a reconnect resends the same State and would otherwise ask the same question twice.
    fn waiting_card(&mut self, st: &JobState) -> Vec<Change> {
        let (kind, ev) = match &st.waiting {
            Waiting::None => return vec![],
            Waiting::Answer { questions, options } => (CardKind::NeedsAnswer { questions: questions.clone(), options: options.clone() },
                Event::NeedsAnswer { job_id: st.id.clone(), questions: questions.clone(), options: options.clone() }),
        };
        if self.list.last().map(|c| &c.kind) == Some(&kind) { return vec![] }
        self.apply(&ev)
    }
}
