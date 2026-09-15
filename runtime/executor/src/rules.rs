use crate::action::Action;
use std::path::{Path, PathBuf};

/// The risky-actions verdict. `Auto` runs without asking; `NeedsConfirm` must be
/// approved by the user first. A pure function of the typed action (decision 9):
/// the model is never consulted.
#[derive(Debug, Clone, PartialEq)]
pub enum Risk {
    Auto,
    NeedsConfirm(String),
}

/// Resolve a possibly-relative path against the workspace, without touching disk.
/// `pub(crate)`: also used by `worker::SandboxWorker` as a defense-in-depth check
/// before it touches the filesystem.
pub(crate) fn resolves_inside(path: &str, workspace: &Path) -> bool {
    let p = Path::new(path);
    let joined: PathBuf = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
    // Reject any `..` escape by normalizing lexically.
    let mut norm = PathBuf::new();
    for c in joined.components() {
        use std::path::Component::*;
        match c {
            ParentDir => { if !norm.pop() { return false; } }
            CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    norm.starts_with(workspace)
}

pub fn classify(action: &Action, workspace: &Path) -> Risk {
    match action {
        // Auto because it's unprivileged and network-isolated (cannot exfiltrate), BUT it is
        // not yet filesystem-confined to the workspace — an unprivileged, cwd-locked,
        // no-network, ProtectHome sandbox can still read/write ai-sandbox-accessible files
        // outside the workspace (e.g. `cat /etc/passwd`). Confining it (mount namespace /
        // InaccessiblePaths) is a prerequisite before the model/secrets phase.
        Action::RunCommand { .. } => Risk::Auto,
        // Reading inside the workspace is harmless; outside is a privacy/secrets leak.
        Action::ReadFile { path } => {
            if resolves_inside(path, workspace) {
                Risk::Auto
            } else {
                Risk::NeedsConfirm(format!("reads outside the workspace: {path}"))
            }
        }
        // Writing inside the workspace is reversible; outside is "destroys work" territory.
        Action::WriteFile { path, .. } => {
            if resolves_inside(path, workspace) {
                Risk::Auto
            } else {
                Risk::NeedsConfirm(format!("writes outside the workspace: {path}"))
            }
        }
        // Leaves the machine → always ask (a snapshot cannot bring it back).
        Action::HttpPost { url, .. } => Risk::NeedsConfirm(format!("sends data off the machine to {url}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws() -> PathBuf { PathBuf::from("/data/jobs/j1") }

    #[test]
    fn run_command_is_auto() {
        assert_eq!(classify(&Action::RunCommand { argv: vec!["ls".into()] }, &ws()), Risk::Auto);
    }

    #[test]
    fn write_inside_workspace_is_auto() {
        let a = Action::WriteFile { path: "out.txt".into(), contents: "x".into() };
        assert_eq!(classify(&a, &ws()), Risk::Auto);
    }

    #[test]
    fn read_inside_workspace_is_auto() {
        let a = Action::ReadFile { path: "out.txt".into() };
        assert_eq!(classify(&a, &ws()), Risk::Auto);
    }

    #[test]
    fn read_outside_workspace_needs_confirm() {
        let a = Action::ReadFile { path: "/etc/passwd".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn write_outside_workspace_needs_confirm() {
        let a = Action::WriteFile { path: "/etc/passwd".into(), contents: "x".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn parent_dir_escape_needs_confirm() {
        let a = Action::WriteFile { path: "../../etc/x".into(), contents: "x".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn http_post_needs_confirm() {
        let a = Action::HttpPost { url: "https://x".into(), body: "b".into() };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }
}
