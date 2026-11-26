//! SQLite persistence for tickets and their event log.
//!
//! Every state change is saved before the side effect that follows it, so a
//! restarted daemon resumes each ticket from its last saved state.

use crate::ticket::{Origin, State, Ticket};
use anyhow::{Context, Result, anyhow};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use std::{path::Path, sync::Mutex, time::Duration};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS tickets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    repo TEXT NOT NULL,
    number INTEGER NOT NULL,
    state TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS tickets_issue ON tickets (repo, number);
CREATE TABLE IF NOT EXISTS events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id INTEGER NOT NULL REFERENCES tickets (id),
    at TEXT NOT NULL,
    kind TEXT NOT NULL,
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS events_ticket ON events (ticket_id, id);
CREATE TABLE IF NOT EXISTS guidance (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    ticket_id INTEGER NOT NULL REFERENCES tickets (id),
    at TEXT NOT NULL,
    by TEXT NOT NULL,
    text TEXT NOT NULL,
    stopped TEXT,
    delivered_at TEXT
);
CREATE INDEX IF NOT EXISTS guidance_ticket ON guidance (ticket_id, delivered_at);
";

/// Directions an operator wrote for a ticket's agent, kept apart from the
/// ticket row so that a running job, which saves its own copy of the
/// ticket, cannot overwrite them.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Guidance {
    pub id: i64,
    pub at: String,
    /// Where it came from: `command line` or `web`.
    pub by: String,
    pub text: String,
    /// Why the ticket was blocked when the guidance came, if it was.
    pub stopped: Option<String>,
}

/// One entry of a ticket's event log.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Event {
    pub id: i64,
    pub at: String,
    pub kind: String,
    pub payload: Value,
}

/// A ticket with the times its row was created and last saved.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct Listed {
    #[serde(flatten)]
    pub ticket: Ticket,
    pub created_at: String,
    pub updated_at: String,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

/// Thread-safe handle to the daemon database.
#[derive(Debug)]
pub struct Store {
    conn: Mutex<Connection>,
}

fn terminal_states() -> String {
    State::ALL
        .into_iter()
        .filter(|s| s.is_terminal())
        .map(|s| format!("'{s}'"))
        .collect::<Vec<_>>()
        .join(",")
}

fn decode(data: &str) -> Result<Ticket> {
    serde_json::from_str(data).context("corrupt ticket row")
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(10))?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| anyhow!("database lock poisoned"))?;
        f(&conn)
    }

    /// Queue an issue unless an unfinished ticket for it already exists.
    pub fn claim(
        &self,
        repo: &str,
        number: u64,
        title: &str,
        origin: Origin,
    ) -> Result<Option<Ticket>> {
        if self.open_ticket(repo, number)?.is_some() {
            return Ok(None);
        }
        let id = self.with(|c| {
            let at = now();
            c.execute(
                "INSERT INTO tickets (repo, number, state, data, created_at, updated_at)
                 VALUES (?1, ?2, ?3, '{}', ?4, ?4)",
                params![repo, number, State::Queued.as_str(), at],
            )?;
            Ok(c.last_insert_rowid())
        })?;
        let ticket = Ticket::new(id, repo.into(), number, title.into(), origin);
        self.save(&ticket)?;
        self.event(id, "CLAIMED", &serde_json::json!({ "origin": origin }))?;
        Ok(Some(ticket))
    }

    /// Queue an issue as `claim` does, keeping its Jira key.
    pub fn claim_issue(
        &self,
        repo: &str,
        issue: &crate::forge::Issue,
        origin: Origin,
    ) -> Result<Option<Ticket>> {
        let Some(mut ticket) = self.claim(repo, issue.number, &issue.title, origin)? else {
            return Ok(None);
        };
        if issue.key.is_some() {
            ticket.key.clone_from(&issue.key);
            self.save(&ticket)?;
        }
        Ok(Some(ticket))
    }

    pub fn save(&self, ticket: &Ticket) -> Result<()> {
        let data = serde_json::to_string(ticket)?;
        self.with(|c| {
            c.execute(
                "UPDATE tickets SET state = ?1, data = ?2, updated_at = ?3 WHERE id = ?4",
                params![ticket.state.as_str(), data, now(), ticket.id],
            )?;
            Ok(())
        })
    }

    pub fn get(&self, id: i64) -> Result<Option<Ticket>> {
        let data: Option<String> = self.with(|c| {
            Ok(
                c.query_row("SELECT data FROM tickets WHERE id = ?1", [id], |r| r.get(0))
                    .optional()?,
            )
        })?;
        data.as_deref().map(decode).transpose()
    }

    /// The unfinished ticket for an issue, if there is one.
    pub fn open_ticket(&self, repo: &str, number: u64) -> Result<Option<Ticket>> {
        let sql = format!(
            "SELECT data FROM tickets WHERE repo = ?1 COLLATE NOCASE AND number = ?2
             AND state NOT IN ({}) ORDER BY id DESC LIMIT 1",
            terminal_states()
        );
        let data: Option<String> = self.with(|c| {
            Ok(c.query_row(&sql, params![repo, number], |r| r.get(0))
                .optional()?)
        })?;
        data.as_deref().map(decode).transpose()
    }

    /// Whether a ticket for the issue reached DONE: its pull request merged.
    pub fn was_done(&self, repo: &str, number: u64) -> Result<bool> {
        self.with(|c| {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM tickets WHERE repo = ?1 COLLATE NOCASE
                 AND number = ?2 AND state = ?3)",
                params![repo, number, State::Done.as_str()],
                |r| r.get(0),
            )?)
        })
    }

    fn query(&self, sql: &str, limit: i64) -> Result<Vec<Ticket>> {
        let rows: Vec<String> = self.with(|c| {
            let mut stmt = c.prepare(sql)?;
            let rows = stmt.query_map([limit], |r| r.get(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.iter().map(|d| decode(d)).collect()
    }

    /// Every ticket that is not finished, oldest first.
    pub fn unfinished(&self) -> Result<Vec<Ticket>> {
        let sql = format!(
            "SELECT data FROM tickets WHERE state NOT IN ({}) ORDER BY id LIMIT ?1",
            terminal_states()
        );
        self.query(&sql, i64::MAX)
    }

    /// The most recent tickets with their times, newest first.
    pub fn listed(&self, limit: i64) -> Result<Vec<Listed>> {
        self.listed_where("1 = ?1", 1, limit)
    }

    /// One ticket with its times.
    pub fn get_listed(&self, id: i64) -> Result<Option<Listed>> {
        Ok(self.listed_where("id = ?1", id, 1)?.pop())
    }

    fn listed_where(&self, filter: &str, value: i64, limit: i64) -> Result<Vec<Listed>> {
        let sql = format!(
            "SELECT data, created_at, updated_at FROM tickets WHERE {filter}
             ORDER BY id DESC LIMIT ?2"
        );
        let rows: Vec<(String, String, String)> = self.with(|c| {
            let mut stmt = c.prepare(&sql)?;
            let rows = stmt.query_map(params![value, limit], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter()
            .map(|(data, created_at, updated_at)| {
                Ok(Listed {
                    ticket: decode(&data)?,
                    created_at,
                    updated_at,
                })
            })
            .collect()
    }

    /// The most recent tickets, newest first.
    pub fn recent(&self, limit: i64) -> Result<Vec<Ticket>> {
        self.query("SELECT data FROM tickets ORDER BY id DESC LIMIT ?1", limit)
    }

    pub fn event(&self, ticket: i64, kind: &str, payload: &Value) -> Result<()> {
        let payload = serde_json::to_string(payload)?;
        self.with(|c| {
            c.execute(
                "INSERT INTO events (ticket_id, at, kind, payload) VALUES (?1, ?2, ?3, ?4)",
                params![ticket, now(), kind, payload],
            )?;
            Ok(())
        })
    }

    /// Keep guidance for the ticket's agent until a turn delivers it.
    pub fn add_guidance(
        &self,
        ticket: i64,
        by: &str,
        text: &str,
        stopped: Option<&str>,
    ) -> Result<i64> {
        self.with(|c| {
            c.execute(
                "INSERT INTO guidance (ticket_id, at, by, text, stopped)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![ticket, now(), by, text, stopped],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    /// Guidance not yet given to the agent, oldest first.
    pub fn pending_guidance(&self, ticket: i64) -> Result<Vec<Guidance>> {
        self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, at, by, text, stopped FROM guidance
                 WHERE ticket_id = ?1 AND delivered_at IS NULL ORDER BY id",
            )?;
            let rows = stmt.query_map([ticket], |r| {
                Ok(Guidance {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    by: r.get(2)?,
                    text: r.get(3)?,
                    stopped: r.get(4)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// Record that these guidance entries went to the agent.
    pub fn deliver_guidance(&self, ids: &[i64]) -> Result<()> {
        let at = now();
        self.with(|c| {
            for id in ids {
                c.execute(
                    "UPDATE guidance SET delivered_at = ?1 WHERE id = ?2",
                    params![at, id],
                )?;
            }
            Ok(())
        })
    }

    /// Events of a ticket with an id greater than `after`, oldest first.
    pub fn events(&self, ticket: i64, after: i64) -> Result<Vec<Event>> {
        let rows: Vec<(i64, String, String, String)> = self.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, at, kind, payload FROM events
                 WHERE ticket_id = ?1 AND id > ?2 ORDER BY id",
            )?;
            let rows = stmt.query_map(params![ticket, after], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        rows.into_iter()
            .map(|(id, at, kind, payload)| {
                let payload = serde_json::from_str(&payload)?;
                Ok(Event {
                    id,
                    at,
                    kind,
                    payload,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (tempfile::TempDir, Store) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("state/amaru.db")).unwrap();
        (dir, store)
    }

    #[test]
    fn claims_an_issue_once_while_unfinished() {
        let (_dir, store) = store();
        let ticket = store
            .claim("me/demo", 7, "Fix it", Origin::Manual)
            .unwrap()
            .unwrap();
        assert_eq!(ticket.state, State::Queued);
        assert!(
            store
                .claim("ME/demo", 7, "Fix it", Origin::Autonomous)
                .unwrap()
                .is_none()
        );
        assert!(!store.was_done("me/demo", 7).unwrap());
        let mut done = ticket;
        done.state = State::Done;
        store.save(&done).unwrap();
        assert!(store.was_done("ME/demo", 7).unwrap());
        assert!(
            store
                .claim("me/demo", 7, "Again", Origin::Manual)
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn keeps_the_jira_key_of_a_claimed_issue() {
        let (_dir, store) = store();
        let issue = crate::forge::Issue {
            number: 12,
            key: Some("CAMEL-12".into()),
            title: "T".into(),
            ..crate::forge::Issue::default()
        };
        let ticket = store
            .claim_issue("apache/camel", &issue, Origin::Manual)
            .unwrap()
            .unwrap();
        let stored = store.get(ticket.id).unwrap().unwrap();
        assert_eq!(stored.key.as_deref(), Some("CAMEL-12"));
        assert_eq!(stored.reference(), "CAMEL-12");
        assert_eq!(stored.label(), "CAMEL-12");
    }

    #[test]
    fn saves_and_reloads_every_field() {
        let (_dir, store) = store();
        let mut ticket = store
            .claim("me/demo", 1, "T", Origin::Milestone)
            .unwrap()
            .unwrap();
        ticket.state = State::AwaitingReview;
        ticket.session_id = Some("abc".into());
        ticket.pr_number = Some(12);
        ticket.handled_feedback.push("review-1".into());
        store.save(&ticket).unwrap();
        assert_eq!(store.get(ticket.id).unwrap(), Some(ticket.clone()));
        assert_eq!(store.unfinished().unwrap(), vec![ticket.clone()]);
        assert_eq!(store.recent(5).unwrap().len(), 1);
    }

    #[test]
    fn records_events_in_order() {
        let (_dir, store) = store();
        let ticket = store
            .claim("me/demo", 1, "T", Origin::Manual)
            .unwrap()
            .unwrap();
        store
            .event(ticket.id, "STATE", &json!({"to": "PREPARING"}))
            .unwrap();
        let events = store.events(ticket.id, 0).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].kind, "CLAIMED");
        assert_eq!(events[1].payload["to"], "PREPARING");
        assert_eq!(store.events(ticket.id, events[0].id).unwrap().len(), 1);
    }

    #[test]
    fn keeps_guidance_until_it_is_delivered() {
        let (_dir, store) = store();
        let ticket = store
            .claim("me/demo", 1, "T", Origin::Manual)
            .unwrap()
            .unwrap();
        let first = store
            .add_guidance(ticket.id, "web", "use ring", None)
            .unwrap();
        store
            .add_guidance(ticket.id, "web", "skip docs", Some("stuck"))
            .unwrap();
        let pending = store.pending_guidance(ticket.id).unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].text, "use ring");
        store.deliver_guidance(&[first]).unwrap();
        let pending = store.pending_guidance(ticket.id).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "skip docs");
        assert!(store.pending_guidance(99).unwrap().is_empty());
    }

    #[test]
    fn lists_tickets_with_their_times() {
        let (_dir, store) = store();
        store.claim("me/a", 1, "one", Origin::Manual).unwrap();
        store.claim("me/a", 2, "two", Origin::Manual).unwrap();
        let all = store.listed(10).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ticket.number, 2);
        assert!(!all[0].created_at.is_empty());
        let one = store.get_listed(all[1].ticket.id).unwrap().unwrap();
        assert_eq!(one.ticket.title, "one");
        assert!(store.get_listed(99).unwrap().is_none());
        let json = serde_json::to_value(&one).unwrap();
        assert_eq!(json["number"], 1);
        assert!(json["updated_at"].is_string());
    }
}
