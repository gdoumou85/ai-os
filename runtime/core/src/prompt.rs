use crate::job::Job;
use crate::model::{Msg, Prompt};
use crate::moves::TodoItem;
use crate::store::ProjectRow;
use executor::action::Action;

/// The learning turn's own brief: it only ever answers with `learn`.
const LEARN_SYSTEM: &str = "You are the AI that uses this computer for its user. A piece of work has just ended; you note down what is worth knowing next time. Answer with one JSON object: the learn move.";

/// The standing brief (one-loop design §1): who it is, its moves, its hands, a few habits. Under
/// 1800 tokens (a test holds it), since at 8k every token here is one the chat does not get.
pub fn brief(sees: bool) -> String {
    let screen = if sees {
        "- screen_look (cell): the screen under numbered squares 1-48; with a cell, that square enlarged under spots 1-16. screen_click cell spot name (double): click a spot in the square you just enlarged. screen_type text (enter): types into what has the focus. scroll cell spot direction amount, and drag from_cell from_spot to_cell to_spot, aim the same way after a look at the whole screen. The screen is for what look cannot reach (a web page in a browser, a program that lists no controls): look, enlarge, click, look again."
    } else {
        "- This model cannot see pictures, so the screen is closed to you: work programs through look, press, type, key and the command line, and read the web with web_search and web_read."
    };
    format!("You are the AI of this computer. You use it for its user the way a skilled person at the keyboard would, for any task they give you. You have full access: you work as the user, with sudo for what needs root. Nobody approves your actions, so take care with anything that deletes or overwrites.

Answer with one JSON move at a time:
- reply: say something to the user and end your turn. After work, say plainly what you did and how it came out; outcome could_not when you did not get it done, and why.
- ask: one question with up to 5 suggested answers, only when the user's words leave open something you cannot find out yourself. It ends your turn; the answer is their next message.
- todo: your to-do list for work of several steps; send it again, with items done, as you go.
- act: one action (below). Its result comes back to you; then choose your next move.
- remember: an instruction to keep for every later conversation, only when the user says something is to hold from now on.
Every move starts with thought: one short sentence on what you are doing and why.

How to work:
- Do what the user asked. A plain order (\"delete X\", \"install Y\") needs no questions.
- Find out rather than guess: ls, cat, --help, web_search. Look before you change something and check after: prove it worked before you say it did.
- A project is a folder with a BLUEPRINT.md: what it is, how it is built and run, where it stands. Read it before working on a project and bring it up to date when you change the project. New projects go in the projects folder named under Where things are, unless the user says where.
- When an action fails, read why and do something different: the same action again fails again.
- Never install what This machine already lists.

Actions (act):
- run_command argv: runs a program in the user's home and waits for it to end (up to 30 min). There is no shell: for pipes, >, &&, *, ~ or $VAR send argv [\"bash\",\"-c\",\"<the whole line>\"]. Use absolute paths. Install with sudo apt-get install -y (else snap, else flatpak); language packages with pip in a .venv, npm or cargo.
- start_program name argv: runs something that keeps going (a server, a download, a long build) in the background. program_output name shows its latest output; stop_program name ends it. wait seconds (up to 300) gives something time to happen.
- read_file path (from_line, lines), write_file path contents, edit_file path find replace: files. edit_file swaps one exact passage you have read.
- web_search query: the top results and their addresses. web_read url (from_line): a web page as text.
- open_app name: opens a program by its desktop name (like org.gnome.TextEditor); visible true when the user is to see it.
- look (window, find): with no window lists the open windows; with a window lists its controls with ids. press control name, type control text (replace), read control: work those controls. Ids come from the latest look: look again after acting. A program's commands (save, print) may sit in its menu: press the menu, look, press the command.
- key keys: a key or combination on the screen, like ctrl+s, alt+tab, escape, down, f5.
{screen}
- set_setting projects_root value: moves where new projects go, only when the user asks.")
}

/// What the model is told about now, rebuilt for every call (one-loop design §1). It goes last,
/// with the newest message, so the pinned request and to-do list are never trimmed away.
pub struct Context<'a> {
    pub machine: &'a str,
    pub places: &'a str,
    pub programs: &'a [String],
    pub instructions: &'a [String],
    pub journal: &'a str,
    pub notes: &'a str,
    pub tips: &'a str,
    pub request: &'a str,
    pub todo: &'a [String],
}

pub fn context_block(c: &Context) -> String {
    let mut s = format!("{}\n{}\n", c.machine.trim_end(), c.places);
    if !c.programs.is_empty() { s.push_str(&format!("Running in the background: {}.\n", c.programs.join(", "))); }
    if !c.instructions.is_empty() {
        s.push_str("Standing instructions from the user:\n");
        for i in c.instructions { s.push_str(&format!("- {i}\n")); }
    }
    // Tips are a help: at their fullest (3000 chars) they would crowd out the chat, so half.
    let tips: String = c.tips.chars().take(1500).collect();
    for part in [c.journal, c.notes, tips.as_str()] { if !part.trim().is_empty() { s.push_str(part.trim_end()); s.push('\n'); } }
    let request: String = c.request.chars().take(1500).collect();
    s.push_str(&format!("The request you are working on: {request}\n"));
    if !c.todo.is_empty() { s.push_str(&format!("Your to-do list: {}\n", c.todo.join(" / "))); }
    s
}

/// The to-do list as the card and the context block show it.
pub fn todo_lines(items: &[TodoItem]) -> Vec<String> {
    items.iter().map(|i| format!("[{}] {}", if i.done { "x" } else { " " }, i.text.trim())).collect()
}

/// The chat as the model gets it (one-loop design §2): stored rows, newest kept first, results
/// older than the newest six cut to 300 chars, the oldest dropped once `budget` tokens (chars/4)
/// are spent. `result` rows are user-side, marked, so the model tells them from the user.
pub fn fit(rows: &[(String, String)], budget: usize) -> Vec<Msg> {
    let mut out = vec![];
    let (mut used, mut results) = (0usize, 0usize);
    for (role, text) in rows.iter().rev() {
        let (role, content) = match role.as_str() {
            "result" => {
                results += 1;
                let t = if results > 6 && text.chars().count() > 300 { format!("{}… (cut)", text.chars().take(300).collect::<String>()) } else { text.clone() };
                ("user", format!("[result] {t}"))
            }
            "assistant" => ("assistant", text.clone()),
            _ => ("user", text.clone()),
        };
        let cost = content.len() / 4 + 4;
        if used + cost > budget { break; }
        used += cost;
        out.push(Msg { role: role.into(), content });
    }
    out.reverse();
    out
}

/// A project the message names: its whole name, or one word of it (4+ letters) — "the car rental
/// site" names car-rental-broker. The front door listed every project, and a 9B greeted with "Hi"
/// picked the only one and started work on it, whatever the prompt said (the owner, 2026-09-23).
pub(crate) fn named_in(p: &ProjectRow, message: &str) -> bool {
    let m = message.to_lowercase();
    let name = p.name.to_lowercase();
    m.contains(&name) || name.split(|c: char| !c.is_alphanumeric()).any(|w| w.len() >= 4 && m.contains(w))
}

/// I4: the full action JSON for the last 6 steps used to go straight into the prompt — one
/// 300-line `write_file` blew the 8k budget and Ollama silently truncates from the head (the
/// system rules live there). Render the two content-carrying actions compactly instead of
/// dumping their payload; everything else keeps its JSON but capped, so an unexpectedly large
/// argv or url still can't blow the budget either.
pub(crate) fn compact_action(action: &Action) -> String {
    match action {
        Action::WriteFile { path, contents } => format!("write_file {path} ({} bytes)", contents.len()),
        Action::EditFile { path, find, .. } => {
            let f: String = find.chars().take(60).collect();
            format!("edit_file {path} (find: {f})")
        }
        Action::Type { control, text, .. } => format!("type into {control} ({} chars)", text.chars().count()),
        other => {
            let full = serde_json::to_string(other).unwrap_or_default();
            if full.chars().count() > 400 {
                format!("{}…", full.chars().take(400).collect::<String>())
            } else {
                full
            }
        }
    }
}

/// Every step of the job, numbered as the learning turn cites them: the last 80 in full, compact.
fn learn_steps(job: &Job) -> String {
    let skip = job.steps.len().saturating_sub(80);
    let mut out = if skip > 0 { format!("(steps 1-{skip} not shown)\n") } else { String::new() };
    for (i, s) in job.steps.iter().enumerate().skip(skip) {
        let a: String = compact_action(&s.action).chars().take(150).collect();
        out.push_str(&format!("{}: {a} -> {}\n", i + 1, if s.ok { "ok" } else { "failed" }));
    }
    if out.is_empty() { "(no steps)".into() } else { out }
}

/// The one extra turn after a job (Phase 3 §4): what to keep, which tips helped, which were wrong.
pub fn learning_turn(job: &Job, passed: bool) -> Prompt {
    let tips = if job.notes_block.is_empty() { "(no tips were shown)".to_string() } else { job.notes_block.clone() };
    let ask = if passed {
        "What did you learn that you would want to know straight away next time? Write at most 5 entries; nothing new or nothing hard means entries []. \
         notebook: \"this computer\" for how this computer and its programs are used (opening a program, finding things, where things are); a craft name (python coding, web design, blender…) for doing that craft better. \
         topic: a short general name for what it does (\"open a website\", never the site's own name). \
         kind: technique (steps = the ok steps that did it), pitfall (steps = the failed step and the ok step that fixed it), taste (what the user liked or rejected; steps []). \
         text: one general line, naming in words what changes between uses (\"the address\"). links: \"this computer\" topics a craft entry depends on."
    } else {
        "The job did not finish, so nothing new is learned: entries []."
    };
    let user = format!(
        "The job {}: {}\nGoal: {}\nThe user asked (verbatim): {}\n\nSteps (number: action -> result):\n{}\n{}\n\n{ask}\n\
         used: the tips above you followed (as notebook/topic). wrong: tips above that did not work. remove: tips above that are useless (only after a job that finished).",
        if passed { "passed its check" } else { "did not finish" },
        if job.housekeeping { "housekeeping" } else { &job.project }, job.goal, job.request, learn_steps(job), tips,
    );
    Prompt { system: LEARN_SYSTEM.into(), user, allowed: vec!["learn"], image: None, ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, StepRecord};
    use executor::action::Action;

    #[test]
    fn the_learning_turn_numbers_every_step_and_asks_only_for_learn() {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        j.request = "open example.org".into();
        j.steps = vec![
            StepRecord { plan_step: 1, action: Action::ScreenLook { cell: None }, ok: true, detail: "d".into() },
            StepRecord { plan_step: 1, action: Action::OpenApp { name: "firefox".into(), visible: true }, ok: false, detail: "d".into() },
        ];
        let p = learning_turn(&j, true);
        assert_eq!(p.allowed, vec!["learn"]);
        assert!(p.user.contains("1: ") && p.user.contains("-> ok") && p.user.contains("2: ") && p.user.contains("-> failed"), "{}", p.user);
        assert!(p.user.contains("open example.org"));
        assert!(p.user.contains("\"this computer\""), "{}", p.user);
        let failed = learning_turn(&j, false);
        assert!(failed.user.contains("did not finish") && !failed.user.contains("Write at most 5 entries"), "{}", failed.user);
    }

    #[test]
    fn the_brief_fits_and_says_what_the_model_can_see() {
        assert!(brief(true).len() / 4 < 1800, "about {} tokens", brief(true).len() / 4);
        assert!(brief(true).contains("screen_look"));
        let blind = brief(false);
        assert!(blind.contains("cannot see pictures") && !blind.contains("screen_click"));
    }

    #[test]
    fn the_fullest_context_block_fits() {
        let apps: Vec<crate::machine::App> = (0..80).map(|i| crate::machine::App { id: format!("org.example.Application{i}"), name: format!("Application {i}") }).collect();
        let tools: Vec<String> = (0..16).map(|i| format!("tool{i}")).collect();
        let mine: Vec<(String, String)> = (0..10).map(|i| (format!("package-{i}"), "apt".into())).collect();
        let machine = crate::machine::block(&"s".repeat(150), &apps, &tools, &mine);
        let places = format!("Where things are: the user's home is /home/{0}; projects live in /data/{0}; projects so far: {1}; the scratch folder is /data/housekeeping.",
            "u".repeat(20), (0..10).map(|i| format!("project-name-{i} (/data/projects/project-name-{i})")).collect::<Vec<_>>().join(", "));
        let instructions: Vec<String> = (0..5).map(|_| "i".repeat(200)).collect();
        let journal = format!("What earlier jobs did:\n{}", (0..5).map(|_| format!("- {}", "j".repeat(400))).collect::<Vec<_>>().join("\n"));
        let notes = "n".repeat(1500 + 100);
        let tips = "t".repeat(3000);
        let todo: Vec<String> = (0..12).map(|i| format!("[ ] {i} {}", "d".repeat(80))).collect();
        let programs = vec!["devserver".to_string(), "download".to_string()];
        let c = Context { machine: &machine, places: &places, programs: &programs, instructions: &instructions, journal: &journal, notes: &notes, tips: &tips, request: &"r".repeat(5000), todo: &todo };
        let n = context_block(&c).len() / 4;
        assert!(n < 3500, "about {n} tokens");
    }

    #[test]
    fn fit_keeps_the_newest_and_cuts_old_results() {
        let mut rows: Vec<(String, String)> = vec![("user".into(), "the very first message".into())];
        for i in 0..30 { rows.push(("assistant".into(), format!("move {i}"))); rows.push(("result".into(), format!("{i} {}", "x".repeat(1000)))); }
        rows.push(("user".into(), "newest".into()));
        let got = fit(&rows, 3000);
        assert_eq!(got.last().unwrap().content, "newest");
        assert!(!got.iter().any(|m| m.content == "the very first message"), "the oldest goes first");
        let results: Vec<&Msg> = got.iter().filter(|m| m.content.starts_with("[result]")).collect();
        assert!(results.iter().rev().take(6).all(|m| m.content.len() > 1000), "the newest six stay whole");
        assert!(results.iter().rev().skip(6).all(|m| m.content.ends_with("(cut)")), "older ones are cut");
        assert!(got.iter().map(|m| m.content.len() / 4 + 4).sum::<usize>() <= 3000);
    }

    #[test]
    fn the_request_and_the_todo_are_always_in_the_context_block() {
        let c = Context { machine: "m", places: "p", programs: &[], instructions: &[], journal: "", notes: "", tips: "", request: "make it blue", todo: &["[x] a".into(), "[ ] b".into()] };
        let s = context_block(&c);
        assert!(s.contains("The request you are working on: make it blue") && s.contains("Your to-do list: [x] a / [ ] b"), "{s}");
    }
}
