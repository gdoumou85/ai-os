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

fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| s.rsplit(')').next().and_then(|r| r.split_whitespace().next()) != Some("Z"))
}

/// The pid in `<n>.pid`, but only when it is still this program: alive, not a zombie, and its
/// `/proc/<pid>/cmdline` still names this pid file. A pid is a small number on a long-lived,
/// root-run machine — the machine reuses it, so a stale file believed forever would eventually
/// call `output` on, or send `stop`'s TERM/KILL to, some unrelated process (review, 2026-09-24).
/// The wrapper's own cmdline is `sh -c <script> sh <pidf> <log> <exitf> <argv...>`, so the pid
/// file's path is always one of its arguments for as long as that pid is this program's.
fn live_pid(n: &str) -> Option<u32> {
    let pid: u32 = std::fs::read_to_string(dir().join(format!("{n}.pid"))).ok()?.trim().parse().ok()?;
    if !alive(pid) { return None; }
    let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
    let needle = dir().join(format!("{n}.pid"));
    String::from_utf8_lossy(&cmdline).contains(needle.to_string_lossy().as_ref()).then_some(pid)
}

/// Starts `argv` in a session of its own, so `stop` ends it and everything it started, and so it
/// is not this process's child — nothing is left here to reap. Its output goes to a log.
pub fn start(name: &str, argv: &[String], cwd: &Path) -> Result<String, String> {
    let n = safe(name)?;
    if argv.is_empty() { return Err("nothing to run: argv is empty".into()); }
    if let Some(pid) = live_pid(&n) {
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
    for _ in 0..40 { if live_pid(&n).is_some() { break } std::thread::sleep(Duration::from_millis(50)); }
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
    let state = match (live_pid(&n), std::fs::read_to_string(d.join(format!("{n}.exit")))) {
        (Some(p), _) => format!("{n} is running (pid {p})"),
        (None, Ok(code)) => format!("{n} has ended with exit {}", code.trim()),
        (None, Err(_)) => format!("{n} has ended (stopped)"),
    };
    Ok(format!("{state}; its last {} of {} lines of output:\n{}", all.len().min(k), all.len(), crate::worker::tail(&shown, 3000)))
}

/// TERM to its whole session, KILL after 5 s.
pub fn stop(name: &str) -> Result<String, String> {
    let n = safe(name)?;
    let Some(pid) = live_pid(&n) else { return Ok(format!("{n} was not running")) };
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
        .filter(|n| live_pid(n).is_some()).collect();
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

    /// A `.pid` file naming a pid that is alive but is NOT this program (its cmdline never
    /// mentions the pid file) must not be believed: a machine that runs a long time, as root,
    /// reuses pids, and a stale file believed forever would let `output` mask a real exit code
    /// and let `stop` send TERM/KILL at some unrelated process (review, 2026-09-24, fix round 1).
    #[test]
    fn a_pid_reused_by_an_unrelated_process_is_not_believed() {
        let name = format!("stale{}", std::process::id());
        let d = dir();
        std::fs::create_dir_all(&d).unwrap();
        // The test process itself is alive and not a zombie, but its own cmdline says nothing
        // about this made-up pid file — exactly the reused-pid case.
        std::fs::write(d.join(format!("{name}.pid")), std::process::id().to_string()).unwrap();
        std::fs::write(d.join(format!("{name}.log")), "old output from a previous, unrelated pid\n").unwrap();

        assert!(!running().contains(&name), "{:?}", running());
        assert!(!output(&name, None).unwrap().contains("is running"), "{}", output(&name, None).unwrap());
        assert!(stop(&name).unwrap().contains("was not running"), "must not signal the test process");

        let _ = std::fs::remove_file(d.join(format!("{name}.pid")));
        let _ = std::fs::remove_file(d.join(format!("{name}.log")));
    }
}
