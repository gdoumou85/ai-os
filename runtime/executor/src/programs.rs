//! Programs left running (one-loop design §1b): a server, a download, a long build. A person
//! starts one and gets on with other things; `run_command` waits up to 30 minutes for its end.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

fn dir() -> PathBuf {
    PathBuf::from(std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into())).join("ai-os-programs")
}

fn safe(name: &str) -> Result<String, String> {
    let n: String = name.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    if n.is_empty() { Err("give the program a name of letters, digits, - or _".into()) } else { Ok(n) }
}

fn pid_of(n: &str) -> Option<u32> { std::fs::read_to_string(dir().join(format!("{n}.pid"))).ok()?.trim().parse().ok() }

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit(')').next().and_then(|r| r.split_whitespace().next()) != Some("Z"))
}

/// Starts `argv` in a session of its own, so `stop` ends it and everything it started, and so it
/// is not this process's child — nothing is left here to reap. Its output goes to a log.
pub fn start(name: &str, argv: &[String], cwd: &Path) -> Result<String, String> {
    let n = safe(name)?;
    if argv.is_empty() { return Err("nothing to run: argv is empty".into()); }
    if let Some(pid) = pid_of(&n).filter(|p| alive(*p)) {
        return Err(format!("{n} is already running (pid {pid}): stop_program it first, or use another name"));
    }
    let d = dir();
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    let (log, pidf, exitf) = (d.join(format!("{n}.log")), d.join(format!("{n}.pid")), d.join(format!("{n}.exit")));
    let _ = std::fs::remove_file(&exitf);
    let _ = std::fs::remove_file(&pidf);
    let script = r#"echo $$ > "$1"; log="$2"; exitf="$3"; shift 3; "$@" > "$log" 2>&1 < /dev/null; echo $? > "$exitf""#;
    let st = Command::new("setsid").arg("-f").arg("sh").arg("-c").arg(script).arg("sh").arg(&pidf).arg(&log).arg(&exitf).args(argv)
        .current_dir(cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map_err(|e| format!("cannot run setsid ({e})"))?;
    if !st.success() { return Err(format!("could not start {}", argv[0])); }
    for _ in 0..40 { if pid_of(&n).is_some() { break } std::thread::sleep(Duration::from_millis(50)); }
    // A moment for its first words, so a program that fails at once says so now.
    std::thread::sleep(Duration::from_millis(700));
    Ok(format!("started {n} in the background. {}", output(&n, Some(10))?))
}

/// Its newest output lines and whether it still runs.
pub fn output(name: &str, lines: Option<usize>) -> Result<String, String> {
    let n = safe(name)?;
    let d = dir();
    let log = std::fs::read_to_string(d.join(format!("{n}.log"))).map_err(|_| format!("no program called {n} was started"))?;
    let k = lines.unwrap_or(40).clamp(1, 200);
    let all: Vec<&str> = log.lines().collect();
    let shown = all[all.len().saturating_sub(k)..].join("\n");
    let state = match (pid_of(&n).filter(|p| alive(*p)), std::fs::read_to_string(d.join(format!("{n}.exit")))) {
        (Some(p), _) => format!("{n} is running (pid {p})"),
        (None, Ok(code)) => format!("{n} has ended with exit {}", code.trim()),
        (None, Err(_)) => format!("{n} has ended (stopped)"),
    };
    Ok(format!("{state}; its last {} of {} lines of output:\n{}", all.len().min(k), all.len(), crate::worker::tail(&shown, 3000)))
}

/// TERM to its whole session, KILL after 5 s.
pub fn stop(name: &str) -> Result<String, String> {
    let n = safe(name)?;
    let Some(pid) = pid_of(&n).filter(|p| alive(*p)) else { return Ok(format!("{n} was not running")) };
    let group = format!("-{pid}");
    let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
    for _ in 0..50 { if !alive(pid) { return Ok(format!("stopped {n}")) } std::thread::sleep(Duration::from_millis(100)); }
    let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
    std::thread::sleep(Duration::from_millis(200));
    Ok(format!("stopped {n} (it had to be killed)"))
}

/// The names of the programs running now, for the context block.
pub fn running() -> Vec<String> {
    let Ok(rd) = std::fs::read_dir(dir()) else { return vec![] };
    let mut v: Vec<String> = rd.flatten().filter_map(|e| e.file_name().to_str()?.strip_suffix(".pid").map(String::from))
        .filter(|n| pid_of(n).is_some_and(alive)).collect();
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_starts_prints_and_stops() {
        let name = format!("t{}", std::process::id());
        let argv: Vec<String> = ["sh", "-c", "echo hello from it; sleep 60"].iter().map(|s| s.to_string()).collect();
        let said = start(&name, &argv, Path::new("/tmp")).unwrap();
        assert!(said.contains("started"), "{said}");
        assert!(running().contains(&name), "{:?}", running());
        let out = output(&name, None).unwrap();
        assert!(out.contains("is running") && out.contains("hello from it"), "{out}");
        assert!(start(&name, &argv, Path::new("/tmp")).unwrap_err().contains("already running"));
        assert!(stop(&name).unwrap().contains("stopped"));
        assert!(!running().contains(&name));
        assert!(output(&name, None).unwrap().contains("has ended"));
        assert!(output("never-started-xyz", None).unwrap_err().contains("no program called"));
    }
}
