use aios_proto::*;
use aios_rail::cards::{busy_after, Button, CardKind, Cards, Change};

fn j() -> String { "j1".into() }

#[test]
fn a_whole_job_becomes_the_right_cards() {
    let mut cards = Cards::default();
    assert_eq!(cards.apply(&Event::You { text: "make p".into() }), vec![Change::Added(0)]);
    assert!(matches!(cards.list[0].kind, CardKind::You));
    cards.apply(&Event::Understood { job_id: j(), name: "p".into(), text: "Starting p".into(), housekeeping: false });
    let b = 1;
    assert!(matches!(cards.list[b].kind, CardKind::Building { .. }));
    assert_eq!(cards.list[b].buttons, vec![Button { label: "Stop".into(), say: "stop".into() }]);
    assert_eq!(cards.apply(&Event::Plan { job_id: j(), steps: vec!["write".into(), "run".into()] }), vec![Change::Updated(b)]);
    cards.apply(&Event::Step { job_id: j(), plan_step: 1, text: "wrote a.py".into(), ok: true });
    cards.apply(&Event::Step { job_id: j(), plan_step: 2, text: "ran python3".into(), ok: false });
    cards.apply(&Event::Step { job_id: j(), plan_step: 2, text: "ran python3 a.py".into(), ok: true });
    let CardKind::Building { steps, .. } = &cards.list[b].kind else { panic!() };
    assert_eq!(steps.iter().map(|s| (s.text.as_str(), s.done, s.ok, s.detail.as_deref())).collect::<Vec<_>>(),
        vec![("write", true, true, Some("wrote a.py")), ("run", true, true, Some("ran python3 a.py"))]);

    cards.apply(&Event::NeedsOk { job_id: j(), what: "http post to x".into(), why: "network".into() });
    let ok = 2;
    assert!(matches!(&cards.list[ok].kind, CardKind::NeedsOk { what, why } if what == "http post to x" && why == "network"));
    assert_eq!(cards.list[ok].buttons, vec![Button { label: "Yes".into(), say: "yes".into() }, Button { label: "No".into(), say: "no".into() }]);
    cards.apply(&Event::Said { text: "it sends b".into() });
    cards.apply(&Event::NeedsOk { job_id: j(), what: "http post to x".into(), why: "network".into() });
    assert_eq!(cards.list.len(), 5, "said, then the OK asked again as a new card");

    let files = vec![ChangedFile { path: "/data/p/a.py".into(), kind: FileKind::Text, size: 3 }, ChangedFile { path: "/data/p/pic.png".into(), kind: FileKind::Image, size: 9 }];
    cards.apply(&Event::Done { job_id: j(), text: "finished".into(), check: Some("ran python3 a.py: ok".into()), files: files.clone(), windows: vec![] });
    let CardKind::Building { collapsed, .. } = &cards.list[b].kind else { panic!() };
    assert!(collapsed);
    let done = cards.list.last().unwrap();
    let CardKind::Done { text, check, files: f, .. } = &done.kind else { panic!() };
    assert_eq!((text.as_str(), check.as_deref()), ("finished", Some("ran python3 a.py: ok")));
    assert_eq!(f, &files);
    assert_eq!(done.buttons, vec![Button { label: "Undo".into(), say: "undo".into() }]);
    assert_eq!(done.opens, vec!["/data/p/a.py".to_string(), "/data/p/pic.png".to_string()]);
    assert_eq!(done.thumbnails, vec!["/data/p/pic.png".to_string()]);

    cards.apply(&Event::Undone { job_id: j(), name: "p".into(), lines: vec![UndoLine { text: "put back a.py".into(), ok: true }, UndoLine { text: "could not".into(), ok: false }], notes: vec!["Not covered: x".into()] });
    let CardKind::Undone { lines, notes } = &cards.list.last().unwrap().kind else { panic!() };
    assert_eq!(lines.len(), 2); assert_eq!(notes, &vec!["Not covered: x".to_string()]);
}

#[test]
fn a_window_job_done_card_names_the_window_and_says_it_cannot_undo() {
    let mut cards = Cards::default();
    cards.apply(&Event::Understood { job_id: j(), name: "housekeeping".into(), text: "Taking your editor".into(), housekeeping: true });
    cards.apply(&Event::Done { job_id: j(), text: "Added the line and saved.".into(), check: None, files: vec![], windows: vec!["Text Editor".into()] });
    let c = cards.list.last().unwrap();
    assert!(matches!(&c.kind, CardKind::Done { windows, .. } if windows == &vec!["Text Editor".to_string()]));
    assert!(c.text.ends_with("What I did inside Text Editor can't be undone by me."), "{}", c.text);
    assert!(c.opens.is_empty());
}

#[test]
fn busy_is_a_line_not_a_card_and_state_rebuilds_a_job() {
    let mut cards = Cards::default();
    assert_eq!(cards.apply(&Event::Busy { job_id: j(), text: "working".into() }), vec![Change::Line("working".into())]);
    assert!(cards.list.is_empty());
    let st = JobState { id: j(), name: "p".into(), housekeeping: false, understood: "Starting p".into(), plan: vec!["a".into(), "b".into()],
        steps: vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }], waiting: Waiting::Answer { questions: vec!["which?".into()], options: vec![] } };
    cards.apply(&Event::State { job: Some(st) });
    assert_eq!(cards.list.len(), 2);
    let CardKind::Building { steps, .. } = &cards.list[0].kind else { panic!() };
    assert!(steps[0].done && !steps[1].done);
    assert!(matches!(&cards.list[1].kind, CardKind::NeedsAnswer { questions, .. } if questions == &vec!["which?".to_string()]));
    assert_eq!(cards.apply(&Event::State { job: None }), vec![], "an empty state changes nothing");
}

#[test]
fn a_reconnect_refills_the_open_job_instead_of_opening_a_second_one() {
    let st = |steps: Vec<StepView>| JobState { id: j(), name: "p".into(), housekeeping: false, understood: "Starting p".into(),
        plan: vec!["a".into(), "b".into()], steps, waiting: Waiting::Answer { questions: vec!["which?".into()], options: vec![] } };
    let mut cards = Cards::default();
    cards.apply(&Event::State { job: Some(st(vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }])) });
    assert_eq!(cards.list.len(), 2, "the Building card and the question");

    // The socket dropped and came back: the same job, one step further on.
    let again = cards.apply(&Event::State { job: Some(st(vec![
        StepView { plan_step: 1, text: "wrote a".into(), ok: true },
        StepView { plan_step: 2, text: "ran b".into(), ok: true }])) });
    assert_eq!(again, vec![Change::Updated(0)], "the open card is refilled, nothing is added");
    assert_eq!(cards.list.len(), 2, "still one Building card and one question");
    assert_eq!(cards.list.iter().filter(|c| matches!(c.kind, CardKind::Building { .. })).count(), 1);
    assert_eq!(cards.list.iter().filter(|c| matches!(c.kind, CardKind::NeedsAnswer { .. })).count(), 1);
    let CardKind::Building { steps, .. } = &cards.list[0].kind else { panic!() };
    assert!(steps[0].done && steps[1].done, "the second step ticked in place");
    assert_eq!(cards.list[0].buttons, vec![Button { label: "Stop".into(), say: "stop".into() }], "still stoppable");
}

#[test]
fn clear_keeps_only_the_running_job_and_what_it_waits_on() {
    let mut cards = Cards::default();
    cards.apply(&Event::You { text: "hi".into() });
    cards.apply(&Event::Said { text: "hello".into() });
    cards.clear();
    assert!(cards.list.is_empty(), "nothing running: everything goes");

    cards.apply(&Event::You { text: "make p".into() });
    cards.apply(&Event::Understood { job_id: j(), name: "p".into(), text: "Starting p".into(), housekeeping: false });
    cards.apply(&Event::NeedsAnswer { job_id: j(), questions: vec!["which one?".into()], options: vec![] });
    cards.apply(&Event::You { text: "the first".into() });
    cards.apply(&Event::Said { text: "noted".into() });
    cards.apply(&Event::NeedsOk { job_id: j(), what: "http post to x".into(), why: "network".into() });
    assert!(cards.running());
    cards.clear();
    assert!(matches!(cards.list[0].kind, CardKind::Building { .. }));
    assert!(matches!(cards.list[1].kind, CardKind::NeedsOk { .. }));
    assert_eq!(cards.list.len(), 2);
    // The job carries on in the cleared list: its steps and its end land on the kept card.
    assert_eq!(cards.apply(&Event::Plan { job_id: j(), steps: vec!["write".into()] }), vec![Change::Updated(0)]);
    cards.apply(&Event::Done { job_id: j(), text: "finished".into(), check: None, files: vec![], windows: vec![] });
    let CardKind::Building { collapsed, .. } = &cards.list[0].kind else { panic!() };
    assert!(collapsed);
    assert!(!cards.running(), "the job ended: no Stop in the title bar");
    cards.apply(&Event::You { text: "hi".into() });
    cards.clear();
    assert!(cards.list.is_empty(), "nothing running: everything goes");
}

#[test]
fn the_spinner_runs_from_your_message_until_the_turn_comes_back() {
    assert_eq!(busy_after(&Event::You { text: "make p".into() }), Some(true));
    assert_eq!(busy_after(&Event::Step { job_id: j(), plan_step: 1, text: "wrote a".into(), ok: true }), Some(true));
    assert_eq!(busy_after(&Event::NeedsOk { job_id: j(), what: "x".into(), why: "y".into() }), Some(false));
    assert_eq!(busy_after(&Event::Said { text: "hi".into() }), Some(false));
    assert_eq!(busy_after(&Event::Error { text: "bad".into() }), Some(false));
    assert_eq!(busy_after(&Event::State { job: None }), None);
    // Done/Failed no longer stop the spinner: a slow local model's learning turn follows, and the
    // Learned that always ends it is what does (Task 8 ruling).
    assert_eq!(busy_after(&Event::Done { job_id: j(), text: "done".into(), check: None, files: vec![], windows: vec![] }), None);
    assert_eq!(busy_after(&Event::Learned { job_id: j(), lines: vec![], pending: false }), Some(false));
}

#[test]
fn learned_lines_join_the_jobs_done_card_and_pending_ones_offer_keep_and_discard() {
    let mut cards = Cards::default();
    cards.apply(&Event::Done { job_id: j(), text: "done".into(), check: None, files: vec![], windows: vec![] });
    let ch = cards.apply(&Event::Learned { job_id: j(), lines: vec!["Learned, if you keep it: get gimp (this computer)".into()], pending: true });
    assert_eq!(ch, vec![Change::Updated(0)]);
    let CardKind::Done { learned, .. } = &cards.list[0].kind else { panic!() };
    assert_eq!(learned, &vec!["Learned, if you keep it: get gimp (this computer)".to_string()]);
    let says: Vec<&str> = cards.list[0].buttons.iter().map(|b| b.say.as_str()).collect();
    assert_eq!(says, vec!["undo", "keep what you learned", "discard what you learned"]);
    // A failed job's "marked as not working" has no Done card to join: it is a line of its own.
    let ch = cards.apply(&Event::Learned { job_id: "other".into(), lines: vec!["Marked as not working: this computer/x".into()], pending: false });
    assert_eq!(ch, vec![Change::Added(1)]);
    assert!(cards.apply(&Event::Skills { notebooks: vec![] }).is_empty());
}

#[test]
fn an_empty_learned_after_a_model_that_could_not_answer_draws_nothing() {
    let mut cards = Cards::default();
    cards.apply(&Event::Done { job_id: j(), text: "done".into(), check: None, files: vec![], windows: vec![] });
    assert!(cards.apply(&Event::Learned { job_id: j(), lines: vec![], pending: false }).is_empty());
    assert_eq!(cards.list.len(), 1, "no Said card either");
}

#[test]
fn clicked_answers_are_sent_once_every_question_has_one() {
    use aios_rail::cards::answer;
    let one = vec!["Which language?".to_string()];
    assert_eq!(answer(&one, &[Some("Python".into())]), ("Python".into(), true), "one question: the click is the answer");
    assert_eq!(answer(&one, &[None]), (String::new(), false));
    let two = vec!["Which language?".to_string(), "Its name?".to_string()];
    assert_eq!(answer(&two, &[Some("Rust".into()), None]), ("Which language? Rust.".into(), false), "the name is still to type");
    assert_eq!(answer(&two, &[Some("Rust".into()), Some("primes".into())]), ("Which language? Rust. Its name? primes.".into(), true));
}
