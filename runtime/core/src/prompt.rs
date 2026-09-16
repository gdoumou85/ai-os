use crate::job::{Job, State};
use crate::model::Prompt;
use crate::store::ProjectRow;
use executor::action::Action;

/// The model's standing rules. Plain, short: a 9B model on 8k has no room for an essay.
pub const SYSTEM: &str = "You are the AI that runs this computer for its user. You answer with exactly one JSON move.
Rules:
- You act only through moves; the executor runs them and reports back. Never claim something ran unless the report says so.
- You cannot know the full scope of what the user imagines. When starting work, ask what you need to know (1-3 questions) unless the job is in creative mode; then decide yourself.
- Say what you understood before you act.
- Work from the project's BLUEPRINT.md: read it to find what to change and where. After each change, update BLUEPRINT.md in place (replace lines, never pile on; keep it as small as possible). Create it first for a new project.
- Edit code in place with edit_file (quote the exact passage). Use write_file only for new files. Use read_file with from_line/lines to read the part you need.
- A step that failed once will fail again. Read the reason and do something different, or replan. Only give_up as a last resort, and say what was missing.
- You are done only when a check proves it: done must carry a check action whose success is the proof.
- If something is worth remembering, write it down (BLUEPRINT.md, or `remember` for a standing instruction). You will not see this conversation again.
- Install software with `install` (apt), never with run_command apt. Enable, disable or restart services with `service`. Language packages (pip, npm, crates) come through `fetch_packages`: the sandbox has no other network.
- A file outside the project needs the user's yes; say in one line why you need it.
- The machine's own layout, settings and installed tools are housekeeping (`housekeep`), not a project.";

fn join_instructions(instructions: &[String]) -> String {
    if instructions.is_empty() { "(none)".into() } else { instructions.iter().map(|i| format!("- {i}")).collect::<Vec<_>>().join("\n") }
}

pub fn front_door(instructions: &[String], projects: &[ProjectRow], recent: &[(String, String)], message: &str) -> Prompt {
    let projects_txt = if projects.is_empty() { "(none yet)".into() } else {
        projects.iter().map(|p| format!("- {} — {}", p.name, p.description)).collect::<Vec<_>>().join("\n")
    };
    let recent_txt = recent.iter().map(|(r, t)| format!("{r}: {t}")).collect::<Vec<_>>().join("\n");
    let user = format!(
        "Standing instructions:\n{}\n\nProjects:\n{}\n\nRecent exchange:\n{}\n\nLegal moves now: reply (just talk), start (new work: give project, new_project, description, goal, creative, understood), or \
         housekeep (the machine itself: folders, settings, tools; give goal, understood). \
         Pick an existing project name when the user means one. Set creative=true only if the user said to decide yourself.\n\nUser says: {}",
        join_instructions(instructions), projects_txt, recent_txt, message
    );
    Prompt { system: SYSTEM.into(), user, allowed: vec!["reply", "start", "housekeep"] }
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

pub fn job_turn(instructions: &[String], job: &Job, blueprint: Option<&str>, last_run: Option<&str>) -> Prompt {
    let answers = if job.answers.is_empty() { "(none)".into() } else {
        job.answers.iter().map(|(q, a)| format!("- {q} -> {a}")).collect::<Vec<_>>().join("\n")
    };
    let plan = if job.plan.is_empty() { "(no plan yet)".into() } else {
        job.plan.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
    };
    let (header, bp_block) = if job.housekeeping {
        ("Housekeeping on the machine itself (scratch folder is the working directory): no project, no blueprint. Anything that must outlive this job is a setting: set_setting projects_root=<abs path under /data or /home/ai>.".to_string(), String::new())
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
    let user = format!(
        "Machine: Ubuntu Linux (python3, no `python`; apt via install; pip/npm/cargo via fetch_packages).\nStanding instructions:\n{}\n\n{}\nGoal: {}\nMode: {}\nWhat you told the user you understood: {}\n\nUser's answers:\n{}\n\nPlan:\n{}{}{}\n\nSteps so far:\n{}{}\n\n{}",
        join_instructions(instructions), header, job.goal, mode, job.understood, answers, plan, bp_block, last, summarise_steps(job), note, hint
    );
    Prompt { system: SYSTEM.into(), user, allowed: allowed_moves(job) }
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
    }

    #[test]
    fn front_door_allows_housekeep() {
        let p = front_door(&[], &[], &[], "hi");
        assert_eq!(p.allowed, vec!["reply", "start", "housekeep"]);
    }

    #[test]
    fn system_rules_mention_the_new_hands() {
        assert!(SYSTEM.contains("install"));
        assert!(SYSTEM.contains("fetch_packages"));
    }

    #[test]
    fn housekeeping_job_turn_has_no_project_no_blueprint() {
        let mut j = Job::new("scratch", "/data/projects/scratch", "prepare /data/work", false, "Housekeeping: preparing /data/work");
        j.housekeeping = true;
        let p = job_turn(&[], &j, None, None);
        assert!(p.user.contains("no project, no blueprint"), "{}", p.user);
        assert!(!p.user.contains("no blueprint yet"), "{}", p.user);
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
    }
}
