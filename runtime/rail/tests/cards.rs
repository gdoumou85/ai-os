use aios_proto::*;
use aios_rail::cards::{Button, CardKind, Cards, Change};

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
    cards.apply(&Event::Done { job_id: j(), text: "finished".into(), check: Some("ran python3 a.py: ok".into()), files: files.clone() });
    let CardKind::Building { collapsed, .. } = &cards.list[b].kind else { panic!() };
    assert!(collapsed);
    let done = cards.list.last().unwrap();
    let CardKind::Done { text, check, files: f } = &done.kind else { panic!() };
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
fn busy_is_a_line_not_a_card_and_state_rebuilds_a_job() {
    let mut cards = Cards::default();
    assert_eq!(cards.apply(&Event::Busy { job_id: j(), text: "working".into() }), vec![Change::Line("working".into())]);
    assert!(cards.list.is_empty());
    let st = JobState { id: j(), name: "p".into(), housekeeping: false, understood: "Starting p".into(), plan: vec!["a".into(), "b".into()],
        steps: vec![StepView { plan_step: 1, text: "wrote a".into(), ok: true }], waiting: Waiting::Answer { questions: vec!["which?".into()] } };
    cards.apply(&Event::State { job: Some(st) });
    assert_eq!(cards.list.len(), 2);
    let CardKind::Building { steps, .. } = &cards.list[0].kind else { panic!() };
    assert!(steps[0].done && !steps[1].done);
    assert!(matches!(&cards.list[1].kind, CardKind::NeedsAnswer { questions } if questions == &vec!["which?".to_string()]));
    assert_eq!(cards.apply(&Event::State { job: None }), vec![], "an empty state changes nothing");
}
