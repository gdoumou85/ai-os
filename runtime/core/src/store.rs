use crate::job::Job;
use rusqlite::{Connection, OptionalExtension};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")] Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")] Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProjectRow { pub name: String, pub folder: String, pub description: String, pub touched_at: i64 }

/// Memory that lives outside the chat: projects, jobs, standing instructions, recent messages.
/// The same SQLite file also holds the executor's `jobs`/`actions` tables (separate connection),
/// so these tables are named `projects`, `core_jobs`, `instructions`, `messages` to avoid the clash.
pub struct Store { conn: Connection }

fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0) }

impl Store {
    pub fn open(path: &str) -> Result<Self, StoreError> { Self::init(Connection::open(path)?) }
    pub fn open_in_memory() -> Result<Self, StoreError> { Self::init(Connection::open_in_memory()?) }

    /// The notebooks live in this database too (`crate::notes` works on the connection).
    pub fn conn(&self) -> &Connection { &self.conn }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(name TEXT PRIMARY KEY, folder TEXT NOT NULL, description TEXT NOT NULL, touched_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS core_jobs(id TEXT PRIMARY KEY, project TEXT NOT NULL, state TEXT NOT NULL, json TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS instructions(id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        // The service reads and deletes notebook entries on its own connection while the engine
        // works (Phase 3 §6): wait for the other side's write rather than fail on a locked file.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        crate::notes::init(&conn)?;
        crate::machine::init(&conn)?;
        Ok(Self { conn })
    }

    pub fn upsert_project(&self, name: &str, folder: &str, description: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO projects(name, folder, description, touched_at) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(name) DO UPDATE SET folder = excluded.folder, description = excluded.description, touched_at = excluded.touched_at",
            (name, folder, description, now_ms()),
        )?;
        Ok(())
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectRow>, StoreError> {
        let mut st = self.conn.prepare("SELECT name, folder, description, touched_at FROM projects ORDER BY touched_at DESC")?;
        let rows = st.query_map([], |r| Ok(ProjectRow { name: r.get(0)?, folder: r.get(1)?, description: r.get(2)?, touched_at: r.get(3)? }))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn get_project(&self, name: &str) -> Result<Option<ProjectRow>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT name, folder, description, touched_at FROM projects WHERE name = ?1", [name],
            |r| Ok(ProjectRow { name: r.get(0)?, folder: r.get(1)?, description: r.get(2)?, touched_at: r.get(3)? }),
        ).optional()?)
    }

    pub fn save_job(&self, job: &Job) -> Result<(), StoreError> {
        let json = serde_json::to_string(job)?;
        self.conn.execute(
            "INSERT INTO core_jobs(id, project, state, json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET state = excluded.state, json = excluded.json, updated_at = excluded.updated_at",
            (&job.id, &job.project, job.state.as_str(), json, now_ms()),
        )?;
        Ok(())
    }

    pub fn load_job(&self, id: &str) -> Result<Option<Job>, StoreError> {
        let json: Option<String> = self.conn.query_row("SELECT json FROM core_jobs WHERE id = ?1", [id], |r| r.get(0)).optional()?;
        Ok(match json { Some(j) => Some(serde_json::from_str(&j)?), None => None })
    }

    /// The newest job that still needs work or an answer — at most one is ever open (1b: one job at a time).
    ///
    /// A job saved by an older version with a step this one no longer knows (an `install` from
    /// before full access) can never run again: it is closed as failed rather than left to jam
    /// every message after it with a read error.
    pub fn open_job(&self) -> Result<Option<Job>, StoreError> {
        let row: Option<(String, String)> = self.conn.query_row(
            "SELECT id, json FROM core_jobs WHERE state NOT IN ('done','failed','cancelled') ORDER BY updated_at DESC LIMIT 1",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let Some((id, json)) = row else { return Ok(None) };
        match serde_json::from_str(&json) {
            Ok(job) => Ok(Some(job)),
            Err(e) => {
                eprintln!("store: closing job {id}, saved by an older version ({e})");
                self.conn.execute("UPDATE core_jobs SET state = 'failed' WHERE id = ?1", [&id])?;
                Ok(None)
            }
        }
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<(), StoreError> {
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(())
    }

    pub fn add_instruction(&self, text: &str) -> Result<(), StoreError> {
        self.conn.execute("INSERT INTO instructions(text, at) VALUES (?1, ?2)", (text, now_ms()))?;
        Ok(())
    }

    pub fn instructions(&self) -> Result<Vec<String>, StoreError> {
        let mut st = self.conn.prepare("SELECT text FROM instructions ORDER BY id")?;
        let rows = st.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn push_message(&self, role: &str, text: &str) -> Result<(), StoreError> {
        self.conn.execute("INSERT INTO messages(role, text, at) VALUES (?1, ?2, ?3)", (role, text, now_ms()))?;
        Ok(())
    }

    /// Clear: a clean start. The chat and the standing instructions go (the owner, 2026-09-23: a
    /// Clear that kept "(Noted for the future: …Blender…)" answered his next "Hello" with Blender,
    /// and nothing else could forget them). Projects and notebooks stay.
    pub fn forget_chat(&self) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM messages", ())?;
        self.conn.execute("DELETE FROM instructions", ())?;
        Ok(())
    }

    pub fn recent_messages(&self, n: usize) -> Result<Vec<(String, String)>, StoreError> {
        let mut st = self.conn.prepare("SELECT role, text FROM messages ORDER BY id DESC LIMIT ?1")?;
        let rows = st.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut v: Vec<(String, String)> = rows.collect::<Result<_, _>>()?;
        v.reverse();
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::{Job, State};

    #[test]
    fn projects_round_trip_newest_first() {
        let s = Store::open_in_memory().unwrap();
        s.upsert_project("alpha", "/data/projects/alpha", "first").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        s.upsert_project("beta", "/data/projects/beta", "second").unwrap();
        let list = s.list_projects().unwrap();
        assert_eq!(list.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["beta", "alpha"]);
        assert_eq!(s.get_project("alpha").unwrap().unwrap().description, "first");
        assert!(s.get_project("nope").unwrap().is_none());
    }

    #[test]
    fn job_saves_loads_and_open_job_ignores_finished() {
        let s = Store::open_in_memory().unwrap();
        let mut j = Job::new("alpha", "/data/projects/alpha", "do it", false, "Starting alpha");
        j.plan = vec!["one".into()];
        s.save_job(&j).unwrap();
        assert_eq!(s.open_job().unwrap().unwrap().id, j.id);
        let back = s.load_job(&j.id).unwrap().unwrap();
        assert_eq!(back.plan, vec!["one".to_string()]);
        j.state = State::Done;
        s.save_job(&j).unwrap();
        assert!(s.open_job().unwrap().is_none());
    }

    /// I2: two jobs started for the same project inside one process must both survive —
    /// before the fix, `Job::id` was `{project}-{unix_secs}` so two jobs in the same second
    /// shared an id and `save_job`'s upsert silently merged the second one over the first.
    #[test]
    fn two_jobs_for_the_same_project_both_persist() {
        let s = Store::open_in_memory().unwrap();
        let a = Job::new("p", "/data/projects/p", "first", true, "Starting p");
        let b = Job::new("p", "/data/projects/p", "second", true, "Starting p again");
        assert_ne!(a.id, b.id);
        s.save_job(&a).unwrap();
        s.save_job(&b).unwrap();
        assert_eq!(s.load_job(&a.id).unwrap().unwrap().goal, "first");
        assert_eq!(s.load_job(&b.id).unwrap().unwrap().goal, "second");
    }

    #[test]
    fn settings_round_trip() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.get_setting("projects_root").unwrap().is_none());
        s.set_setting("projects_root", "/data/projects").unwrap();
        s.set_setting("projects_root", "/srv/work").unwrap();
        assert_eq!(s.get_setting("projects_root").unwrap().as_deref(), Some("/srv/work"));
    }

    #[test]
    fn an_open_job_from_an_older_version_is_closed_not_a_jam() {
        let s = Store::open_in_memory().unwrap();
        let old = r#"{"steps":[{"action":{"kind":"install"}}]}"#;
        s.conn.execute("INSERT INTO core_jobs(id, project, state, json, updated_at) VALUES ('old', 'p', 'working', ?1, 1)", [old]).unwrap();
        assert!(s.open_job().unwrap().is_none());
        let state: String = s.conn.query_row("SELECT state FROM core_jobs WHERE id = 'old'", [], |r| r.get(0)).unwrap();
        assert_eq!(state, "failed", "closed for good, not re-read every time");
    }
}
