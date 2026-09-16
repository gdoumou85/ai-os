// The real thing, on the real filesystem: a project folder that is a btrfs subvolume, a
// read-only snapshot of it, and a restore that puts a deleted file back — all as user `ai`,
// no sudo anywhere. Run inside the distro:
//   AI_OS_SANDBOX_IT=1 cargo test -p aios-core --test snapshot_it
use aios_core::snapshot;
use std::path::{Path, PathBuf};

/// The writable-then-remove trick `snapshot::take` uses for stale snapshots: an owner may
/// `rm -rf` their own subvolume, but not while it is read-only.
fn scrub(path: &Path) {
    if !path.exists() { return; }
    let _ = std::process::Command::new("btrfs").args(["property", "set", "-ts"]).arg(path).args(["ro", "false"]).status();
    let _ = std::fs::remove_dir_all(path);
}

#[test]
fn a_project_subvolume_snapshots_and_restores_as_the_ai_user() {
    if std::env::var("AI_OS_SANDBOX_IT").as_deref() != Ok("1") { eprintln!("skipped: AI_OS_SANDBOX_IT=1"); return; }
    let folder = PathBuf::from("/data/projects/it-snap");
    let snapshots = PathBuf::from("/data/snapshots");
    scrub(&folder);
    scrub(&folder.with_file_name("it-snap.old"));
    for e in std::fs::read_dir(&snapshots).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().starts_with("it-snap@") { scrub(&e.path()); }
    }

    assert!(snapshot::create_project_dir(&folder).unwrap(), "a project under /data must land on btrfs as a subvolume");
    assert!(snapshot::is_subvolume(&folder), "inode 256 is what tells a subvolume from a plain folder");
    std::fs::write(folder.join("a.txt"), "the work").unwrap();

    let snap = snapshot::take(&folder, &snapshots, "it-snap@job-1").unwrap().expect("a subvolume project is snapshotted");
    assert!(snap.exists(), "{snap:?}");

    std::fs::remove_file(folder.join("a.txt")).unwrap();
    assert!(!folder.join("a.txt").exists());

    snapshot::restore(&folder, &snap).unwrap();
    assert_eq!(std::fs::read_to_string(folder.join("a.txt")).unwrap(), "the work", "restore puts the job's files back");
    assert!(!snap.exists(), "the snapshot became the project folder");
    assert!(!folder.with_file_name("it-snap.old").exists(), "the folder the job worked in is gone");

    // The group-shared setgid mode survives the snapshot/swap — the sandbox user still gets in.
    let mode = std::process::Command::new("stat").args(["-c", "%a"]).arg(&folder).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&mode.stdout).trim(), "2770");

    scrub(&folder);
    assert!(!folder.exists());
}
