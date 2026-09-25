//! Helpers (helpers design): pieces of work the main AI hands out with `delegate`, each done at the
//! same time by a role helper on a cloud model of its own, with the machine hand only. A helper
//! whose model fails moves to the next one; the owner never talks to a helper.
use crate::cloud::Account;
use crate::model::{Model, ModelError, Msg, Prompt, RemoteModel};
use crate::moves::{Ending, Move};
use executor::action::HelperTask;
use executor::executor::{lane, Lane};
use executor::worker::{MachineWorker, Outcome, Worker};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Sender};

pub const ROLES: [&str; 6] = aios_proto::HELPER_ROLES;
/// Moves one helper may make before it hands back what it has.
const MAX_MOVES: usize = 40;
/// Models one helper tries before it gives up.
const MAX_MODELS: usize = 8;

fn role_text(role: &str) -> &'static str {
    match role {
        "coding" => "You write and change code: small working pieces, run them, and fix what fails before you reply.",
        "design" => "You design: layout, look, structure and how it is used. Make concrete choices and write them down as files the others can build from.",
        "reasoning" => "You think a problem through: compare the options, check the facts with commands and the web, and reply with a clear conclusion and why.",
        "review" => "You review: read the work your task names, run it where you can, and reply with what is wrong, what is risky and what to change, most important first.",
        "debugging" => "You find why something fails: reproduce it, read the errors, test one cause at a time, fix the root cause, and prove the fix by running it.",
        _ => "You create art: images, sound, words or 3D, made with the programs and scripts of this computer (ImageMagick, Python, blender --background …). Save what you make as files.",
    }
}

/// A helper's standing instructions.
pub fn brief(role: &str, folder: &Path) -> String {
    format!("You are a {role} helper of the AI of this computer. The main AI handed you one piece of a bigger task: do it fully, then reply with what you did, where the results are, and what the main AI must know. Nobody answers questions here: decide yourself.
{}
You have full access through your tools: commands (as the user, with sudo for root), files, the web and background programs. There is no desktop or screen for you. When no tool does what you need, write a script or install a program and run it.
Work in {} unless your task names other places. One tool call per answer; its result comes back to you. Look before you change something and check after. A file over about 150 lines goes in parts: write_file the first, then append_file the rest.
When done, reply; when you cannot do it, reply with outcome could_not and why.", role_text(role), folder.display())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Report { pub role: String, pub model: String, pub done: bool, pub text: String }

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

/// Every helper at once, one thread each. `models` starts with one model per account (`starts`
/// of them) and goes on with the other models those keys open. Each helper takes them in
/// `aios_proto::rank`'s order — the owner's pick for its role, models made for the role, then the
/// rest from account *i* on, so the helpers spread over the providers — and walks on when a model
/// fails. `dir` holds the owner's picks and the speeds, and gets each answer's time. `progress`
/// hears each helper's steps as they happen, on the calling thread.
pub fn run(tasks: &[HelperTask], models: &[Account], starts: usize, dir: Option<&Path>, folder: &Path, stop: &AtomicBool, progress: &mut dyn FnMut(String)) -> Vec<Report> {
    let read = |f: &str| dir.and_then(|d| std::fs::read_to_string(d.join(f)).ok()).unwrap_or_default();
    let (speeds, picks) = (aios_proto::speeds(&read(crate::cloud::SPEEDS)), aios_proto::role_picks(&read(crate::cloud::PICKS)));
    let pairs: Vec<(&str, &str)> = models.iter().map(|a| (a.url.as_str(), a.model.as_str())).collect();
    let (tx, rx) = channel::<String>();
    std::thread::scope(|s| {
        let handles: Vec<_> = tasks.iter().enumerate().map(|(i, t)| {
            let tx = tx.clone();
            let mut order = aios_proto::rank(&t.role, &pairs, i, starts, &speeds, &picks);
            order.truncate(MAX_MODELS);
            s.spawn(move || one(t, models, order, dir, folder, stop, &tx))
        }).collect();
        drop(tx);
        for line in rx { progress(line) }
        handles.into_iter().zip(tasks).map(|(h, t)| h.join().unwrap_or_else(|_| Report { role: t.role.clone(), model: String::new(), done: false, text: "the helper crashed".into() })).collect()
    })
}

fn one(t: &HelperTask, models: &[Account], order: Vec<usize>, dir: Option<&Path>, folder: &Path, stop: &AtomicBool, tx: &Sender<String>) -> Report {
    let report = |model: &str, done: bool, text: String| Report { role: t.role.clone(), model: model.to_string(), done, text };
    let hand = MachineWorker { workspace: folder.to_path_buf() };
    let system = brief(&t.role, folder);
    let (mut history, mut user, mut at, mut last_err) = (Vec::<Msg>::new(), t.task.clone(), 0, String::new());
    for _ in 0..MAX_MOVES {
        if stop.load(Ordering::SeqCst) { return report("", false, "stopped".into()) }
        let Some(a) = order.get(at).map(|&k| &models[k]) else {
            return report("", false, format!("no cloud model would do it; the last said: {last_err}"))
        };
        let m = RemoteModel { key: Some(a.key.clone()), tools: true, ..RemoteModel::at(a.kind, &a.url, &a.model) };
        // ponytail: the whole helper history every call; a long helper run costs tokens, and a
        // cloud window holds 40 moves.
        let p = Prompt { system: system.clone(), user: user.clone(), history: history.clone(), allowed: vec!["reply", "act"], no_screen: true, machine_only: true, ..Default::default() };
        let began = std::time::Instant::now();
        let mv = match m.next_move_watched(&p, &mut |_| !stop.load(Ordering::SeqCst)) {
            Ok(mv) => { if let Some(d) = dir { crate::cloud::note_speed(d, &a.url, &a.model, began.elapsed()) } mv }
            Err(ModelError::Stopped) => return report(&a.model, false, "stopped".into()),
            Err(e) => {
                last_err = cut(&e.to_string(), 200);
                at += 1;
                let next = order.get(at).map(|&k| models[k].model.as_str()).unwrap_or("nothing left");
                let _ = tx.send(format!("{} · {}: failed ({}), switching to {next}", t.role, a.model, cut(&e.to_string(), 80)));
                continue
            }
        };
        let said = serde_json::to_string(&mv).unwrap_or_default();
        history.push(Msg { role: "user".into(), content: std::mem::take(&mut user) });
        history.push(Msg { role: "assistant".into(), content: said });
        match mv {
            Move::Reply { text, outcome, .. } => return report(&a.model, outcome != Ending::CouldNot, text),
            Move::Act { action, .. } => {
                let o = if lane(&action) == Lane::Machine { hand.run(&action) }
                    else { Outcome::err("helpers have commands, files, the web and background programs; the desktop and the screen are the main AI's") };
                let _ = tx.send(format!("{} · {}: {}", t.role, a.model, crate::event::describe(&action)));
                user = format!("{} -> {}: {}", crate::prompt::compact_action(&action), if o.ok { "ok" } else { "failed" }, cut(&o.detail, 4000));
            }
            _ => user = "work with a tool, or reply with your result".into(),
        }
    }
    report(order.get(at).map(|&k| models[k].model.as_str()).unwrap_or(""), false, format!("ran out of moves ({MAX_MOVES}) before finishing"))
}

/// What the main AI reads back: each helper's role, model, whether it finished, and its reply.
pub fn summary(reports: &[Report]) -> String {
    reports.iter().map(|r| format!("- {} helper ({}): {}. {}", r.role, if r.model.is_empty() { "no model" } else { &r.model },
        if r.done { "finished" } else { "did not finish" }, cut(&r.text, 1500))).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::find::Kind;
    use crate::testing::{json_response, serve_each, temp_root};

    fn says(mv: serde_json::Value) -> String {
        json_response("200 OK", &serde_json::json!({ "choices": [{ "message": { "content": mv.to_string() } }] }).to_string())
    }
    fn account(at: std::net::SocketAddr, model: &str) -> Account {
        Account { name: "P".into(), kind: Kind::OpenAi, url: format!("http://{at}"), model: model.into(), key: "k".into() }
    }
    fn task(role: &str, task: &str) -> HelperTask { HelperTask { role: role.into(), task: task.into() } }

    #[test]
    fn helpers_work_at_once_each_on_its_own_provider() {
        let d = temp_root("helpers-two");
        let a = serve_each(vec![
            says(serde_json::json!({ "move": "act", "thought": "t", "action": { "kind": "write_file", "path": "a.txt", "contents": "hi" } })),
            says(serde_json::json!({ "move": "reply", "text": "wrote a.txt" })),
        ]);
        let b = serve_each(vec![says(serde_json::json!({ "move": "reply", "text": "looks fine" }))]);
        let (mut lines, stop) = (vec![], AtomicBool::new(false));
        let r = run(&[task("coding", "write a.txt"), task("review", "check it")], &[account(a, "mA"), account(b, "mB")], 2, Some(&d), &d, &stop, &mut |l| lines.push(l));
        assert_eq!(r[0], Report { role: "coding".into(), model: "mA".into(), done: true, text: "wrote a.txt".into() });
        assert_eq!((r[1].model.as_str(), r[1].text.as_str()), ("mB", "looks fine"));
        assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "hi");
        assert!(lines.iter().any(|l| l.starts_with("coding · mA: ")), "{lines:?}");
        assert!(summary(&r).contains("- review helper (mB): finished. looks fine"));
        let speeds = std::fs::read_to_string(d.join(crate::cloud::SPEEDS)).unwrap();
        assert!(speeds.contains("\tmA\t") && speeds.contains("\tmB\t"), "each answer's time is kept: {speeds}");
    }

    #[test]
    fn the_owners_pick_for_a_role_goes_first() {
        let d = temp_root("helpers-pick");
        let a = serve_each(vec![]);
        let b = serve_each(vec![says(serde_json::json!({ "move": "reply", "text": "done on B" }))]);
        std::fs::write(d.join(crate::cloud::PICKS), format!("coding\thttp://{b}\tmB\n")).unwrap();
        let stop = AtomicBool::new(false);
        let r = run(&[task("coding", "code")], &[account(a, "mA"), account(b, "mB")], 2, Some(&d), &d, &stop, &mut |_| {});
        assert_eq!((r[0].model.as_str(), r[0].done), ("mB", true));
    }

    #[test]
    fn a_helper_whose_model_fails_moves_to_the_next() {
        let d = temp_root("helpers-switch");
        let a = serve_each(vec![json_response("400 Bad Request", r#"{"error":{"message":"model refused"}}"#)]);
        let b = serve_each(vec![says(serde_json::json!({ "move": "reply", "text": "done on B" }))]);
        let (mut lines, stop) = (vec![], AtomicBool::new(false));
        let r = run(&[task("reasoning", "think")], &[account(a, "mA"), account(b, "mB")], 2, Some(&d), &d, &stop, &mut |l| lines.push(l));
        assert_eq!((r[0].model.as_str(), r[0].done), ("mB", true));
        assert!(lines.iter().any(|l| l.contains("mA: failed") && l.contains("switching to mB")), "{lines:?}");
    }

    #[test]
    fn a_helper_has_no_desktop() {
        let d = temp_root("helpers-desktop");
        let a = serve_each(vec![
            says(serde_json::json!({ "move": "act", "thought": "t", "action": { "kind": "look" } })),
            says(serde_json::json!({ "move": "reply", "text": "ok", "outcome": "could_not" })),
        ]);
        let stop = AtomicBool::new(false);
        let r = run(&[task("design", "look at it")], &[account(a, "mA")], 1, None, &d, &stop, &mut |_| {});
        assert!(!r[0].done && r[0].text == "ok");
    }
}
