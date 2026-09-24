//! The projects the AI knows (one-loop design §2; sidebar design §3): the folders under the
//! projects root, found by their BLUEPRINT.md, and folders registered elsewhere that still exist.
//! The engine pins them in every prompt; the service lists them for the rail's Projects page.
use crate::store::{ProjectRow, Store, StoreError};
use std::path::{Path, PathBuf};

/// Where new projects go when the user never said: `AI_OS_PROJECTS`, else /data/projects.
pub fn default_root() -> PathBuf {
    PathBuf::from(std::env::var("AI_OS_PROJECTS").unwrap_or_else(|_| "/data/projects".into()))
}

/// The projects root: the `projects_root` setting, else `default`. A store error is passed on,
/// not read as "unset": that would put a project under the wrong root (I3's lesson).
pub fn root(store: &Store, default: &Path) -> Result<PathBuf, StoreError> {
    Ok(store.get_setting("projects_root")?.map(PathBuf::from).unwrap_or_else(|| default.to_path_buf()))
}

/// A folder's folders, hidden ones left out.
fn subfolders(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().map(|d| d.path())
        .filter(|p| p.is_dir() && !p.file_name().is_some_and(|f| f.to_string_lossy().starts_with('.'))).collect()).unwrap_or_default()
}

/// The projects in a folder of projects, `depth` levels of groups down at most: each folder in it
/// with a BLUEPRINT.md, or with none anywhere below it. A folder with some below is a group
/// ("WEb Games"), looked into the same way, so a sibling with no notes yet still counts.
fn projects_in(dir: &Path, depth: u32) -> Vec<PathBuf> {
    subfolders(dir).into_iter().flat_map(|d| {
        if depth == 0 || d.join("BLUEPRINT.md").is_file() { return vec![d] }
        let inner = projects_in(&d, depth - 1);
        if inner.iter().any(|p| p.join("BLUEPRINT.md").is_file()) { inner } else { vec![d] }
    }).collect()
}

/// Every project: under the root, sorted by name, then those registered elsewhere that still
/// exist. The owner's "WEb Games/Pool Game" read as a project called "WEb Games" (2026-09-24).
pub fn list(store: &Store, default: &Path) -> Result<Vec<ProjectRow>, StoreError> {
    let root = root(store, default)?;
    let mut v: Vec<ProjectRow> = projects_in(&root, 2).iter().map(|p| ProjectRow {
        name: p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(),
        folder: p.display().to_string(), description: String::new(), touched_at: 0 }).collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    for r in store.list_projects()? {
        if Path::new(&r.folder).is_dir() && !Path::new(&r.folder).starts_with(&root) && v.iter().all(|p| p.folder != r.folder) { v.push(r); }
    }
    Ok(v)
}

/// The first line of a project's notes that says something, without its heading marks, cut short.
pub fn first_line(notes: &str) -> String {
    notes.lines().map(|l| l.trim().trim_start_matches('#').trim()).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect()
}

/// The projects as the Projects page shows them, the most recently changed first.
pub fn views(store: &Store, default: &Path) -> Result<Vec<aios_proto::ProjectView>, StoreError> {
    let changed = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_secs() as i64);
    let mut v: Vec<aios_proto::ProjectView> = list(store, default)?.into_iter().map(|p| {
        let notes = Path::new(&p.folder).join("BLUEPRINT.md");
        aios_proto::ProjectView {
            summary: std::fs::read_to_string(&notes).map(|t| first_line(&t)).unwrap_or_default(),
            touched_at: changed(Path::new(&p.folder)).max(changed(&notes)),
            name: p.name, folder: p.folder,
        }
    }).collect();
    v.sort_by(|a, b| b.touched_at.cmp(&a.touched_at));
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("aios-projects-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn a_group_folder_holds_projects_with_and_without_notes() {
        let root = tmp("group");
        std::fs::create_dir_all(root.join("WEb Games/Pool Game")).unwrap();
        std::fs::write(root.join("WEb Games/Pool Game/BLUEPRINT.md"), "# Pool Game\nA pool table").unwrap();
        std::fs::create_dir_all(root.join("WEb Games/Chess")).unwrap();
        std::fs::create_dir_all(root.join("loose/src")).unwrap();
        let names: Vec<String> = list(&Store::open_in_memory().unwrap(), &root).unwrap().into_iter().map(|p| p.name).collect();
        assert_eq!(names, ["Chess", "Pool Game", "loose"]);
    }

    #[test]
    fn the_first_line_that_says_something() {
        assert_eq!(first_line("\n# Pool Game\nA pool table"), "Pool Game");
        assert_eq!(first_line(""), "");
        assert_eq!(first_line(&"x".repeat(300)).len(), 120);
    }

    #[test]
    fn the_page_shows_each_project_with_its_first_line() {
        let root = tmp("views");
        std::fs::create_dir_all(root.join("game")).unwrap();
        std::fs::write(root.join("game/BLUEPRINT.md"), "A pool table in the browser").unwrap();
        let v = views(&Store::open_in_memory().unwrap(), &root).unwrap();
        assert_eq!((v[0].name.as_str(), v[0].summary.as_str()), ("game", "A pool table in the browser"));
        assert!(v[0].touched_at > 0);
    }
}
