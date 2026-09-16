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
    // Shared with the sandbox user (1b spec §7) — and a subvolume does not inherit the setgid
    // group from its parent, so this matters more here than it did for a plain folder.
    // Best effort: in tests there is no such group.
    let _ = Command::new("chgrp").arg("ai-sandbox").arg(path).status();
    let _ = Command::new("chmod").arg("2770").arg(path).status();
    Ok(subvolume)
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

/// Make a read-only snapshot of the project as a job starts, named `<project>@<job>`.
/// `None` when the project is a plain folder: nothing to snapshot, and the undo report says so.
/// Only the newest snapshot per project is kept — the older ones go first.
pub fn take(folder: &Path, snapshots_dir: &Path, name: &str) -> io::Result<Option<PathBuf>> {
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
    let dest = snapshots_dir.join(name);
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
///
/// Never `remove_dir_all(folder)` on its own, and never before the swap: undo must not be able
/// to leave a project missing. If the swap half-fails the original folder goes straight back.
pub fn restore(folder: &Path, snapshot: &Path) -> io::Result<()> {
    let out = Command::new("btrfs").args(["property", "set", "-ts"]).arg(snapshot).args(["ro", "false"]).output()?;
    if !out.status.success() {
        return Err(failed("btrfs property set ro false", &out.stderr));
    }
    let name = folder.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "project folder has no name"))?;
    let old = folder.with_file_name(format!("{}.old", name.to_string_lossy()));
    std::fs::rename(folder, &old)?;
    if let Err(e) = std::fs::rename(snapshot, folder) {
        let _ = std::fs::rename(&old, folder);
        return Err(e);
    }
    std::fs::remove_dir_all(&old)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_folder_is_no_subvolume_and_is_never_snapshotted() {
        let root = std::env::temp_dir().join(format!("ai-os-snapshot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let folder = root.join("p");
        assert!(!create_project_dir(&folder).unwrap(), "a temp folder is not btrfs");
        assert!(folder.is_dir(), "it is still a real folder");
        assert!(!is_subvolume(&folder));
        // No snapshots folder is created either: there is nothing to put in it.
        assert_eq!(take(&folder, &root.join("snapshots"), "p@job-1").unwrap(), None);
        assert!(!root.join("snapshots").exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
