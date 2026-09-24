//! Watchers (watchers design §1): timers, checks and live programs that wake the AI, each with
//! the reason its maker gave. Rows in the store; the service's scheduler runs them.
use crate::store::StoreError;
use rusqlite::{Connection, OptionalExtension};
use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

/// An alert from a watcher less than this after its last one is counted, not handled.
pub const THROTTLE_MS: i64 = 120_000;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind { Timer, Check, Push }

impl Kind {
    fn as_str(self) -> &'static str { match self { Kind::Timer => "timer", Kind::Check => "check", Kind::Push => "push" } }
    fn parse(s: &str) -> Kind { match s { "check" => Kind::Check, "push" => Kind::Push, _ => Kind::Timer } }
}

/// One row. Times are ms since the epoch; `next_at` is when a timer or check is next due.
#[derive(Debug, Clone, PartialEq)]
pub struct Watcher {
    pub name: String, pub reason: String, pub urgent: bool, pub made_by: String, pub kind: Kind,
    pub every_s: i64, pub daily_at: String, pub argv: Vec<String>, pub paused: bool,
    pub next_at: i64, pub last_fired_at: i64, pub last_text: String, pub fails: i64, pub skipped: i64, pub created_at: i64,
}

/// What a watcher hands the engine when it fires.
#[derive(Debug, Clone, PartialEq)]
pub struct Alert { pub watcher: String, pub text: String, pub reason: String, pub urgent: bool, pub made_by: String }

/// When, as the AI writes it.
#[derive(Debug, Clone, PartialEq)]
pub enum When { Every(i64), Daily(String), Live }

const WHEN_HELP: &str = "when is one of: every N minutes, every N hours, daily HH:MM, live";

pub fn init(c: &Connection) -> Result<(), StoreError> {
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS watchers(name TEXT PRIMARY KEY, reason TEXT NOT NULL, urgent INTEGER NOT NULL, made_by TEXT NOT NULL,
           kind TEXT NOT NULL, every_s INTEGER NOT NULL DEFAULT 0, daily_at TEXT NOT NULL DEFAULT '', argv TEXT NOT NULL DEFAULT '[]',
           paused INTEGER NOT NULL DEFAULT 0, next_at INTEGER NOT NULL DEFAULT 0, last_fired_at INTEGER NOT NULL DEFAULT 0,
           last_text TEXT NOT NULL DEFAULT '', fails INTEGER NOT NULL DEFAULT 0, skipped INTEGER NOT NULL DEFAULT 0, created_at INTEGER NOT NULL);",
    )?;
    Ok(())
}

pub fn parse_when(s: &str) -> Result<When, String> {
    let lower = s.trim().to_lowercase();
    let w: Vec<&str> = lower.split_whitespace().collect();
    let every = |n: i64, unit: &str| -> Result<When, String> {
        let per = match unit { "minute" | "minutes" | "min" | "mins" => 60, "hour" | "hours" => 3600, "day" | "days" => 86_400,
            _ => return Err(format!("cannot read when \"{s}\" (at least a minute apart): {WHEN_HELP}")) };
        // Up to a year: a bigger number would overflow the times it is added to.
        match n.checked_mul(per) {
            Some(secs) if n >= 1 && secs <= 365 * 86_400 => Ok(When::Every(secs)),
            _ => Err(format!("cannot read when \"{s}\": {WHEN_HELP}")),
        }
    };
    match w[..] {
        ["live"] => Ok(When::Live),
        ["every", unit] => every(1, unit),
        ["every", n, unit] => every(n.parse().map_err(|_| format!("cannot read when \"{s}\": {WHEN_HELP}"))?, unit),
        ["daily", at] => {
            let hm: Vec<u32> = at.split(':').filter_map(|p| p.parse().ok()).collect();
            match hm[..] {
                [h, m] if h < 24 && m < 60 => Ok(When::Daily(format!("{h:02}:{m:02}"))),
                _ => Err(format!("cannot read the time \"{at}\": daily HH:MM, like daily 08:00")),
            }
        }
        _ => Err(format!("cannot read when \"{s}\": {WHEN_HELP}")),
    }
}

/// A watcher from the AI's `watch`, checked; `put` stores it.
pub fn build(name: &str, reason: &str, urgent: bool, made_by: &str, when: &str, argv: Vec<String>) -> Result<Watcher, String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 40 || !name.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_' || c == ' ') {
        return Err(format!("a watcher's name is letters, digits, -, _ and spaces, up to 40 of them, not \"{name}\""));
    }
    if reason.trim().is_empty() { return Err("give the watcher a reason: what it is for and what to do when it fires".into()) }
    let argv: Vec<String> = argv.into_iter().filter(|a| !a.is_empty()).collect();
    let (kind, every_s, daily_at) = match (parse_when(when)?, argv.is_empty()) {
        (When::Every(s), true) => (Kind::Timer, s, String::new()),
        (When::Every(s), false) => (Kind::Check, s, String::new()),
        (When::Daily(at), true) => (Kind::Timer, 0, at),
        (When::Daily(_), false) => return Err("a command runs every N minutes or hours, not daily at a time: use every, or daily with no command for a timer".into()),
        (When::Live, false) => (Kind::Push, 0, String::new()),
        (When::Live, true) => return Err("live needs a command: a program that keeps running and runs ai-os-alert when something happens".into()),
    };
    Ok(Watcher { name: name.into(), reason: reason.trim().into(), urgent, made_by: made_by.into(), kind, every_s, daily_at, argv,
        paused: false, next_at: 0, last_fired_at: 0, last_text: String::new(), fails: 0, skipped: 0, created_at: 0 })
}

/// Whether replacing `old` with `new` must stop its live program: only when what runs changed,
/// so an alert that only brings the reason up to date misses no events.
pub fn needs_restart(old: &Watcher, new: &Watcher) -> bool {
    old.kind == Kind::Push && (new.kind != old.kind || new.argv != old.argv)
}

/// Creates it, or replaces the one of that name (its last alert kept); on again, fails forgotten.
pub fn put(c: &Connection, w: &Watcher, now: i64) -> Result<(), StoreError> {
    let next = next_after(c, w, now)?;
    c.execute(
        "INSERT INTO watchers(name, reason, urgent, made_by, kind, every_s, daily_at, argv, paused, next_at, fails, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 0, ?9, 0, ?10)
         ON CONFLICT(name) DO UPDATE SET reason = excluded.reason, urgent = excluded.urgent, made_by = excluded.made_by, kind = excluded.kind,
           every_s = excluded.every_s, daily_at = excluded.daily_at, argv = excluded.argv, paused = 0, next_at = excluded.next_at, fails = 0",
        rusqlite::params![w.name, w.reason, w.urgent, w.made_by, w.kind.as_str(), w.every_s, w.daily_at, serde_json::to_string(&w.argv)?, next, now],
    )?;
    Ok(())
}

const COLS: &str = "name, reason, urgent, made_by, kind, every_s, daily_at, argv, paused, next_at, last_fired_at, last_text, fails, skipped, created_at";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Watcher> {
    let argv: String = r.get(7)?;
    Ok(Watcher {
        name: r.get(0)?, reason: r.get(1)?, urgent: r.get(2)?, made_by: r.get(3)?, kind: Kind::parse(&r.get::<_, String>(4)?),
        every_s: r.get(5)?, daily_at: r.get(6)?, argv: serde_json::from_str(&argv).unwrap_or_default(), paused: r.get(8)?,
        next_at: r.get(9)?, last_fired_at: r.get(10)?, last_text: r.get(11)?, fails: r.get(12)?, skipped: r.get(13)?, created_at: r.get(14)?,
    })
}

pub fn list(c: &Connection) -> Result<Vec<Watcher>, StoreError> {
    let mut st = c.prepare(&format!("SELECT {COLS} FROM watchers ORDER BY created_at, name"))?;
    let v = st.query_map([], row)?.collect::<Result<Vec<_>, _>>()?;
    Ok(v)
}

pub fn get(c: &Connection, name: &str) -> Result<Option<Watcher>, StoreError> {
    Ok(c.query_row(&format!("SELECT {COLS} FROM watchers WHERE name = ?1"), [name], row).optional()?)
}

pub fn delete(c: &Connection, name: &str) -> Result<bool, StoreError> {
    Ok(c.execute("DELETE FROM watchers WHERE name = ?1", [name])? > 0)
}

/// Pause or resume; either way its failures start again from none.
pub fn set_paused(c: &Connection, name: &str, paused: bool) -> Result<bool, StoreError> {
    Ok(c.execute("UPDATE watchers SET paused = ?2, fails = 0 WHERE name = ?1", (name, paused))? > 0)
}

pub enum Fire { Handled(Alert), Throttled, Refused(String) }

/// Something happened for `name` (its timer, its check's output, its program's `ai-os-alert`).
pub fn fire(c: &Connection, name: &str, text: &str, now: i64) -> Result<Fire, StoreError> {
    let Some(w) = get(c, name)? else { return Ok(Fire::Refused(format!("no watcher is called {name}"))) };
    if w.paused { return Ok(Fire::Refused(format!("the watcher {name} is paused"))) }
    if w.last_fired_at > 0 && now - w.last_fired_at < THROTTLE_MS {
        c.execute("UPDATE watchers SET skipped = skipped + 1 WHERE name = ?1", [name])?;
        return Ok(Fire::Throttled);
    }
    let text = cut(text.trim(), 1500);
    let text = if w.skipped > 0 { format!("{text} (and {} more since the last)", w.skipped) } else { text };
    c.execute("UPDATE watchers SET last_fired_at = ?2, last_text = ?3, skipped = 0 WHERE name = ?1", (name, now, &text))?;
    Ok(Fire::Handled(Alert { watcher: w.name, text, reason: w.reason, urgent: w.urgent, made_by: w.made_by }))
}

/// When it is next due after `now`: a timer or check missed for hours is due once, not once per
/// missed period. Local time through SQLite: no clock crate for one conversion.
fn next_after(c: &Connection, w: &Watcher, now: i64) -> Result<i64, StoreError> {
    if w.kind == Kind::Push { return Ok(0) }
    if w.daily_at.is_empty() { return Ok(now + w.every_s * 1000) }
    let at = |days: &str| -> Result<i64, StoreError> {
        Ok(c.query_row("SELECT CAST(strftime('%s', date(?1, 'unixepoch', 'localtime', ?3) || ' ' || ?2, 'utc') AS INTEGER) * 1000",
            (now / 1000, &w.daily_at, days), |r| r.get(0))?)
    };
    let today = at("+0 days")?;
    if today > now { Ok(today) } else { at("+1 days") }
}

/// `HH:MM`, local time, at `at` (ms).
pub fn clock(c: &Connection, at: i64) -> Result<String, StoreError> {
    Ok(c.query_row("SELECT strftime('%H:%M', ?1, 'unixepoch', 'localtime')", [at / 1000], |r| r.get(0))?)
}

pub fn when_text(w: &Watcher) -> String {
    match (w.kind, w.daily_at.as_str()) {
        (Kind::Push, _) => "live".into(),
        (_, "") if w.every_s % 3600 == 0 => format!("every {} h", w.every_s / 3600),
        (_, "") => format!("every {} min", w.every_s / 60),
        (_, at) => format!("daily {at}"),
    }
}

/// The watchers as the AI is told them each turn ("" when none).
pub fn line(c: &Connection) -> Result<String, StoreError> {
    let all = list(c)?;
    if all.is_empty() { return Ok(String::new()) }
    // ponytail: the first 10; the Watchers page shows the rest.
    let v: Vec<String> = all.iter().take(10).map(|w| format!("{} ({}{})", w.name, when_text(w), if w.paused { ", paused" } else { "" })).collect();
    Ok(format!("Your watchers: {}.", v.join(", ")))
}

pub fn views(c: &Connection) -> Result<Vec<aios_proto::WatcherView>, StoreError> {
    Ok(list(c)?.into_iter().map(|w| aios_proto::WatcherView {
        when: when_text(&w), name: w.name, reason: w.reason, urgent: w.urgent, made_by: w.made_by, paused: w.paused,
        last_fired_at: w.last_fired_at / 1000, last_text: w.last_text,
    }).collect())
}

/// The background program a live watcher runs as.
pub fn program_name(watcher: &str) -> String { format!("watch-{watcher}") }

/// Urgent alerts ahead of the others, each kind in the order it came.
pub fn enqueue(q: &mut VecDeque<Alert>, a: Alert) {
    let at = if a.urgent { q.iter().position(|x| !x.urgent).unwrap_or(q.len()) } else { q.len() };
    q.insert(at, a);
}

fn cut(s: &str, n: usize) -> String {
    if s.chars().count() <= n { s.to_string() } else { format!("{}…", s.chars().take(n).collect::<String>()) }
}

#[derive(Debug)]
pub enum Fired { Alert(Alert), Paused { name: String, why: String } }

/// What the scheduler works with: the real ones run commands, fakes stand in for tests.
pub trait Hands {
    /// Runs a check: its output when it exits 0, else why not.
    fn check(&mut self, argv: &[String]) -> Result<String, String>;
    fn running(&self, program: &str) -> bool;
    fn start(&mut self, program: &str, argv: &[String]) -> Result<(), String>;
    /// The end of a program's output.
    fn tail(&self, program: &str) -> String;
}

/// A check under `timeout 60` in the user's home, as the machine hand runs any command; live
/// programs through `executor::programs`.
pub struct RealHands { pub home: PathBuf }

impl Hands for RealHands {
    fn check(&mut self, argv: &[String]) -> Result<String, String> {
        let out = std::process::Command::new("timeout").arg("60").args(argv).current_dir(&self.home)
            .stdin(std::process::Stdio::null()).output().map_err(|e| format!("could not run it ({e})"))?;
        if out.status.success() { return Ok(String::from_utf8_lossy(&out.stdout).into_owned()) }
        let how = match out.status.code() { Some(124) => "it took longer than 60 s".to_string(), Some(n) => format!("exit {n}"), None => "it was killed".into() };
        Err(format!("{how} ({})", cut(String::from_utf8_lossy(&out.stderr).trim(), 200)))
    }
    fn running(&self, program: &str) -> bool { executor::programs::is_running(program) }
    fn start(&mut self, program: &str, argv: &[String]) -> Result<(), String> { executor::programs::start(program, argv, &self.home).map(|_| ()) }
    fn tail(&self, program: &str) -> String { executor::programs::output(program, Some(5)).unwrap_or_default() }
}

/// The service's clock (watchers design §1). It remembers only the live programs' recent starts.
#[derive(Default)]
pub struct Scheduler { starts: HashMap<String, Vec<i64>> }

impl Scheduler {
    /// One pass: due timers and checks run, live programs are kept up; alerts and pauses come back.
    pub fn tick(&mut self, c: &Connection, now: i64, hands: &mut dyn Hands) -> Result<Vec<Fired>, StoreError> {
        let mut out = vec![];
        for w in list(c)?.into_iter().filter(|w| !w.paused) {
            if w.kind == Kind::Push { self.keep_up(c, &w, now, hands, &mut out)?; continue }
            if w.next_at > now { continue }
            // Moved on before anything runs: a timer missed while the machine was off fires once.
            c.execute("UPDATE watchers SET next_at = ?2 WHERE name = ?1", (&w.name, next_after(c, &w, now)?))?;
            let text = if w.kind == Kind::Timer { format!("it is {}", clock(c, now)?) } else {
                match hands.check(&w.argv) {
                    Ok(o) => { c.execute("UPDATE watchers SET fails = 0 WHERE name = ?1", [&w.name])?; o.trim().to_string() }
                    Err(e) => {
                        c.execute("UPDATE watchers SET fails = fails + 1 WHERE name = ?1", [&w.name])?;
                        if w.fails + 1 >= 3 {
                            set_paused(c, &w.name, true)?;
                            out.push(Fired::Paused { name: w.name.clone(), why: format!("its check failed 3 times in a row, the last time: {e}") });
                        }
                        String::new()
                    }
                }
            };
            if text.is_empty() { continue }
            if let Fire::Handled(a) = fire(c, &w.name, &text, now)? { out.push(Fired::Alert(a)); }
        }
        Ok(out)
    }

    /// A live watcher's program, started again when it is down, at most once a minute; a sixth
    /// start within ten minutes (five restarts) pauses the watcher instead.
    fn keep_up(&mut self, c: &Connection, w: &Watcher, now: i64, hands: &mut dyn Hands, out: &mut Vec<Fired>) -> Result<(), StoreError> {
        let program = program_name(&w.name);
        if hands.running(&program) { return Ok(()) }
        let starts = self.starts.entry(w.name.clone()).or_default();
        starts.retain(|t| now - t < 600_000);
        if starts.last().is_some_and(|t| now - t < 60_000) { return Ok(()) }
        if starts.len() >= 6 {
            starts.clear();
            set_paused(c, &w.name, true)?;
            out.push(Fired::Paused { name: w.name.clone(), why: format!("its program stopped 5 times in 10 minutes; the end of its output: {}", cut(&hands.tail(&program), 300)) });
            return Ok(());
        }
        starts.push(now);
        if let Err(e) = hands.start(&program, &w.argv) { eprintln!("watchers: {program} did not start ({e})"); }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_800_000_000_000;

    fn conn() -> Connection { let c = Connection::open_in_memory().unwrap(); init(&c).unwrap(); c }

    fn add(c: &Connection, name: &str, when: &str, argv: &[&str]) {
        let w = build(name, "why", false, "AI", when, argv.iter().map(|s| s.to_string()).collect()).unwrap();
        put(c, &w, T0).unwrap();
    }

    #[derive(Default)]
    struct Fake { checks: VecDeque<Result<String, String>>, up: bool, started: Vec<String> }
    impl Hands for Fake {
        fn check(&mut self, _: &[String]) -> Result<String, String> { self.checks.pop_front().unwrap_or(Ok(String::new())) }
        fn running(&self, _: &str) -> bool { self.up }
        fn start(&mut self, p: &str, _: &[String]) -> Result<(), String> { self.started.push(p.into()); Ok(()) }
        fn tail(&self, _: &str) -> String { "last words".into() }
    }

    #[test]
    fn every_when_form_is_read_and_the_rest_refused() {
        assert_eq!(parse_when("every 5 minutes"), Ok(When::Every(300)));
        assert_eq!(parse_when("Every 2 hours"), Ok(When::Every(7200)));
        assert_eq!(parse_when("every minute"), Ok(When::Every(60)));
        assert_eq!(parse_when("daily 8:00"), Ok(When::Daily("08:00".into())));
        assert_eq!(parse_when("live"), Ok(When::Live));
        for bad in ["every 30 seconds", "daily 25:00", "sometimes", "every 0 minutes", "every 99999999999999999 hours", "every 400 days", ""] { assert!(parse_when(bad).is_err(), "{bad}"); }
        assert!(build("t", "why", false, "AI", "live", vec![]).unwrap_err().contains("live needs a command"));
        assert!(build("t", "why", false, "AI", "daily 08:00", vec!["ls".into()]).is_err());
        assert!(build("t", " ", false, "AI", "every 5 minutes", vec![]).unwrap_err().contains("reason"));
        assert!(build("a/b", "why", false, "AI", "every 5 minutes", vec![]).is_err());
        assert_eq!(build("t", "why", false, "AI", "every 5 minutes", vec!["ls".into()]).unwrap().kind, Kind::Check);
        assert_eq!(build("t", "why", false, "AI", "live", vec!["w.sh".into()]).unwrap().kind, Kind::Push);
    }

    #[test]
    fn a_live_program_is_stopped_only_when_what_it_runs_changed() {
        let w = |when: &str, argv: &[&str]| build("price", "why", false, "AI", when, argv.iter().map(|s| s.to_string()).collect()).unwrap();
        let live = w("live", &["watch-price.sh"]);
        assert!(!needs_restart(&live, &w("live", &["watch-price.sh"])), "the same command: left running");
        assert!(needs_restart(&live, &w("live", &["watch-price.sh", "--fast"])));
        assert!(needs_restart(&live, &w("every 5 minutes", &["watch-price.sh"])));
        assert!(!needs_restart(&w("every 5 minutes", &["ls"]), &w("live", &["ls"])), "no program was running");
    }

    #[test]
    fn a_timer_fires_when_due_and_once_after_a_long_gap() {
        let c = conn();
        add(&c, "time", "every 2 minutes", &[]);
        assert_eq!(line(&c).unwrap(), "Your watchers: time (every 2 min).");
        let (mut s, mut h) = (Scheduler::default(), Fake::default());
        assert!(s.tick(&c, T0 + 60_000, &mut h).unwrap().is_empty());
        let f = s.tick(&c, T0 + 120_000, &mut h).unwrap();
        assert!(matches!(&f[..], [Fired::Alert(a)] if a.watcher == "time" && a.text.starts_with("it is ")), "{f:?}");
        let f = s.tick(&c, T0 + 10 * 3_600_000, &mut h).unwrap();
        assert_eq!(f.len(), 1, "a long gap fires once, not once per missed period: {f:?}");
        assert!(s.tick(&c, T0 + 10 * 3_600_000 + 15_000, &mut h).unwrap().is_empty());
    }

    #[test]
    fn a_daily_timer_is_next_due_at_that_local_time() {
        let c = conn();
        add(&c, "morning", "daily 08:00", &[]);
        let w = get(&c, "morning").unwrap().unwrap();
        assert!(w.next_at > T0 && w.next_at <= T0 + 90_000_000, "{}", w.next_at);
        assert_eq!(clock(&c, w.next_at).unwrap(), "08:00");
        assert_eq!(when_text(&w), "daily 08:00");
    }

    #[test]
    fn a_checks_output_fires_no_output_does_not_and_three_failures_pause_it() {
        let c = conn();
        add(&c, "downloads", "every 1 minute", &["ls"]);
        let mut s = Scheduler::default();
        let mut h = Fake { checks: vec![Ok("\n".into()), Ok("new.pdf\n".into()), Err("exit 2".into()), Err("exit 2".into()), Err("exit 2 (no such folder)".into())].into(), ..Default::default() };
        let mut all = vec![];
        for i in 1..=5 { all.extend(s.tick(&c, T0 + i * 180_000, &mut h).unwrap()); }
        assert_eq!(all.len(), 2, "{all:?}");
        assert!(matches!(&all[0], Fired::Alert(a) if a.text == "new.pdf"), "{all:?}");
        assert!(matches!(&all[1], Fired::Paused { name, why } if name == "downloads" && why.contains("no such folder")), "{all:?}");
        assert!(get(&c, "downloads").unwrap().unwrap().paused);
    }

    #[test]
    fn alerts_within_two_minutes_are_counted_and_said_with_the_next() {
        let c = conn();
        add(&c, "price", "live", &["watch-price.sh"]);
        assert!(matches!(fire(&c, "price", "dropped", T0).unwrap(), Fire::Handled(a) if a.text == "dropped"));
        assert!(matches!(fire(&c, "price", "dropped more", T0 + 30_000).unwrap(), Fire::Throttled));
        assert!(matches!(fire(&c, "price", "lower still", T0 + 130_000).unwrap(), Fire::Handled(a) if a.text == "lower still (and 1 more since the last)"));
        assert!(matches!(fire(&c, "nobody", "x", T0).unwrap(), Fire::Refused(w) if w.contains("no watcher")));
        set_paused(&c, "price", true).unwrap();
        assert!(matches!(fire(&c, "price", "x", T0 + 999_999).unwrap(), Fire::Refused(w) if w.contains("paused")));
    }

    #[test]
    fn a_live_program_is_restarted_at_most_once_a_minute_and_paused_after_five_restarts() {
        let c = conn();
        add(&c, "price", "live", &["watch-price.sh"]);
        let (mut s, mut h) = (Scheduler::default(), Fake::default());
        let mut paused = vec![];
        // Ten minutes of ticks, and the program never stays up.
        for i in 0..40 { paused.extend(s.tick(&c, T0 + i * 15_000, &mut h).unwrap()); }
        assert_eq!(h.started.len(), 6, "{:?}", h.started);
        assert_eq!(h.started[0], "watch-price");
        assert!(matches!(&paused[..], [Fired::Paused { why, .. }] if why.contains("last words")), "{paused:?}");
        h.up = true;
        assert!(s.tick(&c, T0 + 3_600_000, &mut h).unwrap().is_empty(), "paused: left alone");
    }

    #[test]
    fn urgent_alerts_go_ahead_of_the_others() {
        let a = |w: &str, urgent: bool| Alert { watcher: w.into(), text: String::new(), reason: String::new(), urgent, made_by: String::new() };
        let mut q = VecDeque::new();
        for (w, u) in [("1", false), ("2", true), ("3", false), ("4", true)] { enqueue(&mut q, a(w, u)); }
        assert_eq!(q.iter().map(|a| a.watcher.as_str()).collect::<Vec<_>>(), ["2", "4", "1", "3"]);
    }
}
