use crate::job::Job;
use executor::undo::UndoEntry;
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

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS projects(name TEXT PRIMARY KEY, folder TEXT NOT NULL, description TEXT NOT NULL, touched_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS core_jobs(id TEXT PRIMARY KEY, project TEXT NOT NULL, state TEXT NOT NULL, json TEXT NOT NULL, updated_at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS instructions(id INTEGER PRIMARY KEY AUTOINCREMENT, text TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, role TEXT NOT NULL, text TEXT NOT NULL, at INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
             CREATE TABLE IF NOT EXISTS undo(id INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL, seq INTEGER NOT NULL, entry TEXT NOT NULL, applied INTEGER NOT NULL DEFAULT 0, at INTEGER NOT NULL);",
        )?;
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
    pub fn open_job(&self) -> Result<Option<Job>, StoreError> {
        let json: Option<String> = self.conn.query_row(
            "SELECT json FROM core_jobs WHERE state NOT IN ('done','failed','cancelled') ORDER BY updated_at DESC LIMIT 1",
            [], |r| r.get(0),
        ).optional()?;
        Ok(match json { Some(j) => Some(serde_json::from_str(&j)?), None => None })
    }

    pub fn get_setting(&self, key: &str) -> Result<Option<String>, StoreError> {
        Ok(self.conn.query_row("SELECT value FROM settings WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    /// Returns what the key held before — the caller needs that to put the setting back (undo).
    pub fn set_setting(&self, key: &str, value: &str) -> Result<Option<String>, StoreError> {
        let previous = self.get_setting(key)?;
        self.conn.execute(
            "INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            (key, value),
        )?;
        Ok(previous)
    }

    /// Putting back a setting that was never set means removing the row, not storing "".
    pub fn delete_setting(&self, key: &str) -> Result<(), StoreError> {
        self.conn.execute("DELETE FROM settings WHERE key = ?1", [key])?;
        Ok(())
    }

    pub fn add_undo(&self, job_id: &str, entry: &UndoEntry) -> Result<(), StoreError> {
        let seq: i64 = self.conn.query_row("SELECT COUNT(*) FROM undo WHERE job_id = ?1", [job_id], |r| r.get(0))?;
        self.conn.execute(
            "INSERT INTO undo(job_id, seq, entry, applied, at) VALUES (?1, ?2, ?3, 0, ?4)",
            (job_id, seq + 1, serde_json::to_string(entry)?, now_ms()),
        )?;
        Ok(())
    }

    /// Newest first: undo runs the job backwards, so the last change made is the first put back.
    pub fn unapplied_undo(&self, job_id: &str) -> Result<Vec<(i64, UndoEntry)>, StoreError> {
        let mut st = self.conn.prepare("SELECT id, entry FROM undo WHERE job_id = ?1 AND applied = 0 ORDER BY seq DESC")?;
        let rows = st.query_map([job_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            let (id, json) = row?;
            out.push((id, serde_json::from_str(&json)?));
        }
        Ok(out)
    }

    pub fn mark_undo_applied(&self, row_id: i64) -> Result<(), StoreError> {
        self.conn.execute("UPDATE undo SET applied = 1 WHERE id = ?1", [row_id])?;
        Ok(())
    }

    /// The job "undo that" means: the most recently touched finished job that still has
    /// something left to put back. A job still running is excluded — stop it first.
    pub fn last_undoable_job(&self) -> Result<Option<Job>, StoreError> {
        let json: Option<String> = self.conn.query_row(
            "SELECT json FROM core_jobs WHERE state IN ('done','failed','cancelled')
               AND EXISTS (SELECT 1 FROM undo WHERE undo.job_id = core_jobs.id AND undo.applied = 0)
             ORDER BY updated_at DESC LIMIT 1",
            [], |r| r.get(0),
        ).optional()?;
        Ok(match json { Some(j) => Some(serde_json::from_str(&j)?), None => None })
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
    use executor::undo::UndoEntry;

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
    fn settings_round_trip_and_hand_back_the_previous_value() {
        let s = Store::open_in_memory().unwrap();
        assert!(s.get_setting("projects_root").unwrap().is_none());
        assert_eq!(s.set_setting("projects_root", "/data/projects").unwrap(), None);
        assert_eq!(s.get_setting("projects_root").unwrap().as_deref(), Some("/data/projects"));
        // The previous value is what Task 9 needs to put a setting back.
        assert_eq!(s.set_setting("projects_root", "/srv/work").unwrap().as_deref(), Some("/data/projects"));
        assert_eq!(s.get_setting("projects_root").unwrap().as_deref(), Some("/srv/work"));
        s.delete_setting("projects_root").unwrap();
        assert!(s.get_setting("projects_root").unwrap().is_none(), "deleting restores the never-set state, not an empty string");
    }

    #[test]
    fn undo_rows_come_back_newest_first_and_applied_ones_drop_out() {
        let s = Store::open_in_memory().unwrap();
        let first = UndoEntry::DirCreated { path: "/data/projects/p".into() };
        let second = UndoEntry::PackagesAdded { packages: vec!["cowsay".into()] };
        s.add_undo("job-1", &first).unwrap();
        s.add_undo("job-1", &second).unwrap();
        s.add_undo("job-2", &UndoEntry::Setting { key: "k".into(), previous: None }).unwrap();

        let rows = s.unapplied_undo("job-1").unwrap();
        assert_eq!(rows.iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(), vec![second, first.clone()],
                   "undo runs backwards: the last thing done is the first thing put back");
        s.mark_undo_applied(rows[0].0).unwrap();
        assert_eq!(s.unapplied_undo("job-1").unwrap().iter().map(|(_, e)| e.clone()).collect::<Vec<_>>(), vec![first]);
        assert_eq!(s.unapplied_undo("job-2").unwrap().len(), 1, "another job's rows are untouched");
    }

    #[test]
    fn last_undoable_job_ignores_open_jobs_and_jobs_without_rows() {
        let s = Store::open_in_memory().unwrap();
        let mut job = Job::new("a", "/data/projects/a", "one", true, "u");
        s.save_job(&job).unwrap();
        s.add_undo(&job.id, &UndoEntry::DirCreated { path: "/data/projects/a".into() }).unwrap();
        assert!(s.last_undoable_job().unwrap().is_none(), "a job still running is not undoable yet");

        let mut bare = Job::new("b", "/data/projects/b", "two", true, "u");
        bare.state = State::Done;
        s.save_job(&bare).unwrap();
        assert!(s.last_undoable_job().unwrap().is_none(), "a finished job that changed nothing has nothing to undo");

        job.state = State::Cancelled;
        s.save_job(&job).unwrap();
        assert_eq!(s.last_undoable_job().unwrap().unwrap().id, job.id);

        let rows = s.unapplied_undo(&job.id).unwrap();
        s.mark_undo_applied(rows[0].0).unwrap();
        assert!(s.last_undoable_job().unwrap().is_none(), "once every row is applied the job is spent");
    }

    #[test]
    fn instructions_and_recent_messages() {
        let s = Store::open_in_memory().unwrap();
        s.add_instruction("always use python3").unwrap();
        assert_eq!(s.instructions().unwrap(), vec!["always use python3".to_string()]);
        for i in 0..5 { s.push_message("user", &format!("m{i}")).unwrap(); }
        let last = s.recent_messages(2).unwrap();
        assert_eq!(last, vec![("user".to_string(), "m3".to_string()), ("user".to_string(), "m4".to_string())]);
    }
}
