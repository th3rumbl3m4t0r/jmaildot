//! SQLite: accounts, folders, message headers and flags, the search text, keys, settings.
//! The calendar tables are here too; cal.rs has their queries.
//!
//! Two connections: one for writes (sync), one for the window's reads, so a long write
//! never makes the window wait (WAL). Searches open their own, one per worker thread.

use crate::model::{Account, Addr, Identity, Mailbox, MsgRow};
use anyhow::{Context, Result};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS accounts(
  id INTEGER PRIMARY KEY, name TEXT NOT NULL, server TEXT NOT NULL, user TEXT NOT NULL,
  email TEXT NOT NULL DEFAULT '', display_name TEXT, jmap_id TEXT NOT NULL DEFAULT '',
  identity_id TEXT, identities TEXT NOT NULL DEFAULT '[]', sender_domains TEXT NOT NULL DEFAULT '',
  email_state TEXT, ord INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS mailboxes(
  account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE, id TEXT NOT NULL,
  name TEXT NOT NULL, role TEXT, parent TEXT, sort INTEGER NOT NULL DEFAULT 0,
  total INTEGER NOT NULL DEFAULT 0, unread INTEGER NOT NULL DEFAULT 0, notify INTEGER,
  PRIMARY KEY(account, id));
CREATE TABLE IF NOT EXISTS messages(
  id INTEGER PRIMARY KEY, account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  jid TEXT NOT NULL, blob TEXT NOT NULL DEFAULT '', thread TEXT NOT NULL DEFAULT '',
  keywords TEXT NOT NULL DEFAULT '', has_att INTEGER NOT NULL DEFAULT 0,
  from_name TEXT, from_addr TEXT NOT NULL DEFAULT '', to_json TEXT NOT NULL DEFAULT '[]',
  cc_json TEXT NOT NULL DEFAULT '[]', subject TEXT NOT NULL DEFAULT '',
  received INTEGER NOT NULL DEFAULT 0, sent INTEGER, size INTEGER NOT NULL DEFAULT 0,
  preview TEXT NOT NULL DEFAULT '', kind TEXT NOT NULL DEFAULT '', msgid TEXT,
  fetched INTEGER NOT NULL DEFAULT 0, encrypted INTEGER NOT NULL DEFAULT 0, pgp TEXT, err TEXT,
  UNIQUE(account, jid));
CREATE INDEX IF NOT EXISTS messages_recv ON messages(account, received);
CREATE INDEX IF NOT EXISTS messages_fetch ON messages(fetched, received);
CREATE TABLE IF NOT EXISTS msg_mailbox(
  msg INTEGER NOT NULL REFERENCES messages(id) ON DELETE CASCADE, account INTEGER NOT NULL,
  mailbox TEXT NOT NULL, received INTEGER NOT NULL, PRIMARY KEY(msg, mailbox));
CREATE INDEX IF NOT EXISTS msg_mailbox_list ON msg_mailbox(account, mailbox, received);
CREATE TABLE IF NOT EXISTS texts(
  id INTEGER PRIMARY KEY, subject TEXT NOT NULL, sender TEXT NOT NULL, rcpts TEXT NOT NULL,
  headers TEXT NOT NULL, body TEXT NOT NULL, attach TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS seen_rcpt(account INTEGER NOT NULL, addr TEXT NOT NULL, n INTEGER NOT NULL,
  PRIMARY KEY(account, addr));
CREATE TABLE IF NOT EXISTS contacts(addr TEXT PRIMARY KEY, name TEXT, n INTEGER NOT NULL DEFAULT 0,
  last INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS keys(fpr TEXT PRIMARY KEY, armored TEXT NOT NULL, secret INTEGER NOT NULL,
  added INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS calendars(
  account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE, href TEXT NOT NULL,
  name TEXT NOT NULL, color TEXT, ctag TEXT, sync_token TEXT, read_only INTEGER NOT NULL DEFAULT 0,
  owner TEXT, enabled INTEGER NOT NULL DEFAULT 1, ord INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(account, href));
CREATE TABLE IF NOT EXISTS events(
  id INTEGER PRIMARY KEY, account INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  cal TEXT NOT NULL, href TEXT NOT NULL, etag TEXT, ics TEXT NOT NULL, uid TEXT NOT NULL DEFAULT '',
  summary TEXT NOT NULL DEFAULT '', location TEXT, start INTEGER NOT NULL, end INTEGER NOT NULL,
  last INTEGER NOT NULL, all_day INTEGER NOT NULL, recurring INTEGER NOT NULL, alarm_min INTEGER,
  dirty INTEGER NOT NULL DEFAULT 0, deleted INTEGER NOT NULL DEFAULT 0, UNIQUE(account, href));
CREATE INDEX IF NOT EXISTS events_range ON events(account, cal, start, last);
"#;

pub struct Db {
    path: PathBuf,
    w: Mutex<Connection>,
    r: Mutex<Connection>,
}

pub fn connect(path: &Path) -> Result<Connection> {
    let c = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
    c.busy_timeout(std::time::Duration::from_secs(30))?;
    c.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=ON; PRAGMA mmap_size=1073741824; PRAGMA cache_size=-65536;")?;
    Ok(c)
}

/// Header fields of one message as JMAP gave them (Email/get).
#[derive(Clone, Debug, Default)]
pub struct Meta {
    pub jid: String,
    pub blob: String,
    pub thread: String,
    pub mailboxes: Vec<String>,
    pub keywords: Vec<String>,
    pub has_att: bool,
    pub from: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub received: i64,
    pub sent: Option<i64>,
    pub size: i64,
    pub preview: String,
    pub kind: String,
    pub msgid: Option<String>,
}

/// What processing a downloaded message produced, ready to store.
#[derive(Clone, Debug, Default)]
pub struct Stored {
    pub id: i64,
    pub fetched: i64,
    pub encrypted: bool,
    pub pgp: Option<String>,
    pub err: Option<String>,
    pub has_att: bool,
    pub preview: Option<String>,
    pub text: Option<SearchText>,
}

/// The searchable text of one message, one column per field.
#[derive(Clone, Debug, Default)]
pub struct SearchText {
    pub subject: String,
    pub sender: String,
    pub rcpts: String,
    pub headers: String,
    pub body: String,
    pub attach: String,
}

pub const ROW_COLS: &str = "m.id, m.account, m.jid, m.from_name, m.from_addr, m.to_json, m.subject, m.received, m.keywords, m.has_att, m.encrypted, m.fetched, m.preview, m.size, (SELECT group_concat(mailbox, ' ') FROM msg_mailbox x WHERE x.msg = m.id)";

pub fn row_of(r: &Row) -> rusqlite::Result<MsgRow> {
    let from_name: Option<String> = r.get(3)?;
    let from_addr: String = r.get(4)?;
    let to: String = r.get(5)?;
    let kw: String = r.get(8)?;
    let mbx: Option<String> = r.get(14)?;
    Ok(MsgRow {
        id: r.get(0)?,
        account: r.get(1)?,
        jid: r.get(2)?,
        from: Addr { name: from_name, email: from_addr },
        to: serde_json::from_str(&to).unwrap_or_default(),
        subject: r.get(6)?,
        received: r.get(7)?,
        keywords: kw.split(' ').filter(|s| !s.is_empty()).map(String::from).collect(),
        has_att: r.get::<_, i64>(9)? != 0,
        encrypted: r.get::<_, i64>(10)? != 0,
        fetched: r.get(11)?,
        preview: r.get(12)?,
        size: r.get(13)?,
        mailboxes: mbx.unwrap_or_default().split(' ').filter(|s| !s.is_empty()).map(String::from).collect(),
    })
}

fn account_of(r: &Row) -> rusqlite::Result<Account> {
    let ids: String = r.get(8)?;
    let doms: String = r.get(9)?;
    Ok(Account {
        id: r.get(0)?,
        name: r.get(1)?,
        server: r.get(2)?,
        user: r.get(3)?,
        email: r.get(4)?,
        display_name: r.get(5)?,
        jmap_id: r.get(6)?,
        identity_id: r.get(7)?,
        identities: serde_json::from_str::<Vec<Identity>>(&ids).unwrap_or_default(),
        sender_domains: doms.split([' ', ',', '\n']).filter(|s| !s.is_empty()).map(|s| s.to_lowercase()).collect(),
        ord: r.get(10)?,
    })
}

const ACCOUNT_COLS: &str = "id, name, server, user, email, display_name, jmap_id, identity_id, identities, sender_domains, ord";

fn mailbox_of(r: &Row) -> rusqlite::Result<Mailbox> {
    Ok(Mailbox {
        account: r.get(0)?,
        id: r.get(1)?,
        name: r.get(2)?,
        role: r.get(3)?,
        parent: r.get(4)?,
        sort: r.get(5)?,
        total: r.get(6)?,
        unread: r.get(7)?,
        notify: r.get::<_, Option<i64>>(8)?.map(|v| v != 0),
    })
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

pub fn kw_string(kw: &[String]) -> String {
    let mut v: Vec<String> = kw.iter().map(|k| k.to_lowercase()).collect();
    v.sort();
    v.dedup();
    v.join(" ")
}

impl Db {
    pub fn open(path: &Path) -> Result<Db> {
        if let Some(dir) = path.parent() {
            crate::paths::ensure(dir)?;
        }
        let w = connect(path)?;
        w.execute_batch(SCHEMA)?;
        let r = connect(path)?;
        Ok(Db { path: path.to_path_buf(), w: Mutex::new(w), r: Mutex::new(r) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The writer.
    pub fn w(&self) -> MutexGuard<'_, Connection> {
        self.w.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The window's reader.
    pub fn r(&self) -> MutexGuard<'_, Connection> {
        self.r.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- settings ----

    pub fn setting(&self, key: &str) -> Option<String> {
        self.r().query_row("SELECT value FROM settings WHERE key = ?", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn setting_or(&self, key: &str, default: &str) -> String {
        self.setting(key).unwrap_or_else(|| default.to_string())
    }

    pub fn flag(&self, key: &str, default: bool) -> bool {
        self.setting(key).map(|v| v == "1").unwrap_or(default)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        self.w().execute("INSERT OR REPLACE INTO settings(key, value) VALUES (?, ?)", params![key, value])?;
        Ok(())
    }

    pub fn set_flag(&self, key: &str, on: bool) -> Result<()> {
        self.set_setting(key, if on { "1" } else { "0" })
    }

    // ---- accounts ----

    pub fn accounts(&self) -> Vec<Account> {
        let c = self.r();
        let mut st = c.prepare(&format!("SELECT {ACCOUNT_COLS} FROM accounts ORDER BY ord, id")).unwrap();
        st.query_map([], account_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn account(&self, id: i64) -> Option<Account> {
        self.r()
            .query_row(&format!("SELECT {ACCOUNT_COLS} FROM accounts WHERE id = ?"), [id], account_of)
            .optional()
            .ok()
            .flatten()
    }

    pub fn add_account(&self, a: &Account) -> Result<i64> {
        let c = self.w();
        let ord: i64 = c.query_row("SELECT COALESCE(MAX(ord), 0) + 1 FROM accounts", [], |r| r.get(0))?;
        c.execute(
            "INSERT INTO accounts(name, server, user, email, display_name, jmap_id, identity_id, identities, sender_domains, ord) VALUES (?,?,?,?,?,?,?,?,?,?)",
            params![a.name, a.server, a.user, a.email, a.display_name, a.jmap_id, a.identity_id, serde_json::to_string(&a.identities)?, a.sender_domains.join(" "), ord],
        )?;
        Ok(c.last_insert_rowid())
    }

    pub fn update_account(&self, a: &Account) -> Result<()> {
        self.w().execute(
            "UPDATE accounts SET name=?, server=?, user=?, email=?, display_name=?, jmap_id=?, identity_id=?, identities=?, sender_domains=? WHERE id=?",
            params![a.name, a.server, a.user, a.email, a.display_name, a.jmap_id, a.identity_id, serde_json::to_string(&a.identities)?, a.sender_domains.join(" "), a.id],
        )?;
        Ok(())
    }

    pub fn remove_account(&self, id: i64) -> Result<()> {
        let c = self.w();
        c.execute("DELETE FROM texts WHERE id IN (SELECT id FROM messages WHERE account = ?)", [id])?;
        c.execute("DELETE FROM seen_rcpt WHERE account = ?", [id])?;
        c.execute("DELETE FROM accounts WHERE id = ?", [id])?;
        drop(c);
        let _ = std::fs::remove_dir_all(crate::paths::mail_dir(id));
        Ok(())
    }

    pub fn email_state(&self, account: i64) -> Option<String> {
        self.w().query_row("SELECT email_state FROM accounts WHERE id = ?", [account], |r| r.get(0)).optional().ok().flatten().flatten()
    }

    pub fn set_email_state(&self, account: i64, state: Option<&str>) -> Result<()> {
        self.w().execute("UPDATE accounts SET email_state = ? WHERE id = ?", params![state, account])?;
        Ok(())
    }

    // ---- mailboxes ----

    /// Replaces the account's folder list, keeping the notify choices.
    pub fn save_mailboxes(&self, account: i64, list: &[Mailbox]) -> Result<bool> {
        let before = self.mailboxes(account);
        let mut c = self.w();
        let tx = c.transaction()?;
        let keep: std::collections::HashMap<String, Option<bool>> = before.iter().map(|m| (m.id.clone(), m.notify)).collect();
        tx.execute("DELETE FROM mailboxes WHERE account = ?", [account])?;
        for m in list {
            let n = keep.get(&m.id).cloned().flatten();
            tx.execute(
                "INSERT INTO mailboxes(account, id, name, role, parent, sort, total, unread, notify) VALUES (?,?,?,?,?,?,?,?,?)",
                params![account, m.id, m.name, m.role, m.parent, m.sort, m.total, m.unread, n.map(|b| b as i64)],
            )?;
        }
        // messages that lost every folder (folder deleted) go with it
        let ids: Vec<String> = list.iter().map(|m| m.id.clone()).collect();
        if !ids.is_empty() {
            let marks = vec!["?"; ids.len()].join(",");
            let sql = format!("DELETE FROM msg_mailbox WHERE account = ? AND mailbox NOT IN ({marks})");
            let mut p: Vec<&dyn rusqlite::ToSql> = vec![&account];
            for i in &ids {
                p.push(i);
            }
            tx.execute(&sql, p.as_slice())?;
        }
        tx.commit()?;
        let after: Vec<Mailbox> = list.iter().map(|m| Mailbox { notify: keep.get(&m.id).cloned().flatten(), ..m.clone() }).collect();
        Ok(before != after)
    }

    pub fn mailboxes(&self, account: i64) -> Vec<Mailbox> {
        let c = self.r();
        let mut st = c
            .prepare("SELECT account, id, name, role, parent, sort, total, unread, notify FROM mailboxes WHERE account = ? ORDER BY sort, name")
            .unwrap();
        st.query_map([account], mailbox_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn all_mailboxes(&self) -> Vec<Mailbox> {
        let c = self.r();
        let mut st = c.prepare("SELECT account, id, name, role, parent, sort, total, unread, notify FROM mailboxes ORDER BY account, sort, name").unwrap();
        st.query_map([], mailbox_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn mailbox_by_role(&self, account: i64, role: &str) -> Option<Mailbox> {
        self.mailboxes(account).into_iter().find(|m| m.role.as_deref() == Some(role))
    }

    pub fn set_notify(&self, account: i64, mailbox: &str, on: bool) -> Result<()> {
        self.w().execute("UPDATE mailboxes SET notify = ? WHERE account = ? AND id = ?", params![on as i64, account, mailbox])?;
        Ok(())
    }

    // ---- messages ----

    /// Inserts or updates header rows; returns the local ids of the new ones.
    pub fn save_meta(&self, account: i64, list: &[Meta]) -> Result<Vec<i64>> {
        if list.is_empty() {
            return Ok(vec![]);
        }
        let outgoing: Vec<String> = self
            .mailboxes(account)
            .into_iter()
            .filter(|m| matches!(m.role.as_deref(), Some("sent") | Some("drafts")))
            .map(|m| m.id)
            .collect();
        let mut fresh = Vec::new();
        let mut c = self.w();
        let tx = c.transaction()?;
        for e in list {
            let existing: Option<i64> =
                tx.query_row("SELECT id FROM messages WHERE account = ? AND jid = ?", params![account, e.jid], |r| r.get(0)).optional()?;
            let from = e.from.first().cloned().unwrap_or_default();
            let id = match existing {
                Some(id) => {
                    tx.execute("UPDATE messages SET keywords = ? WHERE id = ?", params![kw_string(&e.keywords), id])?;
                    id
                }
                None => {
                    tx.execute(
                        "INSERT INTO messages(account, jid, blob, thread, keywords, has_att, from_name, from_addr, to_json, cc_json, subject, received, sent, size, preview, kind, msgid) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
                        params![
                            account, e.jid, e.blob, e.thread, kw_string(&e.keywords), e.has_att as i64, from.name, from.email,
                            serde_json::to_string(&e.to)?, serde_json::to_string(&e.cc)?, e.subject, e.received, e.sent, e.size,
                            e.preview, e.kind, e.msgid
                        ],
                    )?;
                    let id = tx.last_insert_rowid();
                    fresh.push(id);
                    let out = e.mailboxes.iter().any(|m| outgoing.contains(m));
                    // mail sent to exactly one address that reached us: that address is one of ours
                    if e.to.len() == 1 && !out {
                        let a = e.to[0].email.to_lowercase();
                        tx.execute(
                            "INSERT INTO seen_rcpt(account, addr, n) VALUES (?, ?, 1) ON CONFLICT(account, addr) DO UPDATE SET n = n + 1",
                            params![account, a],
                        )?;
                    }
                    // people we hear from or write to, for completion
                    let people: Vec<&Addr> = if out { e.to.iter().chain(e.cc.iter()).collect() } else { e.from.iter().collect() };
                    for p in people {
                        if p.email.is_empty() {
                            continue;
                        }
                        tx.execute(
                            "INSERT INTO contacts(addr, name, n, last) VALUES (?, ?, 1, ?) ON CONFLICT(addr) DO UPDATE SET n = n + 1, last = MAX(last, excluded.last), name = COALESCE(excluded.name, name)",
                            params![p.email.to_lowercase(), p.name, e.received],
                        )?;
                    }
                    id
                }
            };
            tx.execute("DELETE FROM msg_mailbox WHERE msg = ?", [id])?;
            for m in &e.mailboxes {
                tx.execute("INSERT OR IGNORE INTO msg_mailbox(msg, account, mailbox, received) VALUES (?,?,?,?)", params![id, account, m, e.received])?;
            }
        }
        tx.commit()?;
        Ok(fresh)
    }

    /// Removes messages by server id; returns their local ids (the files go too).
    pub fn remove_jids(&self, account: i64, jids: &[String]) -> Result<Vec<i64>> {
        let mut gone = Vec::new();
        let mut c = self.w();
        let tx = c.transaction()?;
        for j in jids {
            if let Some(id) = tx.query_row("SELECT id FROM messages WHERE account = ? AND jid = ?", params![account, j], |r| r.get::<_, i64>(0)).optional()? {
                tx.execute("DELETE FROM texts WHERE id = ?", [id])?;
                tx.execute("DELETE FROM messages WHERE id = ?", [id])?;
                gone.push(id);
            }
        }
        tx.commit()?;
        for id in &gone {
            let _ = std::fs::remove_file(crate::paths::eml_path(account, *id));
        }
        Ok(gone)
    }

    pub fn all_jids(&self, account: i64) -> Vec<String> {
        let c = self.w();
        let mut st = c.prepare("SELECT jid FROM messages WHERE account = ?").unwrap();
        st.query_map([account], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn msg(&self, id: i64) -> Option<MsgRow> {
        self.r().query_row(&format!("SELECT {ROW_COLS} FROM messages m WHERE m.id = ?"), [id], row_of).optional().ok().flatten()
    }

    pub fn msg_by_jid(&self, account: i64, jid: &str) -> Option<MsgRow> {
        self.r()
            .query_row(&format!("SELECT {ROW_COLS} FROM messages m WHERE m.account = ? AND m.jid = ?"), params![account, jid], row_of)
            .optional()
            .ok()
            .flatten()
    }

    /// A folder, newest first.
    pub fn list(&self, account: i64, mailbox: &str, limit: i64) -> Vec<MsgRow> {
        let c = self.r();
        let sql = format!(
            "SELECT {ROW_COLS} FROM msg_mailbox b JOIN messages m ON m.id = b.msg WHERE b.account = ? AND b.mailbox = ? ORDER BY b.received DESC, m.id DESC LIMIT ?"
        );
        let mut st = c.prepare(&sql).unwrap();
        st.query_map(params![account, mailbox, limit], row_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Every account's folder with [role] together (all inboxes), newest first.
    pub fn list_role(&self, role: &str, limit: i64) -> Vec<MsgRow> {
        let c = self.r();
        let sql = format!(
            "SELECT {ROW_COLS} FROM msg_mailbox b JOIN mailboxes x ON x.account = b.account AND x.id = b.mailbox JOIN messages m ON m.id = b.msg WHERE x.role = ? ORDER BY b.received DESC, m.id DESC LIMIT ?"
        );
        let mut st = c.prepare(&sql).unwrap();
        st.query_map(params![role, limit], row_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Flagged messages of every account.
    pub fn list_flagged(&self, limit: i64) -> Vec<MsgRow> {
        let c = self.r();
        let sql = format!("SELECT {ROW_COLS} FROM messages m WHERE (' ' || m.keywords || ' ') LIKE '% $flagged %' ORDER BY m.received DESC LIMIT ?");
        let mut st = c.prepare(&sql).unwrap();
        st.query_map(params![limit], row_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn msgs(&self, ids: &[i64]) -> Vec<MsgRow> {
        ids.iter().filter_map(|i| self.msg(*i)).collect()
    }

    /// Messages still to download, newest first.
    pub fn pending(&self, account: i64, limit: i64) -> Vec<(i64, String, i64)> {
        let c = self.w();
        let mut st = c.prepare("SELECT id, blob, size FROM messages WHERE account = ? AND fetched = 0 ORDER BY received DESC LIMIT ?").unwrap();
        st.query_map(params![account, limit], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Stored messages that are still encrypted (no key, or it was locked).
    pub fn still_encrypted(&self, account: i64) -> Vec<i64> {
        let c = self.w();
        let mut st = c.prepare("SELECT id FROM messages WHERE account = ? AND fetched = 2 ORDER BY received DESC").unwrap();
        st.query_map([account], |r| r.get(0)).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn counts(&self, account: i64) -> (i64, i64) {
        self.r()
            .query_row("SELECT COUNT(*), COALESCE(SUM(fetched <> 0), 0) FROM messages WHERE account = ?", [account], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap_or((0, 0))
    }

    pub fn total_messages(&self) -> i64 {
        self.r().query_row("SELECT COUNT(*) FROM texts", [], |r| r.get(0)).unwrap_or(0)
    }

    pub fn store_processed(&self, list: &[Stored]) -> Result<()> {
        let mut c = self.w();
        let tx = c.transaction()?;
        for s in list {
            tx.execute(
                "UPDATE messages SET fetched = ?, encrypted = ?, pgp = ?, err = ?, has_att = CASE WHEN ? THEN 1 ELSE has_att END, preview = COALESCE(?, preview) WHERE id = ?",
                params![s.fetched, s.encrypted as i64, s.pgp, s.err, s.has_att, s.preview, s.id],
            )?;
            if let Some(t) = &s.text {
                tx.execute(
                    "INSERT OR REPLACE INTO texts(id, subject, sender, rcpts, headers, body, attach) VALUES (?,?,?,?,?,?,?)",
                    params![s.id, t.subject, t.sender, t.rcpts, t.headers, t.body, t.attach],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// The PGP line and error of a stored message.
    pub fn pgp_info(&self, id: i64) -> (Option<String>, Option<String>) {
        self.r().query_row("SELECT pgp, err FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?))).unwrap_or((None, None))
    }

    pub fn blob_of(&self, id: i64) -> Option<(i64, String, String)> {
        self.r().query_row("SELECT account, jid, blob FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).optional().ok().flatten()
    }

    pub fn set_keywords(&self, id: i64, add: &[&str], remove: &[&str]) -> Result<()> {
        let Some(m) = self.msg(id) else { return Ok(()) };
        let mut kw: Vec<String> = m.keywords.into_iter().filter(|k| !remove.iter().any(|r| r.eq_ignore_ascii_case(k))).collect();
        for a in add {
            kw.push(a.to_string());
        }
        self.w().execute("UPDATE messages SET keywords = ? WHERE id = ?", params![kw_string(&kw), id])?;
        Ok(())
    }

    pub fn set_mailboxes_local(&self, id: i64, mailboxes: &[String]) -> Result<()> {
        let mut c = self.w();
        let tx = c.transaction()?;
        let (account, received): (i64, i64) = tx.query_row("SELECT account, received FROM messages WHERE id = ?", [id], |r| Ok((r.get(0)?, r.get(1)?)))?;
        tx.execute("DELETE FROM msg_mailbox WHERE msg = ?", [id])?;
        for m in mailboxes {
            tx.execute("INSERT OR IGNORE INTO msg_mailbox(msg, account, mailbox, received) VALUES (?,?,?,?)", params![id, account, m, received])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn find_by_msgid(&self, account: i64, msgid: &str) -> Option<i64> {
        self.r().query_row("SELECT id FROM messages WHERE account = ? AND msgid = ?", params![account, msgid], |r| r.get(0)).optional().ok().flatten()
    }

    // ---- addresses ----

    pub fn seen_rcpts(&self, account: i64) -> Vec<(String, i64)> {
        let c = self.r();
        let mut st = c.prepare("SELECT addr, n FROM seen_rcpt WHERE account = ? ORDER BY n DESC, addr").unwrap();
        st.query_map([account], |r| Ok((r.get(0)?, r.get(1)?))).unwrap().filter_map(|r| r.ok()).collect()
    }

    /// Addresses for completion: what starts with or contains [prefix], the most used first.
    pub fn contacts(&self, prefix: &str, limit: i64) -> Vec<Addr> {
        let c = self.r();
        let like = format!("%{}%", prefix.to_lowercase().replace('%', "").replace('_', ""));
        let mut st = c
            .prepare("SELECT addr, name FROM contacts WHERE addr LIKE ?1 OR lower(name) LIKE ?1 ORDER BY (addr LIKE ?2) DESC, n DESC, last DESC LIMIT ?3")
            .unwrap();
        let starts = format!("{}%", prefix.to_lowercase());
        st.query_map(params![like, starts, limit], |r| Ok(Addr { email: r.get(0)?, name: r.get(1)? })).unwrap().filter_map(|r| r.ok()).collect()
    }

    // ---- keys ----

    pub fn keys(&self) -> Vec<(String, String, bool)> {
        let c = self.r();
        let mut st = c.prepare("SELECT fpr, armored, secret FROM keys ORDER BY secret DESC, added").unwrap();
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0))).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn save_key(&self, fpr: &str, armored: &str, secret: bool) -> Result<()> {
        self.w().execute(
            "INSERT INTO keys(fpr, armored, secret, added) VALUES (?,?,?,?) ON CONFLICT(fpr) DO UPDATE SET armored = CASE WHEN excluded.secret >= secret THEN excluded.armored ELSE armored END, secret = MAX(secret, excluded.secret)",
            params![fpr, armored, secret as i64, now()],
        )?;
        Ok(())
    }

    pub fn remove_key(&self, fpr: &str) -> Result<()> {
        self.w().execute("DELETE FROM keys WHERE fpr = ?", [fpr])?;
        Ok(())
    }
}
