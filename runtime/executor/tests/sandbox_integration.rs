// Gated: only runs inside the distro with the sandbox user set up.
//   AI_OS_SANDBOX_IT=1 cargo test -p executor --test sandbox_integration
use executor::action::Action;
use executor::worker::{SandboxWorker, Worker};
use std::path::PathBuf;

fn gated() -> bool { std::env::var("AI_OS_SANDBOX_IT").as_deref() == Ok("1") }

#[test]
fn command_runs_as_sandbox_user_in_workspace() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = PathBuf::from("/data/jobs/it1");
    std::fs::create_dir_all(&ws).unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    let out = w.run(&Action::RunCommand { argv: vec!["id".into(), "-un".into()] });
    assert!(out.ok, "detail: {}", out.detail);
    assert!(out.detail.contains("ai-sandbox"), "should run as the sandbox user: {}", out.detail);
}

#[test]
fn network_is_blocked() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = PathBuf::from("/data/jobs/it2");
    std::fs::create_dir_all(&ws).unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    // With PrivateNetwork, even loopback name resolution / connect should fail.
    let out = w.run(&Action::RunCommand { argv: vec!["getent".into(), "hosts".into(), "example.com".into()] });
    assert!(!out.ok, "network must be cut off in the sandbox: {}", out.detail);
}

#[test]
fn symlink_escaping_workspace_is_refused_on_read() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = PathBuf::from("/data/jobs/it3");
    std::fs::create_dir_all(&ws).unwrap();
    // Earlier tests only ever have ai-sandbox read the workspace it's given (default dir
    // perms already grant it r-x as "other"); this one has it write a symlink into the
    // workspace, so it also needs write access here.
    assert!(std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status().unwrap().success());
    assert!(std::process::Command::new("chmod").arg("0770").arg(&ws).status().unwrap().success());
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    // The sandboxed command plants a symlink that is lexically inside the workspace but
    // points at a world-readable file outside it (/etc/hostname — readable by anyone, so a
    // refusal below can only be our own workspace guard, not a permissions error).
    let plant = w.run(&Action::RunCommand {
        argv: vec!["ln".into(), "-sf".into(), "/etc/hostname".into(), "leak".into()],
    });
    assert!(plant.ok, "failed to plant the symlink: {}", plant.detail);
    let out = w.run(&Action::ReadFile { path: "leak".into(), from_line: None, lines: None });
    assert!(!out.ok, "a symlink out of the workspace must be refused, not followed: {}", out.detail);
}

#[test]
fn symlink_escaping_workspace_is_refused_on_write() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = PathBuf::from("/data/jobs/it4");
    std::fs::create_dir_all(&ws).unwrap();
    assert!(std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status().unwrap().success());
    assert!(std::process::Command::new("chmod").arg("0770").arg(&ws).status().unwrap().success());

    // A pre-populated, world-writable target outside the workspace: if the write below slipped
    // through the symlink, it would land here, and the assertion at the end is unambiguous either
    // way (not a permissions error masking the result).
    let outside = PathBuf::from("/tmp/ai-os-it4-outside-target");
    std::fs::write(&outside, "original").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o666)).unwrap();
    }

    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws };
    // Sandboxed command plants a symlink whose leaf ("leak") is lexically inside the workspace
    // but already exists as a link to something outside it.
    let plant = w.run(&Action::RunCommand {
        argv: vec!["ln".into(), "-sf".into(), outside.display().to_string(), "leak".into()],
    });
    assert!(plant.ok, "failed to plant the symlink: {}", plant.detail);

    let out = w.run(&Action::WriteFile { path: "leak".into(), contents: "pwned".into() });
    assert!(!out.ok, "a write through a symlink leaf must be refused, not followed: {}", out.detail);

    let contents = std::fs::read_to_string(&outside).unwrap();
    assert_eq!(contents, "original", "outside target must be untouched by the refused write");

    let _ = std::fs::remove_file(&outside);
}

fn project(name: &str) -> PathBuf {
    let ws = PathBuf::from("/data/projects").join(name);
    std::fs::create_dir_all(&ws).unwrap();
    assert!(std::process::Command::new("chgrp").arg("ai-sandbox").arg(&ws).status().unwrap().success());
    assert!(std::process::Command::new("chmod").arg("2770").arg(&ws).status().unwrap().success());
    ws
}

#[test]
fn other_projects_and_the_database_are_invisible() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let mine = project("it-jail-a");
    let other = project("it-jail-b");
    std::fs::write(other.join("secret.txt"), "other project").unwrap();
    std::fs::write("/data/it-jail-db.sqlite", "pretend db").unwrap();
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: mine };
    let peek = w.run(&Action::RunCommand { argv: vec!["cat".into(), "/data/projects/it-jail-b/secret.txt".into()] });
    assert!(!peek.ok, "another project must be invisible: {}", peek.detail);
    let db = w.run(&Action::RunCommand { argv: vec!["cat".into(), "/data/it-jail-db.sqlite".into()] });
    assert!(!db.ok, "the executor's database must be invisible: {}", db.detail);
    let home = w.run(&Action::RunCommand { argv: vec!["ls".into(), "/home/ai".into()] });
    assert!(!home.ok, "the user's home must be hidden: {}", home.detail);
    // C1: /mnt must be hidden too, not just ProtectHome/ProtectSystem's own targets — on this
    // WSL2 dev workshop it's where the whole Windows profile lives. Checked one level down
    // (`/mnt/c/Users`, not bare `/mnt`): WSL mounts the drive at `/mnt/c`, so a bare `ls /mnt`
    // only ever lists mount names ("c", "wsl", …) and would pass either way — it never actually
    // exercises the leak. "!out.detail.contains(\"c\")" would also be too weak (matches almost
    // anything); assert on the real Windows-profile marker instead. Either assertion path covers
    // it: the path is refused outright, or it "succeeds" but the failure text (which echoes the
    // refused path back) is all there is — no directory contents ever come through.
    let mnt = w.run(&Action::RunCommand { argv: vec!["ls".into(), "/mnt/c/Users".into()] });
    assert!(!mnt.ok || !mnt.detail.contains("gdoum"), "/mnt must be hidden from the sandbox: {}", mnt.detail);
    let _ = std::fs::remove_file("/data/it-jail-db.sqlite");
    let _ = std::fs::remove_dir_all("/data/projects/it-jail-a");
    let _ = std::fs::remove_dir_all("/data/projects/it-jail-b");
}

#[test]
fn workspace_is_still_writable_and_tools_still_run() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = project("it-jail-c");
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let out = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "echo hi > out.txt && python3 -c 'print(6*7)'".into()] });
    assert!(out.ok, "workspace must stay writable and system programs runnable: {}", out.detail);
    assert!(out.detail.contains("42"));
    assert_eq!(std::fs::read_to_string(ws.join("out.txt")).unwrap().trim(), "hi");
    let _ = std::fs::remove_dir_all(&ws);
}

#[test]
fn failure_detail_carries_the_end_of_the_error_output() {
    if !gated() { eprintln!("skipped: set AI_OS_SANDBOX_IT=1 inside the distro"); return; }
    let ws = project("it-jail-d");
    let w = SandboxWorker { user: "ai-sandbox".into(), workspace: ws.clone() };
    let out = w.run(&Action::RunCommand { argv: vec!["sh".into(), "-c".into(), "seq 1 2000 >&2; echo THE-REAL-REASON >&2; exit 3".into()] });
    assert!(!out.ok);
    assert!(out.detail.contains("exit 3"), "{}", out.detail);
    assert!(out.detail.contains("THE-REAL-REASON"), "the reason lives at the END of the output: {}", out.detail);
    let _ = std::fs::remove_dir_all(&ws);
}
