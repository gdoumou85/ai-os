//! What it takes to put one action back. Recorded on the `Outcome` that did it,
//! and handed to `Worker::reverse` to actually undo it.
use serde::{Deserialize, Serialize};

/// The state an action replaced — enough to put it back, nothing more.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum UndoEntry {
    PackagesAdded { packages: Vec<String> },
    PackagesRemoved { packages: Vec<String> },
    ServiceState { name: String, was_enabled: bool, was_active: bool },
    /// `None` = the file did not exist before, so undoing means removing it.
    FileBefore { path: String, contents: Option<String> },
    DirCreated { path: String },
    Setting { key: String, previous: Option<String> },
    ProjectSnapshot { folder: String, snapshot: String },
}

impl UndoEntry {
    /// One plain-English line for the user: what undoing this would do.
    pub fn describe(&self) -> String {
        match self {
            UndoEntry::PackagesAdded { packages } => {
                format!("Removed the {} packages installed ({})", packages.len(), packages.join(", "))
            }
            UndoEntry::PackagesRemoved { packages } => {
                format!("Reinstalled the {} packages removed ({})", packages.len(), packages.join(", "))
            }
            UndoEntry::ServiceState { name, was_enabled, was_active } => format!(
                "Put service {name} back to {} and {}",
                if *was_enabled { "enabled" } else { "disabled" },
                if *was_active { "running" } else { "stopped" },
            ),
            UndoEntry::FileBefore { path, contents } => match contents {
                Some(_) => format!("Put {path} back as it was"),
                None => format!("Undo removed {path} (it did not exist before)"),
            },
            UndoEntry::DirCreated { path } => format!("Removed the folder {path}"),
            UndoEntry::Setting { key, previous } => match previous {
                Some(p) => format!("Setting {key} back to {p}"),
                None => format!("Cleared setting {key}"),
            },
            UndoEntry::ProjectSnapshot { folder, .. } => {
                format!("Restored the files of {folder} from before the job")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_round_trip_and_describe() {
        let e = UndoEntry::PackagesAdded { packages: vec!["cowsay".into(), "libx".into()] };
        let s = serde_json::to_string(&e).unwrap();
        assert_eq!(e, serde_json::from_str(&s).unwrap());
        assert!(e.describe().contains("2 packages") && e.describe().contains("cowsay"), "{}", e.describe());
        assert!(UndoEntry::FileBefore { path: "/etc/x".into(), contents: None }.describe().contains("removed /etc/x"));
    }

    #[test]
    fn every_kind_describes_itself() {
        let all = [
            UndoEntry::PackagesRemoved { packages: vec!["cowsay".into()] },
            UndoEntry::ServiceState { name: "cron".into(), was_enabled: true, was_active: false },
            UndoEntry::FileBefore { path: "/etc/x".into(), contents: Some("old".into()) },
            UndoEntry::DirCreated { path: "/data/x".into() },
            UndoEntry::Setting { key: "projects_root".into(), previous: Some("/data/work".into()) },
            UndoEntry::Setting { key: "projects_root".into(), previous: None },
            UndoEntry::ProjectSnapshot { folder: "/data/projects/p".into(), snapshot: "snap-1".into() },
        ];
        for e in all {
            let s = e.describe();
            assert!(!s.is_empty(), "{e:?}");
            assert_eq!(e, serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap());
        }
        assert_eq!(
            UndoEntry::ServiceState { name: "cron".into(), was_enabled: true, was_active: false }.describe(),
            "Put service cron back to enabled and stopped"
        );
    }
}
