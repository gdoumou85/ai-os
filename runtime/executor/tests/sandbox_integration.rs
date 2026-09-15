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
    let out = w.run(&Action::ReadFile { path: "leak".into() });
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
