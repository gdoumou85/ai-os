//! Project folders as btrfs subvolumes, and the snapshot undo puts back.
//!
//! Everything here runs as the current user — no sudo, no wrapper verb. `btrfs subvolume
//! create`, `snapshot` and `property set` all work unprivileged for the owner, and the kernel
//! lets an owner `rm -rf` their own subvolume (proven on the workshop in the Phase 0 probe).
//!
//! ponytail: shells out to `btrfs`/`chgrp`/`chmod` rather than binding libbtrfsutil — a handful
//! of calls per job; bind the library only if that ever shows up in a profile.
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

fn failed(what: &str, stderr: &[u8]) -> io::Error {
    io::Error::new(io::ErrorKind::Other, format!("{what}: {}", String::from_utf8_lossy(stderr).trim()))
}

/// Make a new project's folder. A btrfs subvolume where that works, a plain folder where it
/// does not (a non-btrfs parent — tests, and any machine that is not the workshop). Returns
/// whether the folder really is a subvolume, i.e. whether this project can be snapshotted.
pub fn create_project_dir(path: &Path) -> io::Result<bool> {
    let subvolume = Command::new("btrfs")
        .args(["subvolume", "create"])
        .arg(path)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !subvolume {
        std::fs::create_dir_all(path)?;
    }
    share_with_sandbox(path);
    Ok(subvolume)
}

/// Hand a folder to the sandbox user: group `ai-sandbox`, mode 2770 so whatever is made inside
/// it stays shared (1b spec §7). A subvolume does not inherit the setgid group from its parent,
/// and the housekeeping folder is created by the engine on machines that never saw the setup
/// script — both front doors go through here. Best effort: in tests there is no such group.
pub fn share_with_sandbox(path: &Path) {
    let _ = Command::new("chgrp").arg("ai-sandbox").arg(path).status();
    let _ = Command::new("chmod").arg("2770").arg(path).status();
}

/// Every btrfs subvolume is inode 256 *and* carries its own anonymous device number, so it
/// differs from the folder it sits in. Inode 256 alone is not enough: a plain folder on tmpfs
/// gets that number by simple sequence, which made a unit test try to snapshot /tmp until the
/// device half of the rule went in. Off btrfs (and off unix) there are no subvolumes at all.
#[cfg(unix)]
pub fn is_subvolume(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(me), Some(parent)) = (std::fs::metadata(path), path.parent()) else { return false };
    let Ok(up) = std::fs::metadata(parent) else { return false };
    me.ino() == 256 && me.dev() != up.dev()
}

#[cfg(not(unix))]
pub fn is_subvolume(_path: &Path) -> bool { false }

/// Make a read-only snapshot of the project as a job starts, named `<project>@<job_id>`.
/// `None` when the project is a plain folder: nothing to snapshot, and the undo report says so.
/// Only the newest snapshot per project is kept — the older ones go first.
///
/// The name is built here rather than passed in, so the prefix the prune matches on and the
/// name the snapshot gets can never drift apart.
pub fn take(folder: &Path, snapshots_dir: &Path, job_id: &str) -> io::Result<Option<PathBuf>> {
    if !is_subvolume(folder) {
        return Ok(None);
    }
    // The workshop's setup makes this folder (ai:ai-sandbox 2770); creating it here keeps a
    // machine that has not been through setup from failing every job start.
    std::fs::create_dir_all(snapshots_dir)?;
    let project = folder.file_name().unwrap_or_default().to_string_lossy().to_string();
    let prefix = format!("{project}@");
    for entry in std::fs::read_dir(snapshots_dir)?.flatten() {
        if entry.file_name().to_string_lossy().starts_with(&prefix) {
            remove_snapshot(&entry.path());
        }
    }
    let dest = snapshots_dir.join(format!("{prefix}{job_id}"));
    let out = Command::new("btrfs").args(["subvolume", "snapshot", "-r"]).arg(folder).arg(&dest).output()?;
    if !out.status.success() {
        return Err(failed("btrfs subvolume snapshot", &out.stderr));
    }
    Ok(Some(dest))
}

/// Drop a snapshot: read-only first has to go, then its owner may remove it like any folder.
/// Best effort — a stale snapshot that will not budge must never stop a job from starting.
fn remove_snapshot(path: &Path) {
    let _ = Command::new("btrfs").args(["property", "set", "-ts"]).arg(path).args(["ro", "false"]).status();
    let _ = std::fs::remove_dir_all(path);
}

/// Put a project's files back to the snapshot taken when the job started: the snapshot *becomes*
/// the project folder, and only the copy the job worked in is removed.
pub fn restore(folder: &Path, snapshot: &Path) -> io::Result<()> {
    let out = Command::new("btrfs").args(["property", "set", "-ts"]).arg(snapshot).args(["ro", "false"]).output()?;
    if !out.status.success() {
        return Err(failed("btrfs property set ro false", &out.stderr));
    }
    swap_in(folder, snapshot)
}

/// The swap itself, with no btrfs in it — which is what lets it be tested on any filesystem.
///
/// Never `remove_dir_all(folder)` on its own, and never before the swap: undo must not be able
/// to leave a project missing. If the second rename fails the original folder goes straight back.
fn swap_in(folder: &Path, snapshot: &Path) -> io::Result<()> {
    let name = folder.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "project folder has no name"))?;
    let old = folder.with_file_name(format!("{}.old", name.to_string_lossy()));
    // A leftover from an earlier restore whose cleanup failed would make the rename below fail
    // (ENOTEMPTY) and block every later undo of this project. Clear it first.
    if old.exists() {
        remove_snapshot(&old);
    }
    std::fs::rename(folder, &old)?;
    if let Err(e) = std::fs::rename(snapshot, folder) {
        if let Err(back) = std::fs::rename(&old, folder) {
            return Err(io::Error::new(io::ErrorKind::Other, format!(
                "could not put the snapshot in place ({e}) and could not put {} back either ({back}) — the project's files are at {}",
                folder.display(), old.display(),
            )));
        }
        return Err(e);
    }
    // Past this line the undo has succeeded: the files are back where the user expects them.
    // A copy left behind is untidy, never a failure — reporting it as one would tell the user
    // "could not undo" about a restore that worked, and mark the row applied besides.
    if let Err(e) = std::fs::remove_dir_all(&old) {
        eprintln!("snapshot: restored {} but could not remove the leftover {}: {e}", folder.display(), old.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("ai-os-snapshot-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        root
    }

    /// The swap has no btrfs in it, so the leftover cases can be proven on any filesystem.
    #[test]
    fn the_swap_clears_a_stale_leftover_and_never_fails_on_the_cleanup() {
        let root = temp("swap");
        let folder = root.join("p");
        let snap = root.join("p@job-1");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::create_dir_all(&snap).unwrap();
        std::fs::write(folder.join("a.txt"), "what the job did").unwrap();
        std::fs::write(snap.join("a.txt"), "what was there before").unwrap();
        // An earlier restore whose cleanup failed: `p.old` is still there, and not empty.
        let old = root.join("p.old");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("junk.txt"), "leftover").unwrap();

        swap_in(&folder, &snap).unwrap();
        assert_eq!(std::fs::read_to_string(folder.join("a.txt")).unwrap(), "what was there before");
        assert!(!old.exists(), "the copy the job worked in is gone, leftover and all");
        assert!(!snap.exists(), "the snapshot became the project folder");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_plain_folder_is_no_subvolume_and_is_never_snapshotted() {
        let root = temp("plain");
        let folder = root.join("p");
        assert!(!create_project_dir(&folder).unwrap(), "a temp folder is not btrfs");
        assert!(folder.is_dir(), "it is still a real folder");
        assert!(!is_subvolume(&folder));
        // No snapshots folder is created either: there is nothing to put in it.
        assert_eq!(take(&folder, &root.join("snapshots"), "job-1").unwrap(), None);
        assert!(!root.join("snapshots").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
