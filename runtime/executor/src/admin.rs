//! The privileged hand. Everything it does, it does by calling the root wrapper —
//! a fixed menu of verbs with validated arguments (`runtime/admin/ai-os-admin`), never
//! a shell string. Every change it makes comes back with the undo entry that puts it back.
use crate::action::{Action, ServiceDo};
use crate::undo::UndoEntry;
use crate::worker::{head, tail, window, Outcome, Worker};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// The one program user `ai` may run as root (installed by Task 1).
pub const WRAPPER: &str = "/usr/local/libexec/ai-os-admin";

pub struct AdminWorker;

/// Names in `after` that were not in `before`. Both come from `pkg-list`, already sorted.
/// ponytail: O(n·m) scan over a few thousand names — a set only if this ever shows up.
pub fn added(before: &[&str], after: &[&str]) -> Vec<String> {
    after.iter().filter(|p| !before.contains(p)).map(|p| p.to_string()).collect()
}

/// Names in `before` that are no longer in `after`.
pub fn removed(before: &[&str], after: &[&str]) -> Vec<String> {
    added(after, before)
}

/// `service <name> state` prints one line: `<enabled-state> <active-state>`. Anything
/// that is not exactly "enabled"/"active" (static, masked, unknown, …) counts as false —
/// undo puts the unit back to a state we can actually name.
pub fn parse_state(line: &str) -> (bool, bool) {
    let mut w = line.split_whitespace();
    (w.next() == Some("enabled"), w.next() == Some("active"))
}

/// `["--", pkg, pkg, …]` — the wrapper insists on the `--` before any name list.
fn name_args(packages: &[String]) -> Vec<&str> {
    std::iter::once("--").chain(packages.iter().map(String::as_str)).collect()
}

fn as_str(v: &[String]) -> Vec<&str> { v.iter().map(String::as_str).collect() }

/// What was in a file before a write: its contents, nothing at all, or a read we could not
/// trust (with the reason) — the last one records no undo.
enum Before {
    Contents(String),
    Missing,
    Unknown(String),
}

/// Put a unit back to the state it was in. Free function taking the wrapper call so the two
/// verbs it issues can be checked without a machine.
/// ponytail: the wrapper's enable is `enable --now`, so a unit that was enabled but stopped
/// comes back running. Split the verbs in the wrapper if a case ever needs that exact pair.
fn reverse_service(name: &str, was_enabled: bool, was_active: bool, call: impl Fn(&str, &[&str]) -> Outcome) -> Outcome {
    let first = call("service", &[name, if was_enabled { "enable" } else { "disable" }]);
    if !was_active {
        return first;
    }
    // `disable --now` stopped a unit that was running, and `enable --now` may have been a no-op
    // on one already enabled: restart is what actually gets it running again either way.
    let second = call("service", &[name, "restart"]);
    let mut detail = first.detail.trim().to_string();
    if !second.detail.trim().is_empty() {
        if !detail.is_empty() {
            detail.push_str("; ");
        }
        detail.push_str(second.detail.trim());
    }
    if first.ok && second.ok { Outcome::ok(detail) } else { Outcome::err(detail) }
}

impl AdminWorker {
    /// Run one wrapper verb. `ok` = exit 0; on failure the detail carries the wrapper's
    /// `refused: …` line (or the tool's own stderr, whose reason lives at the end).
    /// The detail is capped — for output the caller must have in full (`pkg-list`,
    /// `read-file`) use `pkg_list`/`read_file`, which go through `raw`.
    pub fn call(verb: &str, args: &[&str], stdin: Option<&str>) -> Outcome {
        let (ok, stdout, stderr) = Self::raw(verb, args, stdin);
        if ok { Outcome::ok(head(&stdout, 500)) } else { Outcome::err(Self::why(&stdout, &stderr)) }
    }

    /// The untruncated call: (exit-0?, stdout, stderr).
    fn raw(verb: &str, args: &[&str], stdin: Option<&str>) -> (bool, String, String) {
        let mut child = match Command::new("sudo")
            .args(["-n", WRAPPER, verb])
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => return (false, String::new(), format!("spawn failed: {e}")),
        };
        if let Some(s) = stdin {
            // Take the pipe so it is closed here: the child waits on EOF before it can exit.
            // A write error is almost always the wrapper having refused and closed the pipe, so
            // fall through to the wait and let its own `refused:` line be the reason (and reap it).
            let mut pipe = child.stdin.take().expect("stdin was piped");
            let _ = pipe.write_all(s.as_bytes());
        }
        match child.wait_with_output() {
            Ok(o) => (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).into_owned(),
                String::from_utf8_lossy(&o.stderr).into_owned(),
            ),
            Err(e) => (false, String::new(), format!("wait failed: {e}")),
        }
    }

    /// Why a call failed, in 500 chars: stderr if there is any, else whatever it printed.
    fn why(stdout: &str, stderr: &str) -> String {
        let s = stderr.trim();
        if s.is_empty() { format!("failed: {}", tail(stdout.trim(), 500)) } else { tail(s, 500) }
    }

    /// Every installed package name, sorted. `Err` when the list could not be taken —
    /// a missing "before" list would make the undo diff the whole system.
    pub fn pkg_list() -> Result<Vec<String>, Outcome> {
        let (ok, stdout, stderr) = Self::raw("pkg-list", &[], None);
        if ok {
            Ok(stdout.lines().map(str::to_string).collect())
        } else {
            Err(Outcome::err(Self::why(&stdout, &stderr)))
        }
    }

    /// A file's full contents through the wrapper (roots /etc, /data, /home/ai).
    fn read_file(path: &str) -> Result<String, Outcome> {
        let (ok, stdout, stderr) = Self::raw("read-file", &[path], None);
        if ok { Ok(stdout) } else { Err(Outcome::err(Self::why(&stdout, &stderr))) }
    }

    /// What a write is about to replace. A read that failed for any reason other than the file
    /// not being there is `Unknown`, never `Missing`: an undo built on a guess would turn a
    /// transient failure into a `remove-file`.
    fn before_write(path: &str) -> Before {
        match Self::read_file(path) {
            Ok(text) => Before::Contents(text),
            Err(out) if Path::new(path).exists() => Before::Unknown(out.detail),
            Err(_) => Before::Missing,
        }
    }

    /// Write `contents`, recording whatever was there before.
    fn write_file(path: &str, contents: &str, before: Before) -> Outcome {
        let mut out = Self::call("write-file", &[path], Some(contents));
        if !out.ok {
            return out;
        }
        let contents = match before {
            Before::Contents(c) => Some(c),
            Before::Missing => None,
            Before::Unknown(why) => {
                out.detail.push_str(&format!("; no undo recorded: {why}"));
                return out;
            }
        };
        out.with_undo(UndoEntry::FileBefore { path: path.to_string(), contents })
    }

    /// Install/remove, with the undo taken from what the package list actually gained or lost —
    /// recorded even when the command failed, because a partial change is still a real one.
    fn packages(verb: &str, packages: &[String]) -> Outcome {
        let before = match Self::pkg_list() { Ok(v) => v, Err(out) => return out };
        let mut out = Self::call(verb, &name_args(packages), None);
        let after = match Self::pkg_list() {
            Ok(v) => v,
            Err(e) => {
                out.detail.push_str(&format!("; no undo recorded: {}", e.detail));
                return out;
            }
        };
        let (b, a) = (as_str(&before), as_str(&after));
        let changed = if verb == "install" { added(&b, &a) } else { removed(&b, &a) };
        if changed.is_empty() {
            return out;
        }
        out.with_undo(if verb == "install" {
            UndoEntry::PackagesAdded { packages: changed }
        } else {
            UndoEntry::PackagesRemoved { packages: changed }
        })
    }
}

impl Worker for AdminWorker {
    fn run(&self, action: &Action) -> Outcome {
        match action {
            Action::Install { packages } => Self::packages("install", packages),
            Action::Remove { packages } => Self::packages("remove", packages),
            Action::Service { name, action } => {
                let state = Self::call("service", &[name, "state"], None);
                let (was_enabled, was_active) = parse_state(state.detail.trim());
                let verb = match action {
                    ServiceDo::Enable => "enable",
                    ServiceDo::Disable => "disable",
                    ServiceDo::Restart => "restart",
                };
                let out = Self::call("service", &[name, verb], None);
                // A restart leaves enabled/active exactly as they were: nothing to put back.
                // Without a "before" state there is nothing trustworthy to put back either.
                if matches!(action, ServiceDo::Restart) || !state.ok || !out.ok {
                    return out;
                }
                out.with_undo(UndoEntry::ServiceState { name: name.clone(), was_enabled, was_active })
            }
            // Idempotent: making a folder that is already there changes nothing, so it
            // records nothing — undoing it would delete a folder we did not create.
            Action::MakeDir { path } => {
                let existed = Path::new(path).exists();
                let out = Self::call("make-dir", &[path], None);
                if existed || !out.ok { out } else { out.with_undo(UndoEntry::DirCreated { path: path.clone() }) }
            }
            Action::ReadFile { path, from_line, lines } => match Self::read_file(path) {
                Ok(text) => Outcome::ok(window(&text, *from_line, *lines)),
                Err(out) => out,
            },
            Action::WriteFile { path, contents } => {
                Self::write_file(path, contents, Self::before_write(path))
            }
            // Rule 9: edit in place, the same once-only `find` as the sandbox.
            Action::EditFile { path, find, replace } => {
                let text = match Self::read_file(path) { Ok(t) => t, Err(out) => return out };
                match text.matches(find.as_str()).count() {
                    0 => return Outcome::err("find text not found — re-read the file and quote it exactly"),
                    1 => {}
                    n => return Outcome::err(format!("find text occurs in {n} places — include more surrounding lines so it is unique")),
                }
                let edited = text.replacen(find.as_str(), replace, 1);
                Self::write_file(path, &edited, Before::Contents(text))
            }
            _ => Outcome::err("not an admin action"),
        }
    }

    fn reverse(&self, entry: &UndoEntry) -> Outcome {
        match entry {
            UndoEntry::PackagesAdded { packages } => Self::call("remove", &name_args(packages), None),
            UndoEntry::PackagesRemoved { packages } => Self::call("install", &name_args(packages), None),
            UndoEntry::ServiceState { name, was_enabled, was_active } => {
                reverse_service(name, *was_enabled, *was_active, |verb, args| Self::call(verb, args, None))
            }
            UndoEntry::FileBefore { path, contents } => match contents {
                Some(c) => Self::call("write-file", &[path], Some(c)),
                None => Self::call("remove-file", &[path], None),
            },
            UndoEntry::DirCreated { path } => Self::call("remove-dir", &[path], None),
            // Settings live in the executor's database and project files in a snapshot —
            // neither is the wrapper's business.
            UndoEntry::Setting { .. } | UndoEntry::ProjectSnapshot { .. } => Outcome::err("not an admin undo"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_difference_is_the_undo_list() {
        assert_eq!(added(&["a", "b"], &["a", "b", "c", "d"]), vec!["c", "d"]);
        assert_eq!(added(&["a", "b", "c"], &["a"]), Vec::<String>::new());
        assert_eq!(removed(&["a", "b", "c"], &["a"]), vec!["b", "c"]);
    }

    #[test]
    fn service_state_parses() {
        assert_eq!(parse_state("enabled active"), (true, true));
        assert_eq!(parse_state("disabled inactive"), (false, false));
        assert_eq!(parse_state("static active"), (false, true));
        assert_eq!(parse_state("garbage"), (false, false));
    }

    #[test]
    fn reverse_service_puts_back_running_as_well_as_enabled() {
        let seen = std::cell::RefCell::new(Vec::new());
        let call = |_verb: &str, args: &[&str]| {
            seen.borrow_mut().push(args.join(" "));
            Outcome::ok("")
        };
        // Was running but disabled: disabling again stops it, so it has to be restarted.
        assert!(reverse_service("cron", false, true, &call).ok);
        assert_eq!(seen.borrow().as_slice(), &["cron disable".to_string(), "cron restart".to_string()]);
        seen.borrow_mut().clear();
        // Was stopped: enable alone (the wrapper's enable --now starts it — see the ponytail note).
        assert!(reverse_service("cron", true, false, &call).ok);
        assert_eq!(seen.borrow().as_slice(), &["cron enable".to_string()]);
    }

    #[test]
    fn reverse_service_fails_if_either_half_fails() {
        let call = |_verb: &str, args: &[&str]| {
            if args.contains(&"restart") { Outcome::err("Job for cron.service failed") } else { Outcome::ok("") }
        };
        let out = reverse_service("cron", true, true, &call);
        assert!(!out.ok);
        assert_eq!(out.detail, "Job for cron.service failed");
    }

    #[test]
    fn name_args_puts_the_dash_dash_first() {
        assert_eq!(name_args(&["cowsay".to_string(), "sl".to_string()]), vec!["--", "cowsay", "sl"]);
    }

    #[test]
    fn actions_without_an_admin_hand_are_refused() {
        // No wrapper call: these are rejected before anything is spawned.
        let out = AdminWorker.run(&Action::HttpPost { url: "http://x".into(), body: "b".into() });
        assert!(!out.ok);
        assert_eq!(out.detail, "not an admin action");
        assert_eq!(AdminWorker.reverse(&UndoEntry::Setting { key: "k".into(), previous: None }).detail, "not an admin undo");
    }
}
