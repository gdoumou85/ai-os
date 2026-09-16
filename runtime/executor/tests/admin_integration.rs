// Gated: only runs inside the distro, as user `ai`, with the root wrapper installed.
//   AI_OS_SANDBOX_IT=1 cargo test -p executor --test admin_integration
// It installs and removes `cowsay` for real.
use executor::action::Action;
use executor::admin::AdminWorker;
use executor::undo::UndoEntry;
use executor::worker::Worker;

fn gated() -> bool { std::env::var("AI_OS_SANDBOX_IT").as_deref() == Ok("1") }

#[test]
fn install_records_added_and_reverse_removes() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let w = AdminWorker;
    let out = w.run(&Action::Install { packages: vec!["cowsay".into()] });
    assert!(out.ok, "{}", out.detail);
    let undo = out.undo.expect("undo entry");
    assert!(matches!(&undo, UndoEntry::PackagesAdded { packages } if packages.contains(&"cowsay".to_string())), "{undo:?}");
    let back = w.reverse(&undo);
    assert!(back.ok, "{}", back.detail);
    // The untruncated list — the same one the undo diff is computed from.
    assert!(!AdminWorker::pkg_list().unwrap().iter().any(|p| p == "cowsay"), "cowsay must be gone");
}

#[test]
fn essential_remove_is_refused_with_reason() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let o = AdminWorker.run(&Action::Remove { packages: vec!["bash".into()] });
    assert!(!o.ok && o.detail.contains("refused"), "{}", o.detail);
    assert_eq!(o.undo, None, "nothing was removed, so nothing to put back");
}

#[test]
fn write_outside_records_previous_and_reverse_restores() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let p = "/data/housekeeping/it-undo.txt";
    let _ = AdminWorker::call("remove-file", &[p], None);
    let o = AdminWorker.run(&Action::WriteFile { path: p.into(), contents: "v1".into() });
    assert!(o.ok, "{}", o.detail);
    assert_eq!(o.undo, Some(UndoEntry::FileBefore { path: p.into(), contents: None }));
    let o2 = AdminWorker.run(&Action::EditFile { path: p.into(), find: "v1".into(), replace: "v2".into() });
    assert!(o2.ok, "{}", o2.detail);
    assert_eq!(o2.undo, Some(UndoEntry::FileBefore { path: p.into(), contents: Some("v1".into()) }));
    assert!(AdminWorker.reverse(&o2.undo.unwrap()).ok);
    assert_eq!(std::fs::read_to_string(p).unwrap(), "v1");
    assert!(AdminWorker.reverse(&o.undo.unwrap()).ok);
    assert!(!std::path::Path::new(p).exists());
}

#[test]
fn read_file_comes_back_windowed() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let p = "/data/housekeeping/it-read.txt";
    assert!(AdminWorker.run(&Action::WriteFile { path: p.into(), contents: "l1\nl2\nl3\n".into() }).ok);
    let out = AdminWorker.run(&Action::ReadFile { path: p.into(), from_line: Some(2), lines: Some(1) });
    assert!(out.ok, "{}", out.detail);
    assert_eq!(out.detail, "2: l2\n");
    assert!(AdminWorker::call("remove-file", &[p], None).ok);
}

#[test]
fn make_dir_then_reverse() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let o = AdminWorker.run(&Action::MakeDir { path: "/data/it-mk".into() });
    assert!(o.ok, "{}", o.detail);
    // A successful step whose detail is empty tells the model nothing, and the live 1c run
    // showed it going looking for proof the sandbox cannot see (§11 Results).
    assert!(o.detail.contains("/data/it-mk"), "the outcome must say what exists: {:?}", o.detail);
    assert!(AdminWorker.reverse(&o.undo.unwrap()).ok);
    assert!(!std::path::Path::new("/data/it-mk").exists());
}

#[test]
fn restart_records_no_undo() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    // `state` on a unit that isn't installed still answers (unknown unknown) — use a real one.
    let before = AdminWorker::call("service", &["cron", "state"], None);
    assert!(before.ok, "{}", before.detail);
    let o = AdminWorker.run(&Action::Service { name: "cron".into(), action: executor::action::ServiceDo::Restart });
    assert!(o.ok, "{}", o.detail);
    assert_eq!(o.undo, None, "a restart changes no state, so there is nothing to put back");
}

#[test]
fn pip_fetch_reaches_registry_and_nothing_else() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = std::path::PathBuf::from("/data/projects/it-fetch");
    std::fs::create_dir_all(&ws).unwrap();
    let _ = std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status();
    let _ = std::process::Command::new("chmod").arg("2770").arg(&ws).status();
    let w = executor::worker::SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let o = w.run(&Action::FetchPackages { manager: executor::action::Manager::Pip, packages: vec!["tabulate".into()] });
    assert!(o.ok, "{}", o.detail);
    assert!(ws.join(".venv/bin/pip").exists());
    // The allowlist is per-call: an ordinary run_command still gets PrivateNetwork.
    let leak = w.run(&Action::RunCommand { argv: vec!["curl".into(), "-sS".into(), "-m".into(), "5".into(), "https://example.com".into()] });
    assert!(!leak.ok, "ordinary run_command must still have no network");
    let _ = std::fs::remove_dir_all(&ws);
}
