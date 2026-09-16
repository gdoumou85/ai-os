use crate::action::Action;
use rusqlite::Connection;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum LogError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
}

pub struct ActionLog {
    conn: Connection,
}

impl ActionLog {
    pub fn open(path: &str) -> Result<Self, LogError> {
        Self::from_conn(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self, LogError> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self, LogError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS jobs(id TEXT PRIMARY KEY, created_at INTEGER);
             CREATE TABLE IF NOT EXISTS actions(
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                job_id TEXT NOT NULL,
                action_json TEXT NOT NULL,
                outcome TEXT NOT NULL,
                at INTEGER NOT NULL);",
        )?;
        Ok(Self { conn })
    }

    fn now() -> i64 {
        SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
    }

    pub fn ensure_job(&self, job_id: &str) -> Result<(), LogError> {
        self.conn.execute(
            "INSERT OR IGNORE INTO jobs(id, created_at) VALUES (?1, ?2)",
            (job_id, Self::now()),
        )?;
        Ok(())
    }

    pub fn append(&self, job_id: &str, action: &Action, outcome: &str) -> Result<i64, LogError> {
        self.insert(job_id, &serde_json::to_string(action)?, outcome)
    }

    /// A line with no action behind it (an undo, say). `action_json` is JSON `null` so a
    /// reader parsing the column still gets valid JSON, and no real action can collide with it.
    pub fn append_text(&self, job_id: &str, text: &str) -> Result<i64, LogError> {
        self.insert(job_id, "null", text)
    }

    fn insert(&self, job_id: &str, action_json: &str, outcome: &str) -> Result<i64, LogError> {
        self.ensure_job(job_id)?;
        self.conn.execute(
            "INSERT INTO actions(job_id, action_json, outcome, at) VALUES (?1, ?2, ?3, ?4)",
            (job_id, action_json, outcome, Self::now()),
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Every row of a job in the order it happened: `(action_json, outcome)`. `action_json` is
    /// the literal column — JSON `null` for a line with no action behind it (`append_text`).
    pub fn rows_for(&self, job_id: &str) -> Result<Vec<(String, String)>, LogError> {
        let mut q = self.conn.prepare("SELECT action_json, outcome FROM actions WHERE job_id = ?1 ORDER BY id")?;
        let rows = q.query_map([job_id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn count_for_job(&self, job_id: &str) -> Result<i64, LogError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*) FROM actions WHERE job_id = ?1",
            [job_id],
            |r| r.get(0),
        )?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_and_counts() {
        let log = ActionLog::open_in_memory().unwrap();
        let a = Action::RunCommand { argv: vec!["ls".into()] };
        let id = log.append("j1", &a, "ok: exit 0").unwrap();
        assert!(id > 0);
        assert_eq!(log.count_for_job("j1").unwrap(), 1);
        assert_eq!(log.count_for_job("other").unwrap(), 0);
        log.append_text("j1", "undo: put it back").unwrap();
        let rows = log.rows_for("j1").unwrap();
        assert_eq!(rows.len(), 2, "in the order they happened: {rows:?}");
        assert!(rows[0].0.contains("run_command") && rows[0].1 == "ok: exit 0", "{rows:?}");
        assert_eq!(rows[1], ("null".to_string(), "undo: put it back".to_string()));
        assert!(log.rows_for("other").unwrap().is_empty());
    }
}
