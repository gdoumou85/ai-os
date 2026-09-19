// The Phase 1c acceptance test (1c design §11): the real local model, both hands (sandbox and
// the root wrapper), four scripts in order, no human. Run inside the distro with Ollama up:
//   AI_OS_LIVE=1 cargo test -p aios-core --test live_1c -- --nocapture
use aios_core::engine::Engine;
use aios_core::job::{Job, State};
use aios_core::model::{Model, RemoteModel};
use aios_core::store::Store;
use executor::admin::AdminWorker;
use executor::atspi::{DesktopState, DesktopWorker};
use executor::log::ActionLog;
use executor::worker::{SandboxWorker, Worker};
use std::path::{Path, PathBuf};
use std::time::Instant;

const DB: &str = "/data/ai-os-live-1c.db";
const WORK: &str = "/data/work";

fn engine() -> Engine<RemoteModel> {
    let store = Store::open(DB).unwrap();
    let llm = RemoteModel::local("qwen3.5:9b");
    let desktop = DesktopState::for_model(llm.context_tokens());
    Engine::new(store, llm, PathBuf::from("/data/projects"), Some(DB.into()),
        Box::new(move |ws| (
            Box::new(SandboxWorker { user: "ai-sandbox".into(), workspace: ws.to_path_buf() }) as Box<dyn Worker>,
            Box::new(AdminWorker) as Box<dyn Worker>,
            Box::new(DesktopWorker(desktop.clone())) as Box<dyn Worker>,
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

/// Sorted, so two listings differ only when their contents really do.
fn listing(dir: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect())
        .unwrap_or_default();
    names.sort();
    names
}

/// Remove something the run must start without. Only "it was not there" is acceptable: a path
/// left behind because it could not be deleted would make the whole acceptance meaningless.
fn must_clear(path: &str, dir: bool) {
    let r = if dir { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) };
    if let Err(e) = r {
        assert_eq!(e.kind(), std::io::ErrorKind::NotFound, "could not clear {path}: {e}");
    }
}

/// One line of the conversation: say it, print every reply, and — for a model that asks a
/// question anyway after being told to decide — answer the way the user already has, a few times
/// at most. Every nudge is printed, so the transcript shows exactly how much human the run
/// needed. An *approval* is never given: these four scripts are all Auto work, so a job waiting
/// for a yes means the model reached outside what the script is about, and that is a failure to
/// report, not something to wave through. Prints the step count and the seconds the line took,
/// which is what §11 wants recorded per script.
fn say(e: &mut Engine<RemoteModel>, label: &str, text: &str) -> Vec<String> {
    let t = Instant::now();
    println!("you> {text}");
    let mut out = e.handle(text).unwrap();
    for l in &out { println!("ai> {l}"); }
    for _ in 0..4 {
        let Some(job) = e.open_job().unwrap() else { break };
        match job.state {
            State::WaitingAnswer => {}
            State::WaitingApproval => panic!(
                "the acceptance needed an approval: {} for {:?}", job.pending_reason, job.pending_action,
            ),
            _ => break,
        }
        println!("you> You decide.");
        out = e.handle("You decide.").unwrap();
        for l in &out { println!("ai> {l}"); }
    }
    println!("[{label}] {} steps, {:.1}s", latest_job().steps.len(), t.elapsed().as_secs_f64());
    out
}

#[test]
fn the_machine_moves_its_projects_installs_undoes_and_fetches() {
    if std::env::var("AI_OS_LIVE").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_LIVE=1"); return; }
    must_clear(DB, false);
    must_clear(WORK, true);
    assert!(!Path::new(WORK).exists(), "{WORK} must not exist when the run starts");
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
    let landed = landed.unwrap_or_else(|| panic!("a project with a BLUEPRINT.md must land under {WORK} (found {:?}): {out:?}", listing(WORK)));
    let project = Path::new(WORK).join(&landed);
    println!("[script 1] project folder: {}", project.display());
    // A file of the test's own, to prove later that an undo really moved files rather than only
    // printing a line. Which snapshot the restore puts back depends on what the model does next
    // (the live run has had it install cowsay *inside* this project, which takes a second
    // snapshot and prunes the first — only the newest per project is kept, §12), so the proof
    // cannot assume one or the other: it is that the folder's contents change.
    let marker = project.join("UNDO_MARKER.txt");
    std::fs::write(&marker, "written after the primes job, before any undo").unwrap();
    assert!(marker.exists());

    // 2. The admin hand: apt through the wrapper.
    let out = say(&mut e, "script 2: install cowsay", "Install cowsay and prove it works");
    assert!(installed("cowsay"), "cowsay must be installed: {out:?}");

    // 3. Undo, newest job first. The primes job sits between the install and the housekeeping
    // job, so "undo" may have to be said more than once before the package goes.
    // The marker goes off disk now, so whichever snapshot the restore puts back, the folder must
    // come out different from what it is at this moment: the snapshot taken when the last job in
    // this project started has the marker in it (it comes back), and an older one predates the
    // project's files entirely (they go). Either way "Restored the files of" has to be a real
    // move of files, which is what this proves.
    std::fs::remove_file(&marker).unwrap();
    let before_undo = listing(&project.display().to_string());
    println!("[script 3] {} before the undo: {before_undo:?}", project.display());
    let mut undos = 0;
    let mut restored = false;
    let undo = |e: &mut Engine<RemoteModel>, undos: &mut i32, restored: &mut bool| {
        *undos += 1;
        let out = say(e, &format!("script 3: undo #{undos}"), "undo");
        assert!(!out.iter().any(|l| l.contains("Nothing left to undo")), "ran out of jobs to undo: {out:?}");
        // "Could not undo: Restored the files of …" is the failure of exactly this, so the
        // line has to be the report of a restore that happened, not of one that did not.
        if out.iter().any(|l| l.contains("Restored the files of") && !l.starts_with("Could not undo")) && !*restored {
            let now = listing(&project.display().to_string());
            println!("[script 3] {} after the restore: {now:?}", project.display());
            assert_ne!(now, before_undo, "the files were reported restored but nothing on disk moved");
            *restored = true;
        }
    };
    while installed("cowsay") {
        assert!(undos < 3, "cowsay is still installed after {undos} undos");
        undo(&mut e, &mut undos, &mut restored);
    }
    while e.store.get_setting("projects_root").unwrap().is_some() {
        assert!(undos < 6, "projects_root is still set after {undos} undos");
        undo(&mut e, &mut undos, &mut restored);
    }
    assert!(restored, "no undo put the project's files back");
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
