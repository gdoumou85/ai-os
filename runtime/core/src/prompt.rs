use crate::job::{Job, State};
use crate::model::Prompt;
use crate::store::ProjectRow;
use executor::action::Action;

/// The model's standing rules. Plain, short: a 9B model on 8k has no room for an essay.
pub const SYSTEM: &str = "You are the AI that runs this computer for its user. You answer with exactly one JSON move.
Rules:
- You act only through moves; the executor runs them and reports back. Never claim something ran unless the report says so.
- You have full access to this computer: every folder, the whole network, and root through `sudo` (it never asks for a password). Nobody is asked before you act: take the step yourself, never wait for permission and never ask for it.
- You cannot know the full scope of what the user imagines. When starting work, ask what you need to know (1-3 questions, only what the user's words leave open) unless the job is in creative mode; then decide yourself. A plain order ("delete X", "install Y") leaves nothing open: do it. The user answers by clicking: give each question 2-4 short likely answers in options (options[i] for questions[i]), and leave its list empty only when the answer is theirs alone, like a name.
- Say what you understood before you act.
- Work from the project's BLUEPRINT.md: read it to find what to change and where. After each change, update BLUEPRINT.md in place (replace lines, never pile on; keep it as small as possible). Create it first for a new project.
- Edit code in place with edit_file (quote the exact passage). Use write_file only for new files. Use read_file with from_line/lines to read the part you need.
- A step that failed once will fail again unless something changed since. Read the reason and do something different, or replan. A step that already succeeded is done: read its result in the steps above and move on, never repeat it. Only give_up as a last resort, and say what was missing.
- You are done only when a check proves it: done must carry a check action whose success is the proof.
- If something is worth remembering, write it down (BLUEPRINT.md, or `remember` for a standing instruction — only what the user said is to hold from now on). You will not see this conversation again.
- Never install what is already there: *This machine* lists what is installed; use it. For anything it does not list, `run_command <program> --help` answers in a second; an install takes minutes.
- Install, remove and set up software with run_command: `sudo apt-get install -y …`, `sudo apt-get remove -y …`, `sudo systemctl …`; language packages with pip (into the working directory's .venv), npm or cargo. Install a program so it can be found again: `sudo apt-get install -y`, else `sudo snap install`, else `flatpak install -y flathub`. Never leave a loose download of a program (an AppImage, an unpacked archive; never a project): put it under /opt/<name> and write its launcher to /usr/local/share/applications/<name>.desktop. Make folders with `run_command mkdir -p`. run_command runs the program directly, with no shell: for *, ~, $VAR, pipes, > or &&, send ["bash","-c","<the line>"].
- read_file, write_file and edit_file take relative or absolute paths; a file only root may write is written for you. Where projects live is changed with `set_setting`, only when the user asks for it.
- The machine's own layout, settings and installed tools are housekeeping (`housekeep`), not a project; so is using a program or a website for the user (opening it, clicking, filling it in). A project is something you build and keep as files.
- Programs on the desktop are worked through their controls, never through run_command: `look` with no window lists the open windows; `look` with a window lists its controls with ids (narrow with find); `press` a control by its id and name; `type` text into a control by id; `read` a text control by id; `open_app` opens a program by its desktop name (like org.gnome.TextEditor), on the visible display only if the user asked to see it. Look before you act and look again after; ids come from the latest look. A control that reports it has no action to press is a wrapper and the refusal names the control to press instead: press that one, do not look for another way. A program's own commands — save, print, find — may not be in the window itself: look for its menu or menu button, press it, look again, and press the command in the menu that opened. A control with no name of its own is listed by its keyboard shortcut and that shortcut is its name, so `Ctrl+S` is the one that saves. What a window shows is proven with `read` or `look`.
- A program that has a command line or a script mode is driven that way rather than by clicking — it is quicker and its result can be proved. Ask it with `run_command <program> --help` before you open its window.
- The screen is the last resort, for what `look` cannot reach (a web page, an app that lists no controls): `screen_look` shows the screen under numbered squares 1-48; `screen_look` with a cell shows that square enlarged under spots 1-16; `screen_click` a spot in the square you just enlarged, naming what you click; `screen_type` types into what has the focus (enter to press Enter after). Look, enlarge, click, then look again to see what happened; every click needs a fresh enlarged look.";

fn join_instructions(instructions: &[String]) -> String {
    if instructions.is_empty() { "(none)".into() } else { instructions.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n") }
}

/// A project the message names: its whole name, or one word of it (4+ letters) — "the car rental
/// site" names car-rental-broker. The front door listed every project, and a 9B greeted with "Hi"
/// picked the only one and started work on it, whatever the prompt said (the owner, 2026-09-23).
fn named_in(p: &ProjectRow, message: &str) -> bool {
    let m = message.to_lowercase();
    let name = p.name.to_lowercase();
    m.contains(&name) || name.split(|c: char| !c.is_alphanumeric()).any(|w| w.len() >= 4 && m.contains(w))
}

pub fn front_door(instructions: &[String], projects: &[ProjectRow], notebooks: &[String], recent: &[(String, String)], message: &str) -> Prompt {
    let named: Vec<&ProjectRow> = projects.iter().filter(|p| named_in(p, message)).collect();
    let projects_txt = if named.is_empty() { "(none named)".into() } else {
        named.iter().map(|p| format!("- {} — {}", p.name, p.description)).collect::<Vec<_>>().join("\n")
    };
    let notebooks_txt = if notebooks.is_empty() { "(none yet)".to_string() } else { notebooks.join(", ") };
    let recent_txt = recent.iter().map(|(r, t)| format!("{r}: {t}")).collect::<Vec<_>>().join("\n");
    let user = format!(
        "Standing instructions:\n{}\n\nProjects the user named:\n{}\n\nSkill notebooks: {}\n\nRecent exchange:\n{}\n\nLegal moves now: reply (just talk: it runs nothing, so when the user wants something done — 'do it', 'proceed' — start or housekeep instead), start (something to build and keep as files — code, documents, a site: give project, new_project, description, goal, creative, understood, and skills (0-3 craft areas the job belongs to — coding, web design, a program like blender — named from the notebooks listed or a new short name; [] for a plain errand)), or \
         housekeep (the machine itself: folders, settings, tools, or a program on the desktop — a window the user named, or one you open yourself to do what was asked, a browser and the websites in it included; give goal, understood). \
         Pick a project listed there when the user means it; when the user says where a project is, give that absolute path as folder. Set creative=true only if the user said to decide yourself. \
         The goal carries the whole of what the user asked for, including what is to hold from now on — the job reads it verbatim. \
         Asked to forget, or to wipe what you remember, reply that the Clear button at the top does it: you cannot, and it is no job.\n\nUser says: {}",
        join_instructions(instructions), projects_txt, notebooks_txt, recent_txt, message
    );
    Prompt { system: SYSTEM.into(), user, allowed: vec!["reply", "start", "housekeep"], image: None }
}

/// The moves legal right now, by job state and mode (decision 13): sent as `Prompt::allowed` so
/// the grammar itself excludes the rest — prose alone ("Legal moves now: ...") doesn't reliably
/// bind a 9B model. `ask` is legal in `Asking`, and also in `Planning`/working-and-beyond when the
/// job isn't creative (mirrors `Engine::run_turns`'s match arms exactly).
fn allowed_moves(job: &Job) -> Vec<&'static str> {
    match job.state {
        State::Asking => vec!["ask", "plan"],
        State::Planning => {
            let mut v = vec![];
            if !job.creative { v.push("ask"); }
            v.push("plan");
            v
        }
        _ => {
            let mut v = vec![];
            if !job.creative { v.push("ask"); }
            v.extend(["act", "replan", "done", "give_up"]);
            v
        }
    }
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

/// Last 6 steps in full (but compact — see `compact_action`); the 24 before them one line each;
/// anything older a count (budget, parent §4.3): a job may run 200 steps, and 200 one-liners
/// would take a fifth of an 8k context.
pub fn summarise_steps(job: &Job) -> String {
    let n = job.steps.len();
    let mut out = String::new();
    let old = n.saturating_sub(30);
    if old > 0 {
        let failed = job.steps[..old].iter().filter(|s| !s.ok).count();
        out.push_str(&format!("steps 1-{old}: {} ok, {failed} failed\n", old - failed));
    }
    for (i, s) in job.steps.iter().enumerate().skip(old) {
        let status = if s.ok { "ok" } else { "failed" };
        if i + 6 < n {
            // What it was, not only its kind: "step 9: run_command ok" left the model guessing
            // which folder it had listed (review, 2026-09-23). Capped for the 8k budget.
            let what: String = crate::event::describe(&s.action).chars().take(40).collect();
            out.push_str(&format!("step {}: {what} {status}\n", i + 1));
        } else {
            // The newest step in full; the five before it shorter (the 8k budget): a failure's
            // reason is at the end of its output, a success's news at the start.
            let (action, detail) = if i + 1 == n { (compact_action(&s.action), s.detail.clone()) } else {
                let d = s.detail.chars().count();
                (compact_action(&s.action).chars().take(200).collect(),
                 if s.ok { s.detail.chars().take(250).collect() } else { s.detail.chars().skip(d.saturating_sub(250)).collect() })
            };
            out.push_str(&format!("step {} (plan step {}): {action} -> {status}: {detail}\n", i + 1, s.plan_step));
        }
    }
    if out.is_empty() { "(nothing done yet)".into() } else { out }
}

pub fn job_turn(instructions: &[String], job: &Job, blueprint: Option<&str>, last_run: Option<&str>) -> Prompt {
    let answers = if job.answers.is_empty() { "(none)".into() } else {
        job.answers.iter().map(|(q, a)| format!("- {q} -> {a}")).collect::<Vec<_>>().join("\n")
    };
    let plan = if job.plan.is_empty() { "(no plan yet)".into() } else {
        job.plan.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
    };
    let (header, bp_block) = if job.housekeeping {
        ("Housekeeping on the machine itself (scratch folder is the working directory): no project, no blueprint — never write or read a BLUEPRINT.md here. You can reach and check anything on the machine: *Where things are* above says where the user's projects and folders are, so work on those paths straight away — to remove projects, run_command rm -rf their folders. Anything that must outlive this job is a setting. Only if the user asks to move where projects live from now on: 1) run_command mkdir -p the folder, 2) set_setting projects_root=<that absolute path>, 3) done with run_command ls <that folder> as the check, and that job is not done until the setting is set. A window the user named is found with `look` first; nothing inside a window is a file of yours.".to_string(), String::new())
    } else {
        let bp = match blueprint {
            Some(b) => b.chars().take(3000).collect::<String>(),
            None => "(no blueprint yet — create BLUEPRINT.md with write_file before changing anything else)".into(),
        };
        // Told only "the working directory", a 9B planned "create the project folder and set
        // projects_root" and wrote the project into /opt/flappy_bird (the owner's run, 2026-09-23).
        (format!("Project: {}. Its folder {} is already made and is the working directory: all its files go there, named relative to it (BLUEPRINT.md, src/main.py). Never make another; never change projects_root.", job.project, job.folder), format!("\n\nBLUEPRINT.md:\n{bp}"))
    };
    // `ask` was in the grammar while working but not in these words, so a plan step "ask the user
    // whether…" became a file of questions written twice, and the job gave up (the owner, 2026-09-23).
    let hint = match job.state {
        State::Asking => "Legal moves now: ask (1-3 questions, only what the user's words above leave open) or plan (if they leave nothing open). A question for the user is an ask, never a plan step.",
        State::Planning => "Legal moves now: plan. Give 2-8 short steps in plain words.",
        _ if job.creative => "Legal moves now: act (one action for the plan step it serves), replan, done (with a check action), give_up (say what was missing).",
        _ => "Legal moves now: act (one action for the plan step it serves), ask (only what the user's words and answers above leave open, and only the user can answer: the job waits for them; never write questions into a file), replan, done (with a check action), give_up (say what was missing).",
    };
    let note = job.note_to_model.as_deref().map(|n| format!("\n\nNote from the executor: {n}")).unwrap_or_default();
    // The bounded last-run note (1b spec §6.6): fix first, prove it, then the goal.
    let last = last_run.map(|l| format!("\n\nLAST RUN in this project ended badly:\n{}\nFix this first and prove it with a check, then carry on with the goal.", l.chars().take(1500).collect::<String>())).unwrap_or_default();
    let mode = if job.creative { "creative (do not ask; decide yourself)" } else { "ask" };
    let tips = if job.notes_block.is_empty() { String::new() } else { format!("\n\n{}", job.notes_block) };
    // `Goal` is the model's own paraphrase from the front door, and it drops what it did not
    // think mattered ("…where all my projects will live from now on" became "for project
    // storage"). The words the user actually used go right under it. Jobs saved before 1c
    // carry none, and then the line is simply not there.
    let verbatim = if job.request.is_empty() { String::new() } else { format!("The user asked (verbatim): {}\n", job.request) };
    let user = format!(
        "Machine: Ubuntu Linux (python3, no `python`; root through sudo; pip packages go into the working directory's .venv — `python3 -m venv .venv`, then .venv/bin/pip — and run from there: .venv/bin/python, .venv/bin/django-admin).\nStanding instructions:\n{}\n\n{}\nGoal: {}\n{}Mode: {}\nWhat you told the user you understood: {}\n\nUser's answers:\n{}\n\nPlan:\n{}{}{}{}\n\nSteps so far:\n{}{}\n\n{}",
        join_instructions(instructions), header, job.goal, verbatim, mode, job.understood, answers, plan, bp_block, last, tips, summarise_steps(job), note, hint
    );
    Prompt { system: SYSTEM.into(), user, allowed: allowed_moves(job), image: None }
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
    Prompt { system: SYSTEM.into(), user, allowed: vec!["learn"], image: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, State, StepRecord};
    use crate::store::ProjectRow;
    use executor::action::Action;

    fn instr() -> Vec<String> { vec!["always use python3".into()] }

    #[test]
    fn the_front_door_lists_the_notebooks_and_asks_for_skills() {
        let p = front_door(&[], &[], &["this computer".into(), "blender".into()], &[], "make a ball in blender");
        assert!(p.user.contains("Skill notebooks: this computer, blender"), "{}", p.user);
        assert!(p.user.contains("skills (0-3"), "{}", p.user);
        assert!(front_door(&[], &[], &[], &[], "hi").user.contains("Skill notebooks: (none yet)"));
    }

    #[test]
    fn the_job_turn_carries_the_tips_fixed_at_start() {
        let mut j = Job::new_housekeeping("/data/housekeeping", "go to a site", "u");
        assert!(!job_turn(&[], &j, None, None).user.contains("What you learned before"));
        j.notes_block = "What you learned before on this computer (tips…):\n- this computer/open a website: open_app firefox".into();
        let user = job_turn(&[], &j, None, None).user;
        assert!(user.contains("- this computer/open a website"), "{user}");
        assert!(user.find("What you learned before").unwrap() < user.find("Steps so far").unwrap(), "{user}");
    }

    #[test]
    fn a_job_turn_with_everything_at_its_cap_still_fits_an_8k_context() {
        // Tokens ≈ chars/4; 6500 leaves the answer room in 8192 (Phase 3 §3's 3000-char tips block
        // is what this guards: it came on top of a prompt already sized for 8k).
        let mut j = Job::new("p", "/data/projects/p", &"g".repeat(300), false, &"u".repeat(300));
        j.state = State::Working;
        j.request = "r".repeat(500);
        j.answers = (0..3).map(|i| (format!("question {i}"), "a".repeat(100))).collect();
        j.plan = (0..8).map(|i| format!("{i} {}", "p".repeat(60))).collect();
        j.notes_block = "t".repeat(3000);
        j.note_to_model = Some("n".repeat(200));
        j.steps = (0..40).map(|_| StepRecord { plan_step: 1, action: Action::RunCommand { argv: vec!["x".repeat(1000)] }, ok: true, detail: "d".repeat(500) }).collect();
        let instructions: Vec<String> = (0..5).map(|_| "i".repeat(200)).collect();
        let p = job_turn(&instructions, &j, Some(&"b".repeat(4000)), Some(&"l".repeat(2000)));
        // The engine puts the machine in front of every turn: its fullest form counts too.
        let apps: Vec<crate::machine::App> = (0..80).map(|i| crate::machine::App { id: format!("org.example.Application{i}"), name: format!("Application {i}") }).collect();
        let tools: Vec<String> = (0..16).map(|i| format!("tool{i}")).collect();
        let mine: Vec<(String, String)> = (0..10).map(|i| (format!("package-{i}"), "apt".into())).collect();
        let machine = crate::machine::block(&"s".repeat(150), &apps, &tools, &mine);
        // And where things are, with its ten projects (engine::places).
        let places = format!("Where things are: the user's home is /home/{0}; projects live in /data/{0} (the projects_root setting); projects so far: {1}; the scratch folder for housekeeping is /data/housekeeping.",
            "u".repeat(20), (0..10).map(|i| format!("project-name-{i} (/data/projects/project-name-{i})")).collect::<Vec<_>>().join(", "));
        let tokens = (p.system.len() + p.user.len() + machine.len() + places.len()) / 4;
        assert!(tokens < 6500, "about {tokens} tokens");
    }

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
    fn front_door_carries_instructions_projects_and_last_exchanges_only() {
        let projects = vec![ProjectRow { name: "primes".into(), folder: "/p/primes".into(), description: "prime printer".into(), touched_at: 1 }];
        let recent = vec![("user".into(), "old".into()), ("assistant".into(), "older reply".into())];
        let p = front_door(&instr(), &projects, &[], &recent, "add a menu to primes");
        assert!(p.system.contains("write it down"), "the memory rule must be in the system text");
        assert!(p.user.contains("always use python3"));
        assert!(p.user.contains("primes — prime printer"));
        assert!(p.user.contains("older reply"));
        assert!(p.user.ends_with("add a menu to primes"));
        assert!(p.user.contains("reply") && p.user.contains("start") && p.user.contains("housekeep"), "front door names its three legal moves");
        assert!(p.user.contains("a browser and the websites in it included"), "the owner's website login was started as a project (2026-09-19)");
        // The live run's finding: "prepare a folder where all my projects will live from now on"
        // reached the job as "create a folder for project storage" — the standing half of the
        // request was summarised away at the door, and the job could not act on what it never saw.
        assert!(p.user.contains("The goal carries the whole of what the user asked for"), "{}", p.user);
    }

    #[test]
    fn the_front_door_shows_only_the_projects_the_message_names() {
        let car = vec![ProjectRow { name: "car-rental-broker".into(), folder: "/p/c".into(), description: "a website".into(), touched_at: 1 }];
        assert!(!front_door(&[], &car, &[], &[], "Hi").user.contains("car-rental"), "a greeting brings up no project");
        assert!(front_door(&[], &car, &[], &[], "carry on with the car rental site").user.contains("car-rental-broker — a website"));
        assert!(front_door(&[], &car, &[], &[], "open car-rental-broker").user.contains("car-rental-broker — a website"));
    }

    #[test]
    fn front_door_allows_housekeep() {
        let p = front_door(&[], &[], &[], &[], "hi");
        assert_eq!(p.allowed, vec!["reply", "start", "housekeep"]);
    }

    #[test]
    fn system_rules_give_full_access_and_never_mention_a_sandbox() {
        // Full access (2026-09-19): root, network, every folder, nobody asked.
        for w in ["full access", "`sudo`", "never wait for permission", "sudo apt-get install -y", "mkdir -p", "`set_setting`"] {
            assert!(SYSTEM.contains(w), "{w}");
        }
        for w in ["sandbox", "user's yes", "make_dir", "fetch_packages", "moves the mouse"] { assert!(!SYSTEM.contains(w), "{w}"); }
        assert!(SYSTEM.contains("give each question 2-4 short likely answers in options"), "the answers the rail offers as buttons");
        // The 1d run: absolute paths for the project's own files put every write one level above
        // the project, so nothing landed in it. Said in the project's own turn, with the folder.
        let turn = job_turn(&[], &Job::new("p", "/data/projects/p", "g", false, "u"), None, None).user;
        assert!(turn.contains("/data/projects/p is already made") && turn.contains("named relative to it"), "{turn}");
    }

    #[test]
    fn system_rules_teach_the_desktop_hand_and_name_no_application() {
        for w in ["`look`", "`press`", "`type`", "`read`", "`open_app`", "Look before you act", "`screen_look`", "`screen_click`", "`screen_type`", "last resort",
                  "--help", "Never install what is already there"] { assert!(SYSTEM.contains(w), "{w}"); }
        // The live 2a run: the model typed the line five times over and reached for Close, because
        // nothing told it that Save lives behind the menu button and answers to `Ctrl+S`.
        for w in ["look for its menu or menu button", "`Ctrl+S`"] { assert!(SYSTEM.contains(w), "{w}"); }
        assert!(!SYSTEM.contains("Writer") && !SYSTEM.contains("Calculator") && !SYSTEM.contains("Text Editor"), "nothing per-app");
        let p = front_door(&[], &[], &[], &[], "take my editor window");
        assert!(p.user.contains("a window the user named"), "{}", p.user);
        // The live 2a run: "open the calculator and tell me what 12 times 34 is" was read as a new
        // software project called calculator-task, blueprint and all, because housekeeping only
        // offered a window the user already had open.
        assert!(p.user.contains("or one you open yourself"), "{}", p.user);
        let job = Job::new_housekeeping("/data/housekeeping", "take the editor", "Taking it");
        let t = job_turn(&[], &job, None, None);
        assert!(t.user.contains("found with `look` first"), "{}", t.user);
    }

    #[test]
    fn the_job_turn_says_where_pip_packages_land() {
        // The owner's Django job (2026-09-19): fetched with pip, then checked with the system
        // python3, which cannot see the project's .venv, so a working install read as a failure.
        let j = Job::new("p", "/data/projects/p", "g", false, "u");
        assert!(job_turn(&[], &j, None, None).user.contains(".venv/bin/python"));
    }

    #[test]
    fn the_job_turn_carries_the_users_own_words_not_only_the_paraphrase() {
        let mut j = Job::new("p", "/data/projects/p", "Create a folder at /data/work for project storage.", false, "u");
        assert!(!job_turn(&[], &j, None, None).user.contains("verbatim"), "a job with no recorded request says nothing");
        j.request = "Prepare a folder at /data/work where all my projects will live from now on".into();
        let user = job_turn(&[], &j, None, None).user;
        assert!(user.contains("The user asked (verbatim): Prepare a folder at /data/work where all my projects will live from now on"), "{user}");
        // Right under the goal, so the paraphrase and the words it came from are read together.
        assert!(user.find("Goal:").unwrap() < user.find("The user asked (verbatim):").unwrap(), "{user}");
        assert!(user.find("The user asked (verbatim):").unwrap() < user.find("Mode:").unwrap(), "{user}");
    }

    #[test]
    fn housekeeping_job_turn_has_no_project_no_blueprint() {
        let mut j = Job::new("scratch", "/data/projects/scratch", "prepare /data/work", false, "Housekeeping: preparing /data/work");
        j.housekeeping = true;
        let p = job_turn(&[], &j, None, None);
        assert!(p.user.contains("no project, no blueprint"), "{}", p.user);
        assert!(!p.user.contains("no blueprint yet"), "{}", p.user);
        // The live run's second finding: told only "no blueprint", the 9B still wrote and then
        // re-read a BLUEPRINT.md in the folder it had just made, and never reached the setting.
        assert!(p.user.contains("never write or read a BLUEPRINT.md"), "{}", p.user);
        // The setting recipe is conditional on the user's request being about where projects
        // live — a housekeeping job that installs a tool has no setting to reach — and inside
        // that condition it is ordered, because the 9B needs the order (§11 Results).
        assert!(p.user.contains("Only if the user asks to move where projects live from now on"), "{}", p.user);
        // Clearing projects is not moving them (the owner's run, 2026-09-23): say how, on the paths given.
        assert!(p.user.contains("to remove projects, run_command rm -rf their folders"), "{}", p.user);
        assert!(p.user.contains("1) run_command mkdir -p the folder, 2) set_setting projects_root"), "{}", p.user);
        assert!(p.user.contains("that job is not done until the setting is set"), "{}", p.user);
        assert!(!p.user.contains("sandbox"), "{}", p.user);
    }

    #[test]
    fn job_turn_carries_goal_answers_plan_blueprint_and_state_hint() {
        let mut j = Job::new("primes", "/data/projects/primes", "print ten primes", false, "Starting primes");
        j.answers.push(("Which language?".into(), "python".into()));
        j.plan = vec!["write primes.py".into(), "run it".into()];
        j.state = State::Working;
        j.note_to_model = Some("done needs a check".into());
        let p = job_turn(&instr(), &j, Some("# primes\nprints primes"), None);
        assert!(p.user.contains("print ten primes"));
        assert!(p.user.contains("Which language? -> python"));
        assert!(p.user.contains("1. write primes.py"));
        assert!(p.user.contains("prints primes"));
        assert!(p.user.contains("done needs a check"));
        assert!(p.user.contains("act"), "working state hints the legal moves");
        assert!(!p.user.contains("LAST RUN"), "no last-run section when there is no note");
        let asking = Job::new("primes", "/data/projects/primes", "g", false, "u");
        assert!(job_turn(&[], &asking, None, None).user.contains("no blueprint yet"));
        assert!(job_turn(&[], &asking, None, None).user.contains("ask"));
    }

    #[test]
    fn last_run_note_is_shown_with_the_fix_first_rule() {
        let j = Job::new("primes", "/data/projects/primes", "g", true, "u");
        let p = job_turn(&[], &j, None, Some("goal: print primes\nfailed at plan step 2\nlast error: NameError: prmes"));
        assert!(p.user.contains("LAST RUN"));
        assert!(p.user.contains("NameError: prmes"));
        assert!(p.user.contains("Fix this first"), "{}", p.user);
    }

    #[test]
    fn older_steps_are_summarised_and_blueprint_is_capped() {
        let mut j = Job::new("p", "/data/projects/p", "g", true, "u");
        for i in 0..9 {
            j.steps.push(StepRecord { plan_step: 1, action: Action::RunCommand { argv: vec![format!("cmd{i}")] }, ok: i != 2, detail: format!("detail-{i}") });
        }
        let s = summarise_steps(&j);
        assert!(s.contains("step 3: ran cmd2 failed"), "{s}");
        assert!(!s.contains("detail-2"), "old details are dropped: {s}");
        assert!(s.contains("detail-8"), "recent details are kept: {s}");
        let big = "x".repeat(10_000);
        let p = job_turn(&[], &j, Some(&big), None);
        assert!(p.user.len() < 6_000, "blueprint must be capped at 3000 chars: {}", p.user.len());
    }

    #[test]
    fn a_long_job_keeps_its_step_list_small() {
        let mut j = Job::new("p", "/data/projects/p", "g", true, "u");
        for i in 0..200 {
            j.steps.push(StepRecord { plan_step: 1, action: Action::RunCommand { argv: vec![format!("cmd{i}")] }, ok: i % 10 != 0, detail: format!("detail-{i}") });
        }
        let s = summarise_steps(&j);
        assert!(s.starts_with("steps 1-170: 153 ok, 17 failed\n"), "{s}");
        assert_eq!(s.lines().count(), 1 + 30);
        assert!(s.contains("step 171: ran cmd170 ok") && s.contains("detail-199"), "{s}");
    }

    /// I4: a big `write_file` step must not blow the prompt budget — the content is summarised
    /// as a byte count, not re-serialised whole.
    #[test]
    fn a_large_write_file_step_does_not_blow_the_prompt_budget() {
        let mut j = Job::new("p", "/data/projects/p", "g", true, "u");
        let big = "x".repeat(20_000);
        j.steps.push(StepRecord { plan_step: 1, action: Action::WriteFile { path: "big.py".into(), contents: big }, ok: true, detail: "written".into() });
        let p = job_turn(&[], &j, None, None);
        assert!(p.user.len() < 6_000, "{}", p.user.len());
        assert!(p.user.contains("write_file big.py (20000 bytes)"), "{}", p.user);
    }

    #[test]
    fn allowed_moves_are_narrowed_by_state_and_mode() {
        let mut asking = Job::new("p", "/data/projects/p", "g", false, "u");
        asking.state = State::Asking;
        assert_eq!(job_turn(&[], &asking, None, None).allowed, vec!["ask", "plan"]);

        let mut planning_creative = Job::new("p", "/data/projects/p", "g", true, "u");
        planning_creative.state = State::Planning;
        assert_eq!(job_turn(&[], &planning_creative, None, None).allowed, vec!["plan"]);

        let mut planning_not_creative = Job::new("p", "/data/projects/p", "g", false, "u");
        planning_not_creative.state = State::Planning;
        assert_eq!(job_turn(&[], &planning_not_creative, None, None).allowed, vec!["ask", "plan"]);

        let mut working_creative = Job::new("p", "/data/projects/p", "g", true, "u");
        working_creative.state = State::Working;
        assert_eq!(job_turn(&[], &working_creative, None, None).allowed, vec!["act", "replan", "done", "give_up"]);

        let mut working_not_creative = Job::new("p", "/data/projects/p", "g", false, "u");
        working_not_creative.state = State::Working;
        assert_eq!(job_turn(&[], &working_not_creative, None, None).allowed, vec!["ask", "act", "replan", "done", "give_up"]);
        // The words name what the grammar allows: a working job that may ask is told so.
        assert!(job_turn(&[], &working_not_creative, None, None).user.contains("ask (only what the user"));
        assert!(!job_turn(&[], &working_creative, None, None).user.contains("ask (only what the user"));

        let p = front_door(&[], &[], &[], &[], "hi");
        assert_eq!(p.allowed, vec!["reply", "start", "housekeep"]);
    }

    /// `allowed_moves` and `front_door`'s `allowed` are hand-typed move-name lists, separate from
    /// `MOVE_SCHEMA` (schema.rs). A typo or a renamed move would silently narrow `oneOf` to fewer
    /// entries than intended — even to zero, locking the model out of every move — without this
    /// ever failing a type check. Prove every list this module produces actually exists in the
    /// real schema, for every state/mode combination `allowed_moves` handles.
    #[test]
    fn every_allowed_list_matches_a_real_move() {
        let check = |p: &crate::model::Prompt| {
            let narrowed = crate::model::narrow_schema(crate::schema::value(), &p.allowed);
            let n = narrowed["oneOf"].as_array().unwrap().len();
            assert_eq!(n, p.allowed.len(), "allowed list has a name not in MOVE_SCHEMA: {:?}", p.allowed);
            assert!(n > 0, "allowed list must never narrow to zero moves: {:?}", p.allowed);
        };

        check(&front_door(&[], &[], &[], &[], "hi"));

        let mut asking = Job::new("p", "/data/projects/p", "g", false, "u");
        asking.state = State::Asking;
        check(&job_turn(&[], &asking, None, None));

        let mut planning_creative = Job::new("p", "/data/projects/p", "g", true, "u");
        planning_creative.state = State::Planning;
        check(&job_turn(&[], &planning_creative, None, None));

        let mut planning_not_creative = Job::new("p", "/data/projects/p", "g", false, "u");
        planning_not_creative.state = State::Planning;
        check(&job_turn(&[], &planning_not_creative, None, None));

        let mut working_creative = Job::new("p", "/data/projects/p", "g", true, "u");
        working_creative.state = State::Working;
        check(&job_turn(&[], &working_creative, None, None));

        let mut working_not_creative = Job::new("p", "/data/projects/p", "g", false, "u");
        working_not_creative.state = State::Working;
        check(&job_turn(&[], &working_not_creative, None, None));

        let job = Job::new("p", "/data/projects/p", "g", false, "u");
        check(&learning_turn(&job, true));
    }
}
