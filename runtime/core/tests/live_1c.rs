// The Phase 1c acceptance test (1c design §11): the real local model, both hands (sandbox and
// the root wrapper), four scripts in order, no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_1c -- --nocapture
use aios_core::engine::Engine;
use aios_core::job::{Job, State};
use aios_core::model::OllamaModel;
use aios_core::store::Store;
use executor::admin::AdminWorker;
use executor::log::ActionLog;
use executor::worker::{SandboxWorker, Worker};
use std::path::{Path, PathBuf};
use std::time::Instant;

const DB: &str = "/data/ai-os-live-1c.db";
const WORK: &str = "/data/work";

fn engine() -> Engine<OllamaModel> {
    let store = Store::open(DB).unwrap();
    Engine::new(store, OllamaModel::local("qwen3.5:9b"), PathBuf::from("/data/projects"), Some(DB.into()),
        Box::new(|ws| (
            Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
            Box::new(AdminWorker) as Box<dyn Worker>,
        )),
        PathBuf::from("/data/housekeeping"),
        PathBuf::from("/data/snapshots"))
}

/// Asked of dpkg through the wrapper — the same list the install undo diffs, not our own memory.
fn installed(pkg: &str) -> bool {
    AdminWorker::pkg_list().expect("pkg-list through the wrapper").iter().any(|p| p == pkg)
}

/// The job the line just ran: newest touched, ties broken by insertion order like the store's own
/// "last job" query.
fn latest_job() -> Job {
    let conn = rusqlite::Connection::open(DB).unwrap();
    let json: String = conn
        .query_row("SELECT json FROM core_jobs ORDER BY updated_at DESC, rowid DESC LIMIT 1", [], |r| r.get(0))
        .unwrap();
    serde_json::from_str(&json).unwrap()
}

fn listing(dir: &str) -> Vec<String> {
    std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default()
}

/// One line of the conversation: say it, print every reply, and — for a model that asks anyway
/// after being told to decide, or wants an approval the script never planned for — answer the way
/// the user already has, a few times at most. Every nudge is printed, so the transcript shows
/// exactly how much human the run needed. Prints the step count and the seconds the line took,
/// which is what §11 wants recorded per script.
fn say(e: &mut Engine<OllamaModel>, label: &str, text: &str) -> Vec<String> {
    let t = Instant::now();
    println!("you> {text}");
    let mut out = e.handle(text).unwrap();
    for l in &out { println!("ai> {l}"); }
    for _ in 0..4 {
        let Some(job) = e.open_job().unwrap() else { break };
        let nudge = match job.state {
            State::WaitingAnswer => "You decide.",
            State::WaitingApproval => "yes",
            _ => break,
        };
        println!("you> {nudge}");
        out = e.handle(nudge).unwrap();
        for l in &out { println!("ai> {l}"); }
    }
    println!("[{label}] {} steps, {:.1}s", latest_job().steps.len(), t.elapsed().as_secs_f64());
    out
}

#[test]
fn the_machine_moves_its_projects_installs_undoes_and_fetches() {
    if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
    let _ = std::fs::remove_file(DB);
    let _ = std::fs::remove_dir_all(WORK);
    // Script 2 measures an install and script 3 the removal of it, so cowsay must start absent.
    // Left over from an interrupted run, it is cleared here — through the same wrapper.
    if installed("cowsay") {
        let out = AdminWorker::call("remove", &["--", "cowsay"], None);
        assert!(out.ok, "could not clear a leftover cowsay: {}", out.detail);
        assert!(!installed("cowsay"), "cowsay survived the cleanup remove");
    }
    let mut e = engine();

    // 1. The machine's own layout: a housekeeping job that makes the folder and sets the setting.
    let out = say(&mut e, "script 1a: the projects folder", "Prepare a folder at /data/work where all my projects will live from now on");
    assert!(Path::new(WORK).is_dir(), "{WORK} must exist after the housekeeping job: {out:?}");
    assert_eq!(e.store.get_setting("projects_root").unwrap().as_deref(), Some(WORK), "the setting must point at it: {out:?}");
    // And the setting is not decoration: the next new project lands under it.
    let out = say(&mut e, "script 1b: a project under it", "make me a script that prints the first ten primes, decide yourself");
    let landed = listing(WORK).into_iter().find(|n| Path::new(WORK).join(n).join("BLUEPRINT.md").exists());
    assert!(landed.is_some(), "a project with a BLUEPRINT.md must land under {WORK} (found {:?}): {out:?}", listing(WORK));
    println!("[script 1] project folder: {WORK}/{}", landed.unwrap());

    // 2. The admin hand: apt through the wrapper.
    let out = say(&mut e, "script 2: install cowsay", "Install cowsay and prove it works");
    assert!(installed("cowsay"), "cowsay must be installed: {out:?}");

    // 3. Undo, newest job first. The primes job sits between the install and the housekeeping
    // job, so "undo" may have to be said more than once before the package goes.
    let mut undos = 0;
    while installed("cowsay") {
        undos += 1;
        assert!(undos <= 3, "cowsay is still installed after {} undos", undos - 1);
        let out = say(&mut e, &format!("script 3: undo #{undos}"), "undo");
        assert!(!out.iter().any(|l| l.contains("Nothing left to undo")), "ran out of jobs with cowsay still installed: {out:?}");
    }
    while e.store.get_setting("projects_root").unwrap().is_some() {
        undos += 1;
        assert!(undos <= 6, "projects_root is still set after {} undos", undos - 1);
        let out = say(&mut e, &format!("script 3: undo #{undos}"), "undo");
        assert!(!out.iter().any(|l| l.contains("Nothing left to undo")), "ran out of jobs with projects_root still set: {out:?}");
    }
    // §11: the folder goes "if empty". Undo restores a project's files, it never removes the
    // project folder, so what the primes job left inside can legitimately keep /data/work alive.
    let left = listing(WORK);
    println!("[script 3] {WORK}: exists={} contents={left:?}", Path::new(WORK).exists());
    assert!(!Path::new(WORK).exists() || !left.is_empty(), "an empty {WORK} should have been removed by the make-dir undo");

    // 4. The network, opened for one step: pip through the registry allowlist.
    let out = say(&mut e, "script 4: fetch a pip package", "make a script that prints a 2-column table using the tabulate package, decide yourself");
    let job = latest_job();
    let rows = ActionLog::open(DB).unwrap().rows_for(&job.id).unwrap();
    let fetch = rows.iter().find(|(action, _)| action.contains("fetch_packages"));
    assert!(
        matches!(fetch, Some((_, outcome)) if outcome.starts_with("ok:")),
        "a fetch_packages step must have succeeded in job {} — replies {out:?}, log {rows:?}", job.id,
    );

    println!("[after] /data: {:?}", listing("/data"));
    println!("[after] /data/projects: {:?}", listing("/data/projects"));
}
