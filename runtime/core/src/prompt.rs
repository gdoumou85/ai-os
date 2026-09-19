use crate::job::{Job, State};
use crate::model::Prompt;
use crate::store::ProjectRow;
use executor::action::Action;

/// The model's standing rules. Plain, short: a 9B model on 8k has no room for an essay.
pub const SYSTEM: &str = "You are the AI that runs this computer for its user. You answer with exactly one JSON move.
Rules:
- You act only through moves; the executor runs them and reports back. Never claim something ran unless the report says so.
- Where a move needs the user's yes, the machine stops it and asks them for you. Take the step and let it be asked: never wait for permission before acting, and never give up for the want of a yes you cannot ask for yourself.
- You cannot know the full scope of what the user imagines. When starting work, ask what you need to know (1-3 questions) unless the job is in creative mode; then decide yourself. The user answers by clicking: give each question 2-4 short likely answers in options (options[i] for questions[i]), and leave its list empty only when the answer is theirs alone, like a name.
- Say what you understood before you act.
- Work from the project's BLUEPRINT.md: read it to find what to change and where. After each change, update BLUEPRINT.md in place (replace lines, never pile on; keep it as small as possible). Create it first for a new project.
- Edit code in place with edit_file (quote the exact passage). Use write_file only for new files. Use read_file with from_line/lines to read the part you need.
- A step that failed once will fail again. Read the reason and do something different, or replan. A step that already succeeded is done: read its result in the steps above and move on, never repeat it. Only give_up as a last resort, and say what was missing.
- You are done only when a check proves it: done must carry a check action whose success is the proof.
- If something is worth remembering, write it down (BLUEPRINT.md, or `remember` for a standing instruction). You will not see this conversation again.
- Install software with `install` (apt), never with run_command apt. Enable, disable or restart services with `service`. Language packages (pip, npm, crates) come through `fetch_packages`: the sandbox has no other network.
- Make a folder outside the working directory with `make_dir` and an absolute path, never `run_command mkdir`: the sandbox can only write inside the working directory, so mkdir there reports a read-only filesystem. A folder inside the working directory is the sandbox's own: make it with `run_command mkdir -p src`. `make_dir` is never used with a relative path.
- The project's own files are named relative to the working directory (`BLUEPRINT.md`, `src/main.py`), never by an absolute path: an absolute path leaves the workspace and needs the user's yes.
- A file outside the project is written with `write_file` and its absolute path, never with run_command (`echo`, `tee`, `cp`): the sandbox cannot reach out there, so such a command reports success and writes nothing. It needs the user's yes (reading under /etc is free); say in one line why you need it.
- Remove software with `remove` and change a setting with `set_setting`; both are hands like the rest, not run_command.
- The machine's own layout, settings and installed tools are housekeeping (`housekeep`), not a project; so is using a program or a website for the user (opening it, clicking, filling it in). A project is something you build and keep as files.
- Programs on the desktop are worked through their controls, never through run_command: `look` with no window lists the open windows; `look` with a window lists its controls with ids (narrow with find); `press` a control by its id and name; `type` text into a control by id; `read` a text control by id; `open_app` opens a program by its desktop name (like org.gnome.TextEditor), on the visible display only if the user asked to see it. Look before you act and look again after; ids come from the latest look. A control that reports it has no action to press is a wrapper and the refusal names the control to press instead: press that one, do not look for another way. A program's own commands — save, print, find — may not be in the window itself: look for its menu or menu button, press it, look again, and press the command in the menu that opened. A control with no name of its own is listed by its keyboard shortcut and that shortcut is its name, so `Ctrl+S` is the one that saves. What a window shows is proven with `read` or `look`, never with run_command: the sandbox cannot see a window.
- The screen is the last resort, for what `look` cannot reach (a web page, an app that lists no controls): `screen_look` shows the screen under numbered squares 1-48; `screen_look` with a cell shows that square enlarged under spots 1-16; `screen_click` a spot in the square you just enlarged, naming what you click; `screen_type` types into what has the focus (enter to press Enter after). Look, enlarge, click, then look again to see what happened; every click needs a fresh enlarged look. If the person moves the mouse, you stop.";

fn join_instructions(instructions: &[String]) -> String {
    if instructions.is_empty() { "(none)".into() } else { instructions.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n") }
}

pub fn front_door(instructions: &[String], projects: &[ProjectRow], recent: &[(String, String)], message: &str) -> Prompt {
    let projects_txt = if projects.is_empty() { "(none yet)".into() } else {
        projects.iter().map(|p| format!("- {} — {}", p.name, p.description)).collect::<Vec<_>>().join("\n")
    };
    let recent_txt = recent.iter().map(|(r, t)| format!("{r}: {t}")).collect::<Vec<_>>().join("\n");
    let user = format!(
        "Standing instructions:\n{}\n\nProjects:\n{}\n\nRecent exchange:\n{}\n\nLegal moves now: reply (just talk), start (something to build and keep as files — code, documents, a site: give project, new_project, description, goal, creative, understood), or \
         housekeep (the machine itself: folders, settings, tools, or a program on the desktop — a window the user named, or one you open yourself to do what was asked, a browser and the websites in it included; give goal, understood). \
         Pick an existing project name when the user means one. Set creative=true only if the user said to decide yourself. \
         The goal carries the whole of what the user asked for, including what is to hold from now on — the job reads it verbatim.\n\nUser says: {}",
        join_instructions(instructions), projects_txt, recent_txt, message
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
fn compact_action(action: &Action) -> String {
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

/// Last 6 steps in full (but compact — see `compact_action`); older ones one line each
/// (budget, parent §4.3).
pub fn summarise_steps(job: &Job) -> String {
    let n = job.steps.len();
    let mut out = String::new();
    for (i, s) in job.steps.iter().enumerate() {
        let kind = serde_json::to_value(&s.action).ok().and_then(|v| v["kind"].as_str().map(String::from)).unwrap_or_default();
        let status = if s.ok { "ok" } else { "failed" };
        if i + 6 < n {
            out.push_str(&format!("step {}: {kind} {status}\n", i + 1));
        } else {
            out.push_str(&format!("step {} (plan step {}): {} -> {status}: {}\n", i + 1, s.plan_step, compact_action(&s.action), s.detail));
        }
    }
    if out.is_empty() { "(nothing done yet)".into() } else { out }
}

/// The owner's home as the engine sees it, named beside /data as a root the wrapper accepts
/// (design §9.1). No home, no invented one: the header then promises only /data.
fn home_clause(home: Option<String>) -> String {
    match home { Some(h) if !h.is_empty() => format!(" and {h}"), _ => String::new() }
}

pub fn job_turn(instructions: &[String], job: &Job, blueprint: Option<&str>, last_run: Option<&str>) -> Prompt {
    let answers = if job.answers.is_empty() { "(none)".into() } else {
        job.answers.iter().map(|(q, a)| format!("- {q} -> {a}")).collect::<Vec<_>>().join("\n")
    };
    let plan = if job.plan.is_empty() { "(no plan yet)".into() } else {
        job.plan.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
    };
    let (header, bp_block) = if job.housekeeping {
        (format!("Housekeeping on the machine itself (scratch folder is the working directory): no project, no blueprint — never write or read a BLUEPRINT.md here. The sandbox cannot see outside the scratch folder — that limits checking, never doing: make_dir and the other hands work anywhere under /data{}, and a check out there reports what the sandbox cannot see, not what is not there. Anything that must outlive this job is a setting. If the user's request is about where projects live from now on: 1) make_dir the folder, 2) set_setting projects_root=<that absolute path>, 3) done with make_dir (or run_command ls) as the check, and that job is not done until the setting is set. A window the user named is found with `look` first; nothing inside a window is a file of yours.", home_clause(std::env::var("HOME").ok())), String::new())
    } else {
        let bp = match blueprint {
            Some(b) => b.chars().take(3000).collect::<String>(),
            None => "(no blueprint yet — create BLUEPRINT.md with write_file before changing anything else)".into(),
        };
        (format!("Project: {} (its folder is the working directory)", job.project), format!("\n\nBLUEPRINT.md:\n{bp}"))
    };
    let hint = match job.state {
        State::Asking => "Legal moves now: ask (1-3 questions) or plan (if you have no questions).",
        State::Planning => "Legal moves now: plan. Give 2-8 short steps in plain words.",
        _ => "Legal moves now: act (one action for the plan step it serves), replan, done (with a check action), give_up (say what was missing).",
    };
    let note = job.note_to_model.as_deref().map(|n| format!("\n\nNote from the executor: {n}")).unwrap_or_default();
    // The bounded last-run note (1b spec §6.6): fix first, prove it, then the goal.
    let last = last_run.map(|l| format!("\n\nLAST RUN in this project ended badly:\n{}\nFix this first and prove it with a check, then carry on with the goal.", l.chars().take(1500).collect::<String>())).unwrap_or_default();
    let mode = if job.creative { "creative (do not ask; decide yourself)" } else { "ask" };
    // `Goal` is the model's own paraphrase from the front door, and it drops what it did not
    // think mattered ("…where all my projects will live from now on" became "for project
    // storage"). The words the user actually used go right under it. Jobs saved before 1c
    // carry none, and then the line is simply not there.
    let verbatim = if job.request.is_empty() { String::new() } else { format!("The user asked (verbatim): {}\n", job.request) };
    let user = format!(
        "Machine: Ubuntu Linux (python3, no `python`; apt via install; pip/npm/cargo via fetch_packages).\nStanding instructions:\n{}\n\n{}\nGoal: {}\n{}Mode: {}\nWhat you told the user you understood: {}\n\nUser's answers:\n{}\n\nPlan:\n{}{}{}\n\nSteps so far:\n{}{}\n\n{}",
        join_instructions(instructions), header, job.goal, verbatim, mode, job.understood, answers, plan, bp_block, last, summarise_steps(job), note, hint
    );
    Prompt { system: SYSTEM.into(), user, allowed: allowed_moves(job), image: None }
}

/// The user asked something instead of yes or no while an action waits for their OK (1d §2.1).
/// Reply only: the answer is words, never a move that changes the job.
pub fn approval_question(instructions: &[String], job: &Job, what: &str, why: &str, question: &str) -> Prompt {
    let user = format!(
        "Standing instructions:\n{}\n\nJob: {} — {}\nAn action is waiting for the user's OK: {what} (reason: {why}).\nThe user asked: {question}\nAnswer the question in one or two plain sentences so they can decide. Do not act, do not decide for them, do not ask them for the OK yourself (the system asks again).",
        join_instructions(instructions), if job.housekeeping { "housekeeping" } else { &job.project }, job.goal,
    );
    Prompt { system: SYSTEM.into(), user, allowed: vec!["reply"], image: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, State, StepRecord};
    use crate::store::ProjectRow;
    use executor::action::Action;

    fn instr() -> Vec<String> { vec!["always use python3".into()] }

    #[test]
    fn front_door_carries_instructions_projects_and_last_exchanges_only() {
        let projects = vec![ProjectRow { name: "primes".into(), folder: "/p/primes".into(), description: "prime printer".into(), touched_at: 1 }];
        let recent = vec![("user".into(), "old".into()), ("assistant".into(), "older reply".into())];
        let p = front_door(&instr(), &projects, &recent, "add a menu");
        assert!(p.system.contains("write it down"), "the memory rule must be in the system text");
        assert!(p.user.contains("always use python3"));
        assert!(p.user.contains("primes — prime printer"));
        assert!(p.user.contains("older reply"));
        assert!(p.user.ends_with("add a menu"));
        assert!(p.user.contains("reply") && p.user.contains("start") && p.user.contains("housekeep"), "front door names its three legal moves");
        assert!(p.user.contains("a browser and the websites in it included"), "the owner's website login was started as a project (2026-09-19)");
        // The live run's finding: "prepare a folder where all my projects will live from now on"
        // reached the job as "create a folder for project storage" — the standing half of the
        // request was summarised away at the door, and the job could not act on what it never saw.
        assert!(p.user.contains("The goal carries the whole of what the user asked for"), "{}", p.user);
    }

    #[test]
    fn front_door_allows_housekeep() {
        let p = front_door(&[], &[], &[], "hi");
        assert_eq!(p.allowed, vec!["reply", "start", "housekeep"]);
    }

    #[test]
    fn system_rules_say_who_asks_the_user_and_which_hand_reaches_outside() {
        // The 1d live run's findings, one line each. Without the first, the 9B planned the
        // write_file it needed and then gave up on the job "because the user did not provide
        // confirmation" — a yes it has no way to ask for. Without the second it wrote /etc with
        // `echo`, which the sealed sandbox reports as a success that wrote nothing. Without the
        // third it used `make_dir` for a folder in its own project.
        assert!(SYSTEM.contains("never wait for permission before acting"));
        assert!(SYSTEM.contains("give each question 2-4 short likely answers in options"), "the answers the rail offers as buttons");
        assert!(SYSTEM.contains("A file outside the project is written with `write_file`"));
        assert!(SYSTEM.contains("`make_dir` is never used with a relative path"));
    }

    #[test]
    fn system_rules_mention_the_new_hands() {
        assert!(SYSTEM.contains("install"));
        assert!(SYSTEM.contains("fetch_packages"));
        // The live run's first finding: without this the 9B tried `run_command mkdir /data/work`,
        // read "Read-only file system" as the machine's truth and gave up on the whole job.
        assert!(SYSTEM.contains("make_dir"));
        // Third finding: absolute paths for the project's own files put every write one level
        // above the workspace, so each one needed an approval and nothing landed in the project.
        assert!(SYSTEM.contains("relative to the working directory"));
        // Reading /etc is Auto (rules::classify), so the flat "needs the user's yes" sent the
        // model asking for approvals it never needed.
        assert!(SYSTEM.contains("reading under /etc is free"));
        // The two hands the list left out: the model reached for run_command instead.
        assert!(SYSTEM.contains("`remove`") && SYSTEM.contains("`set_setting`"));
    }

    #[test]
    fn system_rules_teach_the_desktop_hand_and_name_no_application() {
        for w in ["`look`", "`press`", "`type`", "`read`", "`open_app`", "Look before you act", "`screen_look`", "`screen_click`", "`screen_type`", "last resort"] { assert!(SYSTEM.contains(w), "{w}"); }
        // The live 2a run: the model typed the line five times over and reached for Close, because
        // nothing told it that Save lives behind the menu button and answers to `Ctrl+S`.
        for w in ["look for its menu or menu button", "`Ctrl+S`"] { assert!(SYSTEM.contains(w), "{w}"); }
        assert!(!SYSTEM.contains("Writer") && !SYSTEM.contains("Calculator") && !SYSTEM.contains("Text Editor"), "nothing per-app");
        let p = front_door(&[], &[], &[], "take my editor window");
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
        assert!(p.user.contains("If the user's request is about where projects live from now on"), "{}", p.user);
        assert!(p.user.contains("1) make_dir the folder, 2) set_setting projects_root"), "{}", p.user);
        assert!(p.user.contains("that job is not done until the setting is set"), "{}", p.user);
        // Third finding: with no way to look outside the scratch folder, the 9B invented a file
        // to read as its check (`/etc/settings.conf`) and gave up when it could not be read.
        assert!(p.user.contains("cannot see outside the scratch folder"), "{}", p.user);
    }

    /// The housekeeping header names the home of whoever runs the engine, not a fixed user —
    /// and names no home at all rather than invent one when the engine has no HOME.
    #[test]
    fn the_housekeeping_header_names_the_owners_home() {
        assert_eq!(home_clause(Some("/home/sam".into())), " and /home/sam");
        assert_eq!(home_clause(None), "");
        assert_eq!(home_clause(Some(String::new())), "");
        let h = std::env::var("HOME").expect("the engine always runs with a HOME");
        let mut job = Job::new("scratch", "/data/projects/scratch", "tidy", false, "Tidying");
        job.housekeeping = true;
        let p = job_turn(&[], &job, None, None);
        let text = format!("{}{}", p.system, p.user);
        assert!(text.contains(&format!("under /data and {h}")), "{text}");
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
        assert!(s.contains("step 3: run_command failed"), "{s}");
        assert!(!s.contains("detail-2"), "old details are dropped: {s}");
        assert!(s.contains("detail-8"), "recent details are kept: {s}");
        let big = "x".repeat(10_000);
        let p = job_turn(&[], &j, Some(&big), None);
        assert!(p.user.len() < 6_000, "blueprint must be capped at 3000 chars: {}", p.user.len());
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

        let p = front_door(&[], &[], &[], "hi");
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

        check(&front_door(&[], &[], &[], "hi"));

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

        check(&approval_question(&[], &Job::new("p", "/data/projects/p", "g", false, "u"), "w", "y", "q"));
    }

    #[test]
    fn approval_question_is_reply_only_and_carries_the_action() {
        let job = Job::new("p", "/data/projects/p", "g", false, "u");
        let p = approval_question(&[], &job, "http post to https://x", "network access", "what does it send?");
        assert_eq!(p.allowed, vec!["reply"]);
        assert!(p.user.contains("http post to https://x") && p.user.contains("network access") && p.user.contains("what does it send?"));
        assert!(p.user.contains("Do not") , "tells the model not to act");
    }
}
