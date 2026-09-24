//! The learning turn, checked against the job record (Phase 3 §4–§5). The model proposes; the
//! record decides: a technique's steps are the steps that ran, never the model's account of them.
use crate::job::{Job, StepRecord};
use crate::moves::LearnEntry;
use crate::notes::{self, Note, THIS_COMPUTER};
use crate::store::StoreError;
use executor::action::Action;
use rusqlite::Connection;
use std::path::{Component, Path};

#[derive(Debug, Default, PartialEq)]
pub struct Learned { pub lines: Vec<String>, pub pending: bool }

/// Whether a step changed the machine outside the job's own folder (Phase 3 §5). A `..` anywhere
/// counts as outside: `starts_with` compares components and never resolves one.
fn changes_machine(a: &Action, folder: &str) -> bool {
    let outside = |p: &str| { let p = Path::new(p); p.components().any(|c| c == Component::ParentDir) || (p.is_absolute() && !p.starts_with(folder)) };
    match a {
        Action::SetSetting { .. } => true,
        // Root is how software and services change (full-access spec).
        Action::RunCommand { argv } => argv.first().is_some_and(|p| p == "sudo"),
        Action::WriteFile { path, .. } | Action::EditFile { path, .. } | Action::AppendFile { path, .. } => outside(path),
        _ => false,
    }
}

fn step(job: &Job, n: usize) -> Option<&StepRecord> { n.checked_sub(1).and_then(|i| job.steps.get(i)) }

fn steps_text(job: &Job, ns: &[usize]) -> String {
    ns.iter().filter_map(|&n| step(job, n)).map(|s| crate::prompt::compact_action(&s.action).chars().take(150).collect::<String>()).collect::<Vec<_>>().join("; ")
}

pub fn apply(c: &Connection, job: &Job, entries: Vec<LearnEntry>, used: &[String], wrong: &[String], remove: &[String], passed: bool) -> Result<Learned, StoreError> {
    let mut out = Learned::default();
    let shown = |k: &String| { let (nb, tp) = k.split_once('/').unwrap_or(("", k)); job.shown_notes.contains(&notes::key(nb, tp)) };
    for k in used.iter().filter(|k| shown(k)) { notes::mark_used(c, k)?; }
    for k in wrong.iter().filter(|k| shown(k)) {
        notes::mark_failed(c, k)?;
        out.lines.push(format!("Marked as not working: {k}"));
    }
    // A job that did not pass proved nothing new.
    if !passed { return Ok(out); }
    for k in remove.iter().filter(|k| shown(k)) {
        let (nb, tp) = k.split_once('/').unwrap_or(("", k));
        if notes::remove(c, nb, tp)? { out.lines.push(format!("Forgot: {k}")); }
    }
    let heard_the_user = !job.request.is_empty() || !job.answers.is_empty();
    let computer_topics: Vec<String> = entries.iter().filter(|e| notes::norm_notebook(&e.notebook) == THIS_COMPUTER).map(|e| notes::norm(&e.topic)).collect();
    for e in entries {
        let (nb, tp) = (notes::norm_notebook(&e.notebook), notes::norm(&e.topic));
        if nb.is_empty() || tp.is_empty() || e.text.trim().is_empty() { continue; }
        let cited: Vec<&StepRecord> = e.steps.iter().filter_map(|&n| step(job, n)).collect();
        if cited.len() != e.steps.len() { continue; } // a step that does not exist
        let proof: Vec<usize> = match e.kind.as_str() {
            "technique" if !cited.is_empty() && cited.iter().all(|s| s.ok) => e.steps.clone(),
            "pitfall" => {
                let first_fail = e.steps.iter().copied().filter(|&n| !step(job, n).unwrap().ok).min();
                let fixes: Vec<usize> = e.steps.iter().copied().filter(|&n| step(job, n).unwrap().ok && Some(n) > first_fail).collect();
                if first_fail.is_none() || fixes.is_empty() { continue; }
                fixes
            }
            "taste" if e.steps.is_empty() && heard_the_user => vec![],
            _ => continue,
        };
        let links = if nb == THIS_COMPUTER { vec![] } else {
            let mut keep = vec![];
            for l in e.links.iter().map(|l| notes::norm(l)) {
                if computer_topics.contains(&l) || notes::get(c, THIS_COMPUTER, &l)?.is_some() { keep.push(l); }
            }
            keep
        };
        let pending = proof.iter().filter_map(|&n| step(job, n)).any(|s| changes_machine(&s.action, &job.folder));
        let note = Note { notebook: nb.clone(), topic: tp.clone(), kind: e.kind.clone(), text: e.text.trim().to_string(), steps: steps_text(job, &proof), links, ..Default::default() };
        notes::put(c, &note, pending.then_some(job.id.as_str()))?;
        out.pending |= pending;
        // What waits for Keep is shown whole: the owner keeps what he read, not a topic name.
        out.lines.push(if pending {
            format!("Learned, if you keep it: {tp} ({nb}) — {} (did: {})", notes::cut(&note.text, notes::MAX_TEXT), notes::cut(&note.steps, notes::MAX_STEPS))
        } else { format!("Learned: {tp} ({nb})") });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::notes::{init, list};

    fn db() -> Connection { let c = Connection::open_in_memory().unwrap(); init(&c).unwrap(); c }
    fn rec(a: Action, ok: bool) -> StepRecord { StepRecord { plan_step: 1, action: a, ok, detail: String::new() } }
    fn open(url: &str) -> Action { Action::OpenApp { name: format!("firefox {url}"), visible: true } }
    fn job(steps: Vec<StepRecord>) -> Job {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        j.request = "open the browser and go to example.org".into();
        j.steps = steps;
        j
    }
    fn entry(nb: &str, topic: &str, kind: &str, steps: Vec<usize>) -> LearnEntry {
        LearnEntry { notebook: nb.into(), topic: topic.into(), kind: kind.into(), text: "the way".into(), steps, links: vec![] }
    }

    #[test]
    fn a_technique_stores_the_steps_from_the_record_not_the_model() {
        let c = db();
        let j = job(vec![rec(Action::ScreenLook { cell: None }, true), rec(open("example.org"), true)]);
        let l = apply(&c, &j, vec![entry("This computer", "Open a website", "technique", vec![2])], &[], &[], &[], true).unwrap();
        assert_eq!(l.lines, vec!["Learned: open a website (this computer)".to_string()]);
        let n = &list(&c, THIS_COMPUTER).unwrap()[0];
        assert!(n.steps.contains("open_app") && n.steps.contains("example.org"), "{}", n.steps);
    }

    #[test]
    fn a_technique_citing_a_failed_or_missing_step_is_dropped() {
        let c = db();
        let j = job(vec![rec(open("a"), false), rec(open("b"), true)]);
        let l = apply(&c, &j, vec![entry("this computer", "x", "technique", vec![1]), entry("this computer", "y", "technique", vec![9]), entry("this computer", "z", "technique", vec![])], &[], &[], &[], true).unwrap();
        assert!(l.lines.is_empty(), "{:?}", l.lines);
        assert!(list(&c, THIS_COMPUTER).unwrap().is_empty());
    }

    #[test]
    fn a_pitfall_needs_a_failure_and_a_later_fix() {
        let c = db();
        let j = job(vec![rec(open("a"), false), rec(open("b"), true)]);
        apply(&c, &j, vec![entry("this computer", "fix", "pitfall", vec![1, 2]), entry("this computer", "nofix", "pitfall", vec![2])], &[], &[], &[], true).unwrap();
        let l = list(&c, THIS_COMPUTER).unwrap();
        assert_eq!(l.len(), 1);
        assert!(l[0].steps.contains("b") && !l[0].steps.contains("firefox a"), "only the fix is kept as proof: {}", l[0].steps);
    }

    #[test]
    fn taste_needs_the_users_words_and_no_steps() {
        let c = db();
        let mut j = job(vec![]);
        apply(&c, &j, vec![entry("web design", "colours", "taste", vec![])], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "web design").unwrap().len(), 1);
        j.request.clear();
        apply(&c, &j, vec![entry("web design", "fonts", "taste", vec![])], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "web design").unwrap().len(), 1, "no words from the user, no taste");
    }

    #[test]
    fn used_wrong_and_remove_touch_only_what_was_shown() {
        let c = db();
        for t in ["shown", "hidden"] { notes::put(&c, &Note { notebook: THIS_COMPUTER.into(), topic: t.into(), kind: "technique".into(), text: "x".into(), ..Default::default() }, None).unwrap(); }
        let mut j = job(vec![]);
        j.shown_notes = vec!["this computer/shown".into()];
        apply(&c, &j, vec![], &["this computer/shown".into(), "this computer/hidden".into()], &["this computer/hidden".into()], &[], true).unwrap();
        let l = list(&c, THIS_COMPUTER).unwrap();
        let get = |t: &str| l.iter().find(|n| n.topic == t).unwrap().clone();
        assert_eq!((get("shown").uses, get("hidden").uses, get("hidden").failed), (1, 0, false));
        apply(&c, &j, vec![], &[], &[], &["this computer/hidden".into(), "this computer/shown".into()], true).unwrap();
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().iter().map(|n| n.topic.clone()).collect::<Vec<_>>(), vec!["hidden"]);
    }

    #[test]
    fn a_failed_job_only_marks_and_counts() {
        let c = db();
        notes::put(&c, &Note { notebook: THIS_COMPUTER.into(), topic: "t".into(), kind: "technique".into(), text: "x".into(), ..Default::default() }, None).unwrap();
        let mut j = job(vec![rec(open("a"), true)]);
        j.shown_notes = vec!["this computer/t".into()];
        let l = apply(&c, &j, vec![entry("this computer", "new", "technique", vec![1])], &[], &["this computer/t".into()], &["this computer/t".into()], false).unwrap();
        assert_eq!(l.lines, vec!["Marked as not working: this computer/t".to_string()]);
        let all = list(&c, THIS_COMPUTER).unwrap();
        assert_eq!(all.len(), 1, "nothing new, nothing removed");
        assert!(all[0].failed);
    }

    #[test]
    fn a_step_that_changes_the_machine_waits_for_keep() {
        let c = db();
        let write = |p: &str| rec(Action::WriteFile { path: p.into(), contents: "y".into() }, true);
        let j = job(vec![rec(Action::RunCommand { argv: vec!["sudo".into(), "apt-get".into(), "install".into(), "-y".into(), "gimp".into()] }, true), write("/data/housekeeping/x"), write("/data/housekeeping/../../etc/x"), write("../x")]);
        let l = apply(&c, &j, vec![entry("this computer", "get gimp", "technique", vec![1]), entry("this computer", "scratch note", "technique", vec![2]),
            entry("this computer", "climb out", "technique", vec![3]), entry("this computer", "step up", "technique", vec![4])], &[], &[], &[], true).unwrap();
        assert!(l.pending);
        assert_eq!(l.lines[..2], [format!("Learned, if you keep it: get gimp (this computer) — the way (did: {})", crate::prompt::compact_action(&j.steps[0].action)), "Learned: scratch note (this computer)".to_string()]);
        assert!(l.lines[2].starts_with("Learned, if you keep it: climb out") && l.lines[3].starts_with("Learned, if you keep it: step up"), "a `..` leaves the folder: {:?}", l.lines);
        assert_eq!(list(&c, THIS_COMPUTER).unwrap().len(), 1, "the machine changes wait unseen");
    }

    #[test]
    fn a_skill_entry_keeps_only_links_that_exist() {
        let c = db();
        let j = job(vec![rec(open("blender"), true)]);
        let mut e = entry("blender", "make a round object", "technique", vec![1]);
        e.links = vec!["start blender".into(), "nothing like this".into()];
        apply(&c, &j, vec![entry("this computer", "start blender", "technique", vec![1]), e], &[], &[], &[], true).unwrap();
        assert_eq!(list(&c, "blender").unwrap()[0].links, vec!["start blender".to_string()]);
    }
}
