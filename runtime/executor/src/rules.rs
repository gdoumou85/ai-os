use crate::action::{valid_name, Action};
use std::path::{Path, PathBuf};

/// The risky-actions verdict. `Auto` runs without asking; `NeedsConfirm` must be
/// approved by the user first. A pure function of the typed action (decision 9):
/// the model is never consulted.
#[derive(Debug, Clone, PartialEq)]
pub enum Risk {
    Auto,
    NeedsConfirm(String),
}

/// Lexically resolve `..`/`.` components without touching disk. `None` if a `..`
/// would escape past the root (e.g. `/../x`).
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut norm = PathBuf::new();
    for c in path.components() {
        use std::path::Component::*;
        match c {
            ParentDir => {
                if !norm.pop() {
                    return None;
                }
            }
            CurDir => {}
            other => norm.push(other.as_os_str()),
        }
    }
    Some(norm)
}

/// Resolve a possibly-relative path against the workspace, without touching disk.
/// `pub(crate)`: also used by `worker::SandboxWorker` as a defense-in-depth check
/// before it touches the filesystem.
pub(crate) fn resolves_inside(path: &str, workspace: &Path) -> bool {
    let p = Path::new(path);
    let joined: PathBuf = if p.is_absolute() { p.to_path_buf() } else { workspace.join(p) };
    match normalize(&joined) {
        Some(norm) => norm.starts_with(workspace),
        None => false,
    }
}

/// AI-writable roots outside the per-job workspace — `make_dir` may create inside these.
pub const AI_ROOTS: [&str; 2] = ["/data", "/home/ai"];

/// Whether an absolute path normalizes under one of `roots` (lexical, `..`-safe, like
/// `resolves_inside`). Relative paths are always rejected — there is no "under" without
/// an absolute path to check.
pub fn under_any(path: &str, roots: &[&str]) -> bool {
    let p = Path::new(path);
    if !p.is_absolute() {
        return false;
    }
    match normalize(p) {
        Some(norm) => roots.iter().any(|r| norm.starts_with(Path::new(r))),
        None => false,
    }
}

/// A package manager asked to *change* something, sent as a free command. The sandbox has no
/// privilege and no network, so it fails with "permission denied" or "read-only file system" —
/// which the live 1c run showed a 9B reads as "this machine has no root", after which it plans
/// around a machine that does not exist instead of using the hand that installs. Refused here,
/// with the hand that does the job named in the reason. Read-only uses of the same programs
/// (`apt list --installed`, `cargo build`, `npm test`) stay free: only the verbs that change
/// something are caught.
pub fn wrong_hand(argv: &[String]) -> Option<String> {
    // `sudo apt install …` and `env apt-get install …` are the same request wearing a hat: step
    // over the launcher and its own options and judge what it was going to run.
    let mut rest = argv.iter().map(String::as_str);
    let mut program = rest.next()?.rsplit('/').next()?;
    while matches!(program, "sudo" | "env") {
        match rest.find(|a| !a.starts_with('-') && !a.contains('=')) {
            Some(next) => program = next.rsplit('/').next()?,
            None => return None,
        }
    }
    let verbs: Vec<&str> = rest.filter(|a| !a.starts_with('-')).collect();
    let has = |vs: &[&str]| verbs.iter().any(|v| vs.contains(v));
    let system = ["apt", "apt-get", "aptitude", "dpkg", "snap", "flatpak"];
    let language = ["pip", "pip3", "npm", "yarn", "pnpm", "cargo"];
    // `python -m pip install …` is pip by another name.
    let program = if program.starts_with("python") && verbs.first() == Some(&"pip") { "pip" } else { program };
    if system.contains(&program) && (has(&["install", "remove", "purge", "upgrade", "dist-upgrade", "full-upgrade", "reinstall", "update"]) || argv.iter().any(|a| a == "-i" || a == "--install")) {
        return Some("software is installed with the `install` action and removed with `remove`, never with run_command: the sandbox has no privileges".into());
    }
    // Same lesson, the other hand: `mkdir /data/work` in the jail answers "Read-only file
    // system", which the live run showed a 9B reads as "this machine cannot make folders at
    // all". An absolute path means outside the workspace — inside it, paths are relative.
    // Removing one is not the mirror image: there is no hand for it, and saying `make_dir`
    // here would send the model looking for an action that does the opposite of what it wants.
    if matches!(program, "mkdir" | "rmdir") && verbs.iter().any(|v| v.starts_with('/')) {
        return Some(if program == "mkdir" {
            "a folder outside the working directory is made with the `make_dir` action, never with run_command: the sandbox can only write inside its own folder"
        } else {
            "there is no hand that removes a folder outside the project; if it was created by a job, say undo"
        }.to_string());
    }
    if language.contains(&program) && has(&["install", "add", "ci"]) {
        return Some("language packages come through `fetch_packages` (name the manager and the packages), never with run_command: the sandbox has no network".into());
    }
    None
}

fn names_ok(ns: &[String]) -> Risk {
    match ns.iter().find(|n| !valid_name(n)) {
        None if !ns.is_empty() => Risk::Auto,
        Some(n) => Risk::NeedsConfirm(format!("invalid name: {n}")),
        None => Risk::NeedsConfirm("no names given".into()),
    }
}

pub fn classify(action: &Action, workspace: &Path) -> Risk {
    match action {
        // Auto: unprivileged, network-isolated, and (since Phase 1b) filesystem-jailed to the
        // project folder — system programs read-only, nothing else visible. See worker.rs.
        Action::RunCommand { .. } => Risk::Auto,
        // Reading inside the workspace is harmless; so is reading system config under /etc
        // (world-readable already). Outside either is a privacy/secrets leak.
        Action::ReadFile { path, .. } => {
            if resolves_inside(path, workspace) || under_any(path, &["/etc"]) {
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
        // Same as WriteFile: inside the workspace is reversible, outside is "destroys work".
        Action::EditFile { path, .. } => {
            if resolves_inside(path, workspace) {
                Risk::Auto
            } else {
                Risk::NeedsConfirm(format!("edits outside the workspace: {path}"))
            }
        }
        // Leaves the machine → always ask (a snapshot cannot bring it back).
        Action::HttpPost { url, .. } => Risk::NeedsConfirm(format!("sends data off the machine to {url}")),
        // Installing/removing/(re)starting a service is reversible and unprivileged via the
        // package manager / systemd — auto, gated only on the name being safe to interpolate.
        Action::Install { packages } | Action::Remove { packages } => names_ok(packages),
        Action::Service { name, .. } => names_ok(std::slice::from_ref(name)),
        Action::FetchPackages { packages, .. } => names_ok(packages),
        Action::MakeDir { path } => {
            if under_any(path, &AI_ROOTS) {
                Risk::Auto
            } else {
                Risk::NeedsConfirm(format!("creates a folder outside the AI's areas: {path}"))
            }
        }
        // A setting is a DB row, not a filesystem/process change — always reversible.
        Action::SetSetting { .. } => Risk::Auto,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Manager, ServiceDo};

    fn ws() -> PathBuf { PathBuf::from("/data/jobs/j1") }

    #[test]
    fn run_command_is_auto() {
        assert_eq!(classify(&Action::RunCommand { argv: vec!["ls".into()] }, &ws()), Risk::Auto);
    }

    #[test]
    fn a_package_manager_asked_to_change_something_is_sent_to_the_right_hand() {
        let argv = |s: &str| s.split(' ').map(String::from).collect::<Vec<_>>();
        for line in [
            "apt-get install -y cowsay", "apt install cowsay", "dpkg -i x.deb", "dpkg --install x.deb",
            "/usr/bin/apt-get remove cowsay", "snap install foo", "apt-get update",
            // The launcher does not launder it.
            "sudo apt install cowsay", "env apt-get install x", "env DEBIAN_FRONTEND=noninteractive apt-get install x",
            "sudo -n /usr/bin/apt-get install x",
        ] {
            assert!(wrong_hand(&argv(line)).unwrap().contains("`install` action"), "{line}");
        }
        for line in ["pip install tabulate", "python3 -m pip install tabulate", "npm install left-pad", "cargo add serde"] {
            assert!(wrong_hand(&argv(line)).unwrap().contains("fetch_packages"), "{line}");
        }
        for line in ["mkdir -p /data/work", "sudo mkdir /data/work"] {
            assert!(wrong_hand(&argv(line)).unwrap().contains("`make_dir` action"), "{line}");
        }
        // Removing is not the mirror image: no hand does it, so the reason must not name one.
        let rm = wrong_hand(&argv("rmdir /home/ai/x")).unwrap();
        assert!(rm.contains("no hand that removes a folder") && rm.contains("say undo") && !rm.contains("make_dir"), "{rm}");
        // Reading and building with the same programs stays free — the housekeeping jobs of
        // §7 look at the machine with exactly these.
        for line in ["apt list --installed", "dpkg-query -W -f ${Package}", "cargo build", "npm test", "python3 primes.py", "ls -l", "sudo ls", "env ls -l", "mkdir build", "mkdir -p src/gen"] {
            assert_eq!(wrong_hand(&argv(line)), None, "{line}");
        }
        assert_eq!(wrong_hand(&[]), None);
    }

    #[test]
    fn write_inside_workspace_is_auto() {
        let a = Action::WriteFile { path: "out.txt".into(), contents: "x".into() };
        assert_eq!(classify(&a, &ws()), Risk::Auto);
    }

    #[test]
    fn read_inside_workspace_is_auto() {
        let a = Action::ReadFile { path: "out.txt".into(), from_line: None, lines: None };
        assert_eq!(classify(&a, &ws()), Risk::Auto);
    }

    #[test]
    fn read_outside_workspace_needs_confirm() {
        // /etc is a carved-out exception (see etc_read_is_auto_but_etc_write_is_not below);
        // this checks the general case of an outside-workspace path with no such allowance.
        let a = Action::ReadFile { path: "/root/secret.txt".into(), from_line: None, lines: None };
        assert!(matches!(classify(&a, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn edit_inside_workspace_is_auto_outside_needs_confirm() {
        let inside = Action::EditFile { path: "a.py".into(), find: "a".into(), replace: "b".into() };
        assert_eq!(classify(&inside, &ws()), Risk::Auto);
        let outside = Action::EditFile { path: "/etc/hosts".into(), find: "a".into(), replace: "b".into() };
        assert!(matches!(classify(&outside, &ws()), Risk::NeedsConfirm(_)));
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

    #[test]
    fn install_remove_service_are_auto() {
        assert_eq!(classify(&Action::Install { packages: vec!["cowsay".into()] }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::Remove { packages: vec!["cowsay".into()] }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::Service { name: "nginx".into(), action: ServiceDo::Disable }, &ws()), Risk::Auto);
    }

    #[test]
    fn bad_names_are_blocked_not_run() {
        assert!(matches!(classify(&Action::Install { packages: vec!["-o".into()] }, &ws()), Risk::NeedsConfirm(_)));
        assert!(matches!(classify(&Action::Service { name: "/tmp/x.service".into(), action: ServiceDo::Enable }, &ws()), Risk::NeedsConfirm(_)));
        assert!(matches!(classify(&Action::FetchPackages { manager: Manager::Pip, packages: vec!["a b".into()] }, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn make_dir_roots() {
        assert_eq!(classify(&Action::MakeDir { path: "/data/work".into() }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::MakeDir { path: "/home/ai/x".into() }, &ws()), Risk::Auto);
        assert!(matches!(classify(&Action::MakeDir { path: "/opt/x".into() }, &ws()), Risk::NeedsConfirm(_)));
        assert!(matches!(classify(&Action::MakeDir { path: "/data/../etc".into() }, &ws()), Risk::NeedsConfirm(_)));
        assert!(matches!(classify(&Action::MakeDir { path: "relative".into() }, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn etc_read_is_auto_but_etc_write_is_not() {
        assert_eq!(classify(&Action::ReadFile { path: "/etc/fstab".into(), from_line: None, lines: None }, &ws()), Risk::Auto);
        assert!(matches!(classify(&Action::WriteFile { path: "/etc/fstab".into(), contents: "x".into() }, &ws()), Risk::NeedsConfirm(_)));
        assert!(matches!(classify(&Action::ReadFile { path: "/etc/../root/x".into(), from_line: None, lines: None }, &ws()), Risk::NeedsConfirm(_)));
    }

    #[test]
    fn fetch_and_set_setting_are_auto() {
        assert_eq!(classify(&Action::FetchPackages { manager: Manager::Npm, packages: vec!["left-pad".into()] }, &ws()), Risk::Auto);
        assert_eq!(classify(&Action::SetSetting { key: "projects_root".into(), value: "/data/work".into() }, &ws()), Risk::Auto);
    }
}
