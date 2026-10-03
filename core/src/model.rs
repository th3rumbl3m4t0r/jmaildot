//! The plain data the rest of the program passes around.

use serde::{Deserialize, Serialize};

/// A mail address with an optional display name.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addr {
    pub name: Option<String>,
    pub email: String,
}

impl Addr {
    pub fn new(name: Option<&str>, email: &str) -> Addr {
        Addr {
            name: name.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
            email: email.trim().to_string(),
        }
    }

    /// `Name <email>`, the name quoted when it has to be.
    pub fn full(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() && n != &self.email => {
                let special = n.chars().any(|c| "()<>[]:;@\\,.\"".contains(c));
                if special {
                    format!("\"{}\" <{}>", n.replace('\\', "\\\\").replace('"', "\\\""), self.email)
                } else {
                    format!("{} <{}>", n, self.email)
                }
            }
            _ => self.email.clone(),
        }
    }

    /// The name when there is one, else the address.
    pub fn short(&self) -> String {
        match &self.name {
            Some(n) if !n.is_empty() => n.clone(),
            _ => self.email.clone(),
        }
    }

    pub fn domain(&self) -> String {
        self.email.rsplit_once('@').map(|(_, d)| d.to_lowercase()).unwrap_or_default()
    }

    /// One address as typed: `a@b`, `Name <a@b>`, `"Name, Jr." <a@b>`.
    pub fn parse(s: &str) -> Option<Addr> {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        if let (Some(lt), true) = (s.rfind('<'), s.ends_with('>')) {
            let email = s[lt + 1..s.len() - 1].trim();
            let name = s[..lt].trim().trim_matches('"').replace("\\\"", "\"");
            if !email.contains('@') {
                return None;
            }
            return Some(Addr::new(Some(&name), email));
        }
        if s.contains('@') && !s.contains(' ') {
            return Some(Addr::new(None, s));
        }
        None
    }

    /// A comma (or semicolon) separated list; commas inside quotes stay part of the name.
    pub fn parse_list(s: &str) -> (Vec<Addr>, Vec<String>) {
        let mut out = Vec::new();
        let mut bad = Vec::new();
        let mut cur = String::new();
        let mut quoted = false;
        let mut angle = false;
        for c in s.chars() {
            match c {
                '"' => {
                    quoted = !quoted;
                    cur.push(c)
                }
                '<' if !quoted => {
                    angle = true;
                    cur.push(c)
                }
                '>' if !quoted => {
                    angle = false;
                    cur.push(c)
                }
                ',' | ';' | '\n' if !quoted && !angle => {
                    push_one(&cur, &mut out, &mut bad);
                    cur.clear();
                }
                _ => cur.push(c),
            }
        }
        push_one(&cur, &mut out, &mut bad);
        (out, bad)
    }

    pub fn join(list: &[Addr]) -> String {
        list.iter().map(|a| a.full()).collect::<Vec<_>>().join(", ")
    }
}

fn push_one(s: &str, out: &mut Vec<Addr>, bad: &mut Vec<String>) {
    let t = s.trim();
    if t.is_empty() {
        return;
    }
    match Addr::parse(t) {
        Some(a) => out.push(a),
        None => bad.push(t.to_string()),
    }
}

/// A sending identity the server knows.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Identity {
    pub id: String,
    pub name: Option<String>,
    pub email: String,
}

#[derive(Clone, Debug)]
pub struct Account {
    pub id: i64,
    /// What the user calls it (defaults to the address).
    pub name: String,
    pub server: String,
    pub user: String,
    /// The main identity's address.
    pub email: String,
    pub display_name: Option<String>,
    pub jmap_id: String,
    pub identity_id: Option<String>,
    pub identities: Vec<Identity>,
    /// Domains that are ours besides the identities' (settings → sending).
    pub sender_domains: Vec<String>,
    pub ord: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mailbox {
    pub account: i64,
    pub id: String,
    pub name: String,
    pub role: Option<String>,
    pub parent: Option<String>,
    pub sort: i64,
    pub total: i64,
    pub unread: i64,
    /// Notify about new mail here; None = the default (inbox only).
    pub notify: Option<bool>,
}

impl Mailbox {
    pub fn notifies(&self) -> bool {
        self.notify.unwrap_or(self.role.as_deref() == Some("inbox"))
    }
}

/// One message as the lists show it.
#[derive(Clone, Debug)]
pub struct MsgRow {
    pub id: i64,
    pub account: i64,
    pub jid: String,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub subject: String,
    /// Seconds since 1970.
    pub received: i64,
    pub keywords: Vec<String>,
    pub has_att: bool,
    pub encrypted: bool,
    /// 0 not downloaded yet, 1 stored, 2 stored but still encrypted (no key / locked), 3 failed.
    pub fetched: i64,
    pub preview: String,
    pub size: i64,
    pub mailboxes: Vec<String>,
}

impl MsgRow {
    pub fn has_kw(&self, k: &str) -> bool {
        self.keywords.iter().any(|x| x.eq_ignore_ascii_case(k))
    }
    pub fn seen(&self) -> bool {
        self.has_kw("$seen")
    }
    pub fn flagged(&self) -> bool {
        self.has_kw("$flagged")
    }
    pub fn answered(&self) -> bool {
        self.has_kw("$answered")
    }
    pub fn draft(&self) -> bool {
        self.has_kw("$draft")
    }
}

/// What a sync engine tells the window.
#[derive(Clone, Debug)]
pub enum Event {
    /// Folders or messages of the account changed: lists should reload.
    Changed(i64),
    /// Download progress of the account: (stored, total).
    Progress(i64, i64, i64),
    /// A line for the status bar; error = true marks it red.
    Status(i64, String, bool),
    /// New unread mail in a watched folder (message ids).
    NewMail(i64, Vec<i64>),
    /// Calendars or events of the account changed.
    CalChanged(i64),
    /// A reminder is due: (event title, when, where).
    Reminder(String, String, Option<String>),
    /// Encrypted messages wait for the key's passphrase.
    NeedPassphrase,
}
