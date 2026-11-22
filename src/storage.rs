use anyhow::{Context, Result};
use rusqlite::{
    Connection, Row, params, params_from_iter,
    types::{Value as SqlValue, ValueRef},
};
use serde_json::{Map, Value, json};
use std::{path::Path, time::Duration};

const JOB_COLUMNS: &[(&str, &str)] = &[
    ("kind", "TEXT NOT NULL DEFAULT 'job'"),
    ("parent_job_id", "INTEGER"),
    ("ref", "TEXT"),
    ("issue_number", "INTEGER"),
    ("branch", "TEXT"),
    ("base_branch", "TEXT"),
    ("pr_url", "TEXT"),
    ("pr_number", "INTEGER"),
    ("review_verdict", "TEXT"),
    ("review_message", "TEXT"),
    ("merge_state", "TEXT"),
    ("state", "TEXT"),
    ("state_data", "TEXT"),
    ("iteration", "INTEGER NOT NULL DEFAULT 0"),
];

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub struct Store {
    pub conn: Connection,
}

fn row_json(row: &Row<'_>) -> rusqlite::Result<Value> {
    let mut result = Map::new();
    for i in 0..row.as_ref().column_count() {
        let value = match row.get_ref(i)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => json!(value),
            ValueRef::Real(value) => json!(value),
            ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
            ValueRef::Blob(value) => json!(value),
        };
        result.insert(row.as_ref().column_name(i)?.to_owned(), value);
    }
    Ok(Value::Object(result))
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = crate::config::expand_home(path);
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Cannot create database directory {}", parent.display())
            })?;
        }
        let conn = Connection::open(&path).with_context(|| {
            format!(
                "Cannot open database {}. Ensure the database and its parent directory are writable by the current user",
                path.display()
            )
        })?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
            CREATE TABLE IF NOT EXISTS jobs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,
                prompt TEXT NOT NULL, status TEXT NOT NULL, plan TEXT,
                report TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS tasks (
                id INTEGER PRIMARY KEY AUTOINCREMENT, job_id INTEGER NOT NULL,
                title TEXT NOT NULL, description TEXT NOT NULL, task_type TEXT DEFAULT 'simple_implementation',
                status TEXT NOT NULL DEFAULT 'PENDING', attempts INTEGER NOT NULL DEFAULT 0,
                summary TEXT, error TEXT, created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                FOREIGN KEY(job_id) REFERENCES jobs(id));
            CREATE TABLE IF NOT EXISTS runs (
                id INTEGER PRIMARY KEY AUTOINCREMENT, task_id INTEGER NOT NULL,
                provider TEXT, model TEXT, status TEXT NOT NULL, output_path TEXT,
                started_at TEXT NOT NULL, finished_at TEXT, exit_code INTEGER,
                FOREIGN KEY(task_id) REFERENCES tasks(id));
            CREATE TABLE IF NOT EXISTS artifacts (
                id INTEGER PRIMARY KEY AUTOINCREMENT, job_id INTEGER NOT NULL,
                kind TEXT NOT NULL, path TEXT NOT NULL, created_at TEXT NOT NULL,
                FOREIGN KEY(job_id) REFERENCES jobs(id));
            CREATE TABLE IF NOT EXISTS events (
                id INTEGER PRIMARY KEY AUTOINCREMENT, job_id INTEGER NOT NULL,
                event TEXT NOT NULL, payload TEXT, created_at TEXT NOT NULL,
                FOREIGN KEY(job_id) REFERENCES jobs(id));
            CREATE TABLE IF NOT EXISTS outbox (
                id INTEGER PRIMARY KEY AUTOINCREMENT, event TEXT NOT NULL,
                payload TEXT NOT NULL, status TEXT NOT NULL DEFAULT 'PENDING', created_at TEXT NOT NULL);")?;
        let have = conn
            .prepare("PRAGMA table_info(jobs)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (name, ddl) in JOB_COLUMNS {
            if !have.iter().any(|column| column == name) {
                conn.execute_batch(&format!("ALTER TABLE jobs ADD COLUMN {name} {ddl}"))?;
            }
        }
        Ok(Self { conn })
    }

    fn query(&self, sql: &str, values: impl rusqlite::Params) -> Result<Vec<Value>> {
        Ok(self
            .conn
            .prepare(sql)?
            .query_map(values, row_json)?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn create_job(
        &self,
        project: &str,
        prompt: &str,
        status: &str,
        kind: &str,
        parent: Option<i64>,
    ) -> Result<i64> {
        let ts = now();
        self.conn.execute("INSERT INTO jobs(project,prompt,status,kind,parent_job_id,created_at,updated_at) VALUES(?,?,?,?,?,?,?)", params![project,prompt,status,kind,parent,ts,ts])?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn job(&self, id: i64) -> Result<Option<Value>> {
        Ok(self
            .query("SELECT * FROM jobs WHERE id=?", [id])?
            .into_iter()
            .next())
    }
    pub fn jobs(&self) -> Result<Vec<Value>> {
        self.query("SELECT id,project,prompt,status,kind,parent_job_id,ref,issue_number,branch,base_branch,pr_url,pr_number,review_verdict,merge_state,state,iteration,created_at,updated_at FROM jobs ORDER BY id DESC LIMIT 100", [])
    }
    pub fn children(&self, id: i64) -> Result<Vec<Value>> {
        self.query("SELECT * FROM jobs WHERE parent_job_id=? ORDER BY id", [id])
    }
    pub fn tasks(&self, id: i64) -> Result<Vec<Value>> {
        self.query("SELECT * FROM tasks WHERE job_id=? ORDER BY id", [id])
    }

    pub fn events(&self, id: i64, after: i64) -> Result<Vec<Value>> {
        let mut rows = self.query(
            "SELECT id,event,payload,created_at FROM events WHERE job_id=? AND id>? ORDER BY id",
            [id, after],
        )?;
        for row in &mut rows {
            row["payload"] = serde_json::from_str(row["payload"].as_str().unwrap_or("{}"))?;
        }
        Ok(rows)
    }

    pub fn set_job(
        &self,
        id: i64,
        status: &str,
        plan: Option<&str>,
        report: Option<&str>,
    ) -> Result<()> {
        self.conn.execute("UPDATE jobs SET status=?,plan=COALESCE(?,plan),report=COALESCE(?,report),updated_at=? WHERE id=?", params![status,plan,report,now(),id])?;
        Ok(())
    }

    pub fn set_meta(&self, id: i64, fields: &Value) -> Result<()> {
        let fields = fields
            .as_object()
            .context("Job metadata must be an object")?;
        if fields.is_empty() {
            return Ok(());
        }
        let mut values = Vec::new();
        let mut columns = Vec::new();
        for (key, value) in fields {
            anyhow::ensure!(
                JOB_COLUMNS.iter().any(|(name, _)| key == name),
                "Unknown job metadata: {key}"
            );
            columns.push(format!("{key}=?"));
            values.push(if key == "state_data" && !value.is_string() {
                SqlValue::Text(value.to_string())
            } else {
                match value {
                    Value::Null => SqlValue::Null,
                    Value::String(text) => SqlValue::Text(text.clone()),
                    Value::Number(number) if number.is_i64() => {
                        SqlValue::Integer(number.as_i64().unwrap())
                    }
                    Value::Number(number) => {
                        SqlValue::Real(number.as_f64().context("Invalid metadata number")?)
                    }
                    Value::Bool(value) => SqlValue::Integer(i64::from(*value)),
                    _ => anyhow::bail!("Unsupported metadata value for {key}"),
                }
            });
        }
        values.push(SqlValue::Text(now()));
        values.push(SqlValue::Integer(id));
        self.conn.execute(
            &format!(
                "UPDATE jobs SET {},updated_at=? WHERE id=?",
                columns.join(",")
            ),
            params_from_iter(values),
        )?;
        Ok(())
    }

    pub fn event(&self, id: i64, name: &str, payload: &Value) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let ts = now();
        transaction.execute(
            "INSERT INTO events(job_id,event,payload,created_at) VALUES(?,?,?,?)",
            params![id, name, payload.to_string(), ts],
        )?;
        let mut outbox = payload.as_object().cloned().unwrap_or_default();
        outbox.insert("job_id".into(), json!(id));
        transaction.execute(
            "INSERT INTO outbox(event,payload,created_at) VALUES(?,?,?)",
            params![name, Value::Object(outbox).to_string(), ts],
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub fn add_tasks(&self, id: i64, tasks: &[Value]) -> Result<()> {
        let transaction = self.conn.unchecked_transaction()?;
        let ts = now();
        for task in tasks {
            let title = task["title"].as_str().context("Task requires title")?;
            let description = task["description"]
                .as_str()
                .context("Task requires description")?;
            transaction.execute("INSERT INTO tasks(job_id,title,description,task_type,status,created_at,updated_at) VALUES(?,?,?,?,'PENDING',?,?)", params![id,title,description,task["type"].as_str().unwrap_or("simple_implementation"),ts,ts])?;
        }
        transaction.commit()?;
        Ok(())
    }

    pub fn update_task(
        &self,
        id: i64,
        status: &str,
        summary: Option<&str>,
        error: Option<&str>,
        increment: bool,
    ) -> Result<()> {
        self.conn.execute("UPDATE tasks SET status=?,summary=COALESCE(?,summary),error=COALESCE(?,error),attempts=attempts+?,updated_at=? WHERE id=?", params![status,summary,error,i64::from(increment),now(),id])?;
        Ok(())
    }

    pub fn start_run(&self, task: i64, provider: &str, model: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO runs(task_id,provider,model,status,started_at) VALUES(?,?,?,'RUNNING',?)",
            params![task, provider, model, now()],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    pub fn finish_run(&self, id: i64, status: &str, path: Option<&str>, code: i32) -> Result<()> {
        self.conn.execute(
            "UPDATE runs SET status=?,output_path=?,finished_at=?,exit_code=? WHERE id=?",
            params![status, path, now(), code, id],
        )?;
        Ok(())
    }

    pub fn artifact(&self, id: i64, kind: &str, path: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO artifacts(job_id,kind,path,created_at) VALUES(?,?,?,?)",
            params![id, kind, path, now()],
        )?;
        Ok(())
    }

    pub fn mark_interrupted(&self) -> Result<usize> {
        Ok(self.conn.execute("UPDATE jobs SET status='INTERRUPTED',updated_at=? WHERE status IN ('RUNNING','QUEUED')", [now()])?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_python_schema_and_preserves_rows() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("old.db");
        let connection = Connection::open(&path)?;
        connection.execute_batch("CREATE TABLE jobs(id INTEGER PRIMARY KEY AUTOINCREMENT, project TEXT NOT NULL,prompt TEXT NOT NULL,status TEXT NOT NULL,plan TEXT,report TEXT,created_at TEXT NOT NULL,updated_at TEXT NOT NULL); INSERT INTO jobs(project,prompt,status,created_at,updated_at) VALUES('demo','existing goal','COMPLETED','old','old');")?;
        drop(connection);
        let store = Store::open(&path)?;
        assert_eq!(store.job(1)?.unwrap()["prompt"], "existing goal");
        assert_eq!(store.job(1)?.unwrap()["kind"], "job");
        store.set_meta(1, &json!({"state_data":{"next":"SYNC"},"iteration":2}))?;
        assert_eq!(store.job(1)?.unwrap()["state_data"], "{\"next\":\"SYNC\"}");
        assert!(store.set_meta(1, &json!({"status='oops'":"bad"})).is_err());
        drop(store);
        assert_eq!(Store::open(&path)?.job(1)?.unwrap()["iteration"], 2);
        Ok(())
    }

    #[test]
    fn task_runs_events_and_restart_state_are_persisted() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let store = Store::open(dir.path().join("store.db"))?;
        let id = store.create_job("demo", "goal", "RUNNING", "loop", None)?;
        store.add_tasks(
            id,
            &[json!({"title":"implement","description":"test the result"})],
        )?;
        let task = store.tasks(id)?[0]["id"].as_i64().unwrap();
        store.update_task(task, "RUNNING", None, None, true)?;
        let run = store.start_run(task, "fake", "model")?;
        store.finish_run(run, "SUCCEEDED", Some("result.md"), 0)?;
        store.update_task(task, "COMPLETED", Some("done"), None, false)?;
        store.event(id, "TASK_COMPLETED", &json!({"task_id":task}))?;
        assert_eq!(store.events(id, 0)?[0]["payload"]["task_id"], task);
        assert_eq!(store.tasks(id)?[0]["attempts"], 1);
        assert_eq!(
            store
                .conn
                .query_row("SELECT COUNT(*) FROM outbox", [], |row| row
                    .get::<_, i64>(0))?,
            1
        );
        assert_eq!(store.mark_interrupted()?, 1);
        assert_eq!(store.job(id)?.unwrap()["status"], "INTERRUPTED");
        Ok(())
    }
}
