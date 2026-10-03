//! The engine the window talks to: accounts and their sync loops, message actions, opening,
//! sending, keys, Sieve, shelf. Network work runs on a tokio runtime; the window gets
//! [Event]s over a channel and calls the async functions through `spawn`.

use crate::caldav::CalDav;
use crate::db::Db;
use crate::jmap::{self, Jmap};
use crate::mime::{self, FileAtt, Headers, Parsed, PgpNote};
use crate::model::{Account, Addr, Event, MsgRow};
use crate::pgp::{self, KeyRing};
use crate::{paths, secrets, senders};
use anyhow::{anyhow, bail, Result};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};

pub struct Acct {
    pub id: i64,
    pub account: RwLock<Account>,
    pub jmap: Jmap,
    pub dav: CalDav,
    pub kick: tokio::sync::Notify,
    pub cal_kick: tokio::sync::Notify,
    pub stop: AtomicBool,
    pub syncing: AtomicBool,
    pub push_ok: AtomicBool,
}

impl Acct {
    pub fn account(&self) -> Account {
        self.account.read().unwrap().clone()
    }
}

pub struct Engine {
    pub db: Arc<Db>,
    pub rt: tokio::runtime::Runtime,
    pub keys: Arc<RwLock<KeyRing>>,
    accts: Mutex<HashMap<i64, Arc<Acct>>>,
    tx: async_channel::Sender<Event>,
    statuses: Mutex<HashMap<i64, (String, bool)>>,
}

/// A message opened for reading.
pub struct Opened {
    pub row: MsgRow,
    pub parsed: Parsed,
    pub note: Option<PgpNote>,
    pub err: Option<String>,
    pub raw: Vec<u8>,
}

/// A message on its way out.
#[derive(Clone)]
pub struct Outgoing {
    pub account: i64,
    /// The local id of the draft this replaces.
    pub draft: Option<i64>,
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub subject: String,
    pub body: String,
    pub files: Vec<(String, String, Vec<u8>)>,
    pub encrypt: bool,
    pub sign: bool,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    /// The message being answered (gets $answered).
    pub answering: Option<i64>,
}

pub fn password_key(account: i64) -> String {
    format!("account-{account}")
}

impl Engine {
    pub fn new() -> Result<(Arc<Engine>, async_channel::Receiver<Event>)> {
        paths::ensure(&paths::data_dir())?;
        let db = Arc::new(Db::open(&paths::db_path())?);
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(4).enable_all().thread_name("maildot-net").build()?;
        let (tx, rx) = async_channel::unbounded();
        let certs: Vec<_> = db.keys().into_iter().filter_map(|(_, a, _)| pgp::parse_certs(a.as_bytes()).ok()).flatten().collect();
        let e = Arc::new(Engine { db, rt, keys: Arc::new(RwLock::new(KeyRing::new(certs))), accts: Mutex::new(HashMap::new()), tx, statuses: Mutex::new(HashMap::new()) });
        if e.db.flag("pgp.remember", false) {
            if let Some(p) = secrets::get("pgp-passphrase") {
                e.keys.write().unwrap().unlock(&p);
            }
        }
        Ok((e, rx))
    }

    pub fn emit(&self, ev: Event) {
        if let Event::Status(a, s, bad) = &ev {
            self.statuses.lock().unwrap().insert(*a, (s.clone(), *bad));
        }
        let _ = self.tx.try_send(ev);
    }

    pub fn status(&self, account: i64, s: &str, bad: bool) {
        self.emit(Event::Status(account, s.to_string(), bad));
    }

    pub fn statuses(&self) -> HashMap<i64, (String, bool)> {
        self.statuses.lock().unwrap().clone()
    }

    pub fn acct(&self, id: i64) -> Option<Arc<Acct>> {
        self.accts.lock().unwrap().get(&id).cloned()
    }

    pub fn accounts(&self) -> Vec<Account> {
        self.db.accounts()
    }

    /// Starts every account's sync loops (at program start).
    pub fn start_all(self: &Arc<Self>) {
        self.rt.spawn(self.clone().reminders());
        for a in self.db.accounts() {
            if self.acct(a.id).is_some() {
                continue;
            }
            if let Err(err) = self.start(a.id) {
                self.status(a.id, &format!("{err}"), true);
            }
        }
    }

    pub fn start(self: &Arc<Self>, id: i64) -> Result<()> {
        let a = self.db.account(id).ok_or_else(|| anyhow!("no such account"))?;
        let pass = secrets::get(&password_key(id)).ok_or_else(|| anyhow!("no password stored: set it in settings → accounts"))?;
        let acct = Arc::new(Acct {
            id,
            jmap: Jmap::new(&a.server, &a.user, &pass)?,
            dav: CalDav::new(&a.server, &a.user, &pass)?,
            account: RwLock::new(a),
            kick: tokio::sync::Notify::new(),
            cal_kick: tokio::sync::Notify::new(),
            stop: AtomicBool::new(false),
            syncing: AtomicBool::new(false),
            push_ok: AtomicBool::new(false),
        });
        if let Some(old) = self.accts.lock().unwrap().insert(id, acct.clone()) {
            old.stop.store(true, Ordering::Relaxed);
            old.kick.notify_one();
            old.cal_kick.notify_one();
        }
        self.rt.spawn(crate::sync::run(self.clone(), acct.clone()));
        self.rt.spawn(crate::cal::run(self.clone(), acct));
        Ok(())
    }

    pub fn stop_all(&self) {
        for a in self.accts.lock().unwrap().values() {
            a.stop.store(true, Ordering::Relaxed);
            a.kick.notify_one();
            a.cal_kick.notify_one();
        }
    }

    pub fn sync_now(&self, id: Option<i64>) {
        for (k, a) in self.accts.lock().unwrap().iter() {
            if id.is_none() || id == Some(*k) {
                a.kick.notify_one();
                a.cal_kick.notify_one();
            }
        }
    }

    pub fn syncing(&self) -> bool {
        self.accts.lock().unwrap().values().any(|a| a.syncing.load(Ordering::Relaxed))
    }

    /// Signs in and adds the account; its sync starts at once.
    pub async fn add_account(self: &Arc<Self>, server: &str, user: &str, pass: &str) -> Result<i64> {
        let j = Jmap::new(server, user, pass)?;
        let s = j.connect().await?;
        let ids = j.identities().await.unwrap_or_default();
        let me = ids.iter().find(|i| i.email.eq_ignore_ascii_case(user)).or(ids.first()).cloned();
        let email = me.as_ref().map(|i| i.email.clone()).unwrap_or_else(|| if user.contains('@') { user.to_string() } else { String::new() });
        if email.is_empty() {
            bail!("this account has no sending identity");
        }
        if self.db.accounts().iter().any(|a| a.server == j.base && a.user == user) {
            bail!("that account is here already");
        }
        let display = me.as_ref().and_then(|i| i.name.clone()).filter(|n| !n.contains('@')).or_else(|| Some(s.name.clone()).filter(|n| !n.is_empty() && !n.contains('@')));
        let a = Account {
            id: 0,
            name: email.clone(),
            server: j.base.clone(),
            user: user.to_string(),
            email,
            display_name: display,
            jmap_id: s.account_id.clone(),
            identity_id: me.map(|i| i.id),
            identities: ids,
            sender_domains: vec![],
            ord: 0,
        };
        let id = self.db.add_account(&a)?;
        secrets::set(&password_key(id), pass)?;
        self.start(id)?;
        Ok(id)
    }

    pub async fn set_password(self: &Arc<Self>, id: i64, pass: &str) -> Result<()> {
        let a = self.db.account(id).ok_or_else(|| anyhow!("no such account"))?;
        let j = Jmap::new(&a.server, &a.user, pass)?;
        j.connect().await?;
        secrets::set(&password_key(id), pass)?;
        self.start(id)
    }

    pub fn remove_account(&self, id: i64) -> Result<()> {
        if let Some(a) = self.accts.lock().unwrap().remove(&id) {
            a.stop.store(true, Ordering::Relaxed);
            a.kick.notify_one();
            a.cal_kick.notify_one();
        }
        secrets::remove(&password_key(id));
        self.db.remove_account(id)
    }

    pub fn save_account(&self, a: &Account) -> Result<()> {
        self.db.update_account(a)?;
        if let Some(x) = self.acct(a.id) {
            let mut w = x.account.write().unwrap();
            w.name = a.name.clone();
            w.display_name = a.display_name.clone();
            w.sender_domains = a.sender_domains.clone();
        }
        Ok(())
    }

    // ---- keys ----

    pub fn reload_keys(&self) {
        let certs: Vec<_> = self.db.keys().into_iter().filter_map(|(_, a, _)| pgp::parse_certs(a.as_bytes()).ok()).flatten().collect();
        let mut k = self.keys.write().unwrap();
        let mut fresh = KeyRing::new(certs);
        // passphrases typed this session stay
        fresh.keep_passwords(&k);
        *k = fresh;
    }

    /// Imports every key in [data]; returns what was added.
    pub fn import_keys(self: &Arc<Self>, data: &[u8]) -> Result<Vec<pgp::KeyInfo>> {
        let certs = pgp::parse_certs(data)?;
        let mut out = Vec::new();
        for c in certs {
            self.db.save_key(&c.fingerprint().to_hex(), &pgp::armored(&c)?, c.is_tsk())?;
            out.push(pgp::info(&c));
        }
        self.reload_keys();
        self.after_unlock();
        Ok(out)
    }

    pub fn remove_key(&self, fpr: &str) -> Result<()> {
        self.db.remove_key(fpr)?;
        self.reload_keys();
        Ok(())
    }

    pub fn key_infos(&self) -> Vec<pgp::KeyInfo> {
        self.keys.read().unwrap().certs.iter().map(pgp::info).collect()
    }

    pub fn locked(&self) -> bool {
        self.keys.read().unwrap().locked()
    }

    /// Tries the passphrase; when it opens a key, stored encrypted messages are decrypted.
    pub fn unlock(self: &Arc<Self>, pass: &str, remember: bool) -> bool {
        let ok = self.keys.write().unwrap().unlock(pass);
        if ok {
            let _ = self.db.set_flag("pgp.remember", remember);
            if remember {
                let _ = secrets::set("pgp-passphrase", pass);
            } else {
                secrets::remove("pgp-passphrase");
            }
            self.after_unlock();
        }
        ok
    }

    fn after_unlock(self: &Arc<Self>) {
        let me = self.clone();
        self.rt.spawn(async move {
            for a in me.db.accounts() {
                let _ = crate::sync::reprocess(&me, a.id).await;
            }
        });
    }

    // ---- message actions: the database first (the window follows), then the server ----

    fn by_account(&self, ids: &[i64]) -> HashMap<i64, Vec<MsgRow>> {
        let mut m: HashMap<i64, Vec<MsgRow>> = HashMap::new();
        for r in self.db.msgs(ids) {
            m.entry(r.account).or_default().push(r);
        }
        m
    }

    fn acct_or_err(&self, id: i64) -> Result<Arc<Acct>> {
        self.acct(id).ok_or_else(|| anyhow!("the account is not connected"))
    }

    pub async fn set_keyword(self: &Arc<Self>, ids: Vec<i64>, kw: &str, on: bool) -> Result<()> {
        for (account, rows) in self.by_account(&ids) {
            let mut upd = Map::new();
            for r in &rows {
                if on {
                    self.db.set_keywords(r.id, &[kw], &[])?;
                } else {
                    self.db.set_keywords(r.id, &[], &[kw])?;
                }
                let mut p = Map::new();
                p.insert(format!("keywords/{kw}"), if on { Value::Bool(true) } else { Value::Null });
                upd.insert(r.jid.clone(), Value::Object(p));
            }
            self.emit(Event::Changed(account));
            let a = self.acct_or_err(account)?;
            let r = a.jmap.email_set(upd, vec![]).await;
            a.kick.notify_one();
            r?;
        }
        Ok(())
    }

    /// Moves to [mailbox] (out of every other folder).
    pub async fn move_to(self: &Arc<Self>, ids: Vec<i64>, account: i64, mailbox: &str) -> Result<()> {
        let rows: Vec<MsgRow> = self.db.msgs(&ids).into_iter().filter(|r| r.account == account).collect();
        let mut upd = Map::new();
        for r in &rows {
            self.db.set_mailboxes_local(r.id, &[mailbox.to_string()])?;
            let mut p = Map::new();
            let mut mb = Map::new();
            mb.insert(mailbox.to_string(), Value::Bool(true));
            p.insert("mailboxIds".into(), Value::Object(mb));
            upd.insert(r.jid.clone(), Value::Object(p));
        }
        self.emit(Event::Changed(account));
        let a = self.acct_or_err(account)?;
        let r = a.jmap.email_set(upd, vec![]).await;
        a.kick.notify_one();
        r
    }

    /// To the trash, or gone for good when there already.
    pub async fn trash(self: &Arc<Self>, ids: Vec<i64>) -> Result<()> {
        for (account, rows) in self.by_account(&ids) {
            let trash = self.db.mailbox_by_role(account, "trash");
            let (gone, moved): (Vec<MsgRow>, Vec<MsgRow>) =
                rows.into_iter().partition(|r| trash.as_ref().map(|t| r.mailboxes.contains(&t.id)).unwrap_or(true));
            if !moved.is_empty() {
                let t = trash.as_ref().unwrap().id.clone();
                self.move_to(moved.iter().map(|r| r.id).collect(), account, &t).await?;
            }
            if !gone.is_empty() {
                self.destroy(account, gone.iter().map(|r| r.jid.clone()).collect()).await?;
            }
        }
        Ok(())
    }

    pub async fn destroy(self: &Arc<Self>, account: i64, jids: Vec<String>) -> Result<()> {
        self.db.remove_jids(account, &jids)?;
        self.emit(Event::Changed(account));
        let a = self.acct_or_err(account)?;
        let r = a.jmap.email_set(Map::new(), jids).await;
        a.kick.notify_one();
        r
    }

    // ---- reading ----

    /// The stored message; fetched right now when it isn't stored yet.
    pub async fn open(self: &Arc<Self>, id: i64) -> Result<Opened> {
        let row = self.db.msg(id).ok_or_else(|| anyhow!("the message is gone"))?;
        let path = paths::eml_path(row.account, id);
        let needs = row.fetched == 0 || row.fetched == 3 || !path.exists();
        if needs {
            let (_, _, blob) = self.db.blob_of(id).ok_or_else(|| anyhow!("the message is gone"))?;
            let a = self.acct_or_err(row.account)?;
            let raw = a.jmap.download(&blob, "message.eml", "message/rfc822").await?;
            crate::sync::store(self, row.account, vec![(id, Some(raw))]).await?;
        } else if row.fetched == 2 && !self.locked() {
            let raw = std::fs::read(&path)?;
            crate::sync::store(self, row.account, vec![(id, Some(raw))]).await?;
        }
        let row = self.db.msg(id).ok_or_else(|| anyhow!("the message is gone"))?;
        let raw = std::fs::read(&path)?;
        let (pgp, err) = self.db.pgp_info(id);
        let ring = self.keys.read().unwrap().clone();
        let raw2 = raw.clone();
        let parsed = tokio::task::spawn_blocking(move || {
            let mut p = mime::parse(&raw2);
            // inline PGP is decrypted for reading (it is stored as it came)
            if let Some(t) = &p.text {
                if t.contains("-----BEGIN PGP MESSAGE-----") {
                    if let Some(d) = mime::decrypt_inline(t, &ring) {
                        p.text = Some(d);
                    }
                }
            }
            p
        })
        .await?;
        Ok(Opened { row, parsed, note: pgp.and_then(|s| serde_json::from_str(&s).ok()), err, raw })
    }

    /// An image a message refers to (only when the user let it load online content).
    pub async fn fetch_bytes(&self, url: &str, max: usize) -> Result<Vec<u8>> {
        let u = if let Some(rest) = url.strip_prefix("//") { format!("https://{rest}") } else { url.to_string() };
        if !(u.starts_with("http://") || u.starts_with("https://")) {
            bail!("not a web address");
        }
        let mut r = jmap::http_client().get(&u).timeout(std::time::Duration::from_secs(30)).send().await.map_err(jmap::describe_reqwest)?;
        if !r.status().is_success() {
            bail!("{} said {}", u, r.status().as_u16());
        }
        let mut out = Vec::new();
        while let Some(chunk) = r.chunk().await.map_err(jmap::describe_reqwest)? {
            out.extend_from_slice(&chunk);
            if out.len() > max {
                bail!("too big");
            }
        }
        Ok(out)
    }

    pub fn mailbox_name(&self, account: i64, id: &str) -> String {
        self.db.mailboxes(account).into_iter().find(|m| m.id == id).map(|m| m.name).unwrap_or_default()
    }

    // ---- sending ----

    fn build(&self, o: &Outgoing, self_only: bool) -> Result<Vec<u8>> {
        let files: Vec<FileAtt> = o.files.iter().map(|(n, m, d)| FileAtt { name: n.clone(), mime: m.clone(), data: d.clone() }).collect();
        let mut entity = mime::body_entity(&o.body, &files);
        let ring = self.keys.read().unwrap();
        let ours = ring.ours().cloned();
        if o.encrypt || self_only {
            let mut to = Vec::new();
            if !self_only {
                for a in o.to.iter().chain(o.cc.iter()).chain(o.bcc.iter()) {
                    to.push(ring.for_email(&a.email).ok_or_else(|| anyhow!("no key for {}", a.email))?.clone());
                }
            }
            if let Some(c) = &ours {
                to.push(c.clone());
            }
            if to.is_empty() {
                bail!("no key to encrypt to");
            }
            let refs: Vec<&sequoia_openpgp::Cert> = to.iter().collect();
            let signer = if o.sign && !self_only { ours.as_ref() } else { None };
            entity = mime::encrypted_entity(&ring.encrypt(&entity, &refs, signer)?);
        } else if o.sign {
            if let Some(c) = &ours {
                let canon = mime::crlf(&entity);
                let sig = ring.sign_detached(&canon, c)?;
                entity = mime::signed_entity(&canon, &sig);
            }
        }
        let domain = o.from.email.rsplit_once('@').map(|(_, d)| d.to_string()).unwrap_or_default();
        Ok(mime::message(
            &Headers {
                from: o.from.clone(),
                to: o.to.clone(),
                cc: o.cc.clone(),
                subject: o.subject.clone(),
                message_id: mime::message_id(&domain),
                in_reply_to: o.in_reply_to.clone(),
                references: o.references.clone(),
            },
            &entity,
        ))
    }

    /// What encrypting would need: (every recipient has a key, we have a key).
    pub fn can_encrypt(&self, rcpts: &[Addr]) -> (bool, bool) {
        let ring = self.keys.read().unwrap();
        (!rcpts.is_empty() && rcpts.iter().all(|a| ring.for_email(&a.email).is_some()), ring.ours().is_some())
    }

    /// Sends; the copy lands in Sent. An error means nothing went out.
    pub async fn send(self: &Arc<Self>, o: Outgoing) -> Result<()> {
        let mut rcpts: Vec<String> = o.to.iter().chain(o.cc.iter()).chain(o.bcc.iter()).map(|a| a.email.clone()).collect();
        rcpts.sort();
        rcpts.dedup();
        if rcpts.is_empty() {
            bail!("no recipient");
        }
        let msg = self.build(&o, false)?;
        let a = self.acct_or_err(o.account)?;
        let drafts = self.db.mailbox_by_role(o.account, "drafts");
        let sent = self.db.mailbox_by_role(o.account, "sent");
        let target = drafts.as_ref().or(sent.as_ref()).cloned().or_else(|| self.db.mailbox_by_role(o.account, "inbox")).ok_or_else(|| anyhow!("no folders yet"))?;
        let blob = a.jmap.upload(msg, "message/rfc822").await?;
        let jid = a.jmap.import(&blob, &target.id, &["$draft", "$seen"]).await?;
        // Stalwart wants the envelope sender to be the identity's own address; the From header
        // is free. An identity for the exact address is used when the server has (or lets us add) one.
        let acc = a.account();
        let mut identity = acc.identities.iter().find(|i| i.email.eq_ignore_ascii_case(&o.from.email)).cloned();
        if identity.is_none() && !o.from.email.eq_ignore_ascii_case(&acc.email) {
            if let Ok(Some(i)) = a.jmap.create_identity(o.from.name.as_deref(), &o.from.email).await {
                identity = Some(i);
            }
        }
        let chosen = identity
            .or_else(|| acc.identities.iter().find(|i| Some(&i.id) == acc.identity_id.as_ref()).cloned())
            .or_else(|| acc.identities.first().cloned())
            .ok_or_else(|| anyhow!("the account has no sending identity"))?;
        let sub = a.jmap.submit(&jid, &chosen.id, &chosen.email, &rcpts, sent.as_ref().map(|m| m.id.as_str()), drafts.as_ref().map(|m| m.id.as_str())).await;
        if let Err(err) = sub {
            let _ = a.jmap.email_set(Map::new(), vec![jid]).await;
            return Err(err);
        }
        if let Some(d) = o.draft {
            if let Some(r) = self.db.msg(d) {
                let _ = self.destroy(r.account, vec![r.jid]).await;
            }
        }
        if let Some(ans) = o.answering {
            let _ = self.set_keyword(vec![ans], "$answered", true).await;
        }
        a.kick.notify_one();
        Ok(())
    }

    /// Into Drafts, encrypted to our own key when we have one (the server doesn't encrypt what we upload).
    pub async fn save_draft(self: &Arc<Self>, o: Outgoing) -> Result<()> {
        let have_key = self.keys.read().unwrap().ours().is_some();
        let msg = self.build(&Outgoing { sign: false, ..o.clone() }, have_key)?;
        let a = self.acct_or_err(o.account)?;
        let drafts = self.db.mailbox_by_role(o.account, "drafts").ok_or_else(|| anyhow!("no drafts folder"))?;
        let blob = a.jmap.upload(msg, "message/rfc822").await?;
        a.jmap.import(&blob, &drafts.id, &["$draft", "$seen"]).await?;
        if let Some(d) = o.draft {
            if let Some(r) = self.db.msg(d) {
                let _ = self.destroy(r.account, vec![r.jid]).await;
            }
        }
        a.kick.notify_one();
        Ok(())
    }

    pub fn reply_from(&self, account: i64, to: &[Addr], cc: &[Addr]) -> Addr {
        match self.db.account(account) {
            Some(a) => senders::reply_from(&self.db, &a, to, cc),
            None => Addr::default(),
        }
    }

    pub fn our_addresses(&self, account: i64) -> Vec<Addr> {
        match self.db.account(account) {
            Some(a) => senders::addresses(&self.db, &a).into_iter().map(|e| senders::with_name(&a, &e)).collect(),
            None => vec![],
        }
    }

    pub fn is_ours(&self, account: i64, email: &str) -> bool {
        match self.db.account(account) {
            Some(a) => senders::known(&self.db, &a, email),
            None => false,
        }
    }

    // ---- shelf ----

    /// The shelf places: (id, label, url).
    pub fn shelves(&self) -> Vec<(String, String, String)> {
        let v: Vec<Value> = self.db.setting("shelves").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        v.iter()
            .filter_map(|o| Some((o.get("id")?.as_str()?.to_string(), o.get("label")?.as_str()?.to_string(), o.get("url")?.as_str()?.to_string())))
            .collect()
    }

    pub fn shelf(&self, id: &str) -> Result<crate::shelf::Shelf> {
        let (_, _, url) = self.shelves().into_iter().find(|s| s.0 == id).ok_or_else(|| anyhow!("no such shelf"))?;
        let key = secrets::get(&format!("shelf-{id}")).ok_or_else(|| anyhow!("the shelf key is not stored"))?;
        crate::shelf::Shelf::new(&url, &key)
    }

    /// Checks the key and remembers the place; returns its label.
    pub async fn add_shelf(&self, url: &str, key: &str) -> Result<String> {
        let s = crate::shelf::Shelf::new(url, key)?;
        let info = s.info().await?;
        let id = format!("{:x}", rand::random::<u64>());
        let label = format!("{} · {}/{}", info.name, info.user, info.folder);
        secrets::set(&format!("shelf-{id}"), key)?;
        let mut v: Vec<Value> = self.db.setting("shelves").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        v.push(serde_json::json!({ "id": id, "label": label, "url": url.trim(), "write": info.write }));
        self.db.set_setting("shelves", &serde_json::to_string(&v)?)?;
        Ok(label)
    }

    pub fn remove_shelf(&self, id: &str) -> Result<()> {
        secrets::remove(&format!("shelf-{id}"));
        let mut v: Vec<Value> = self.db.setting("shelves").and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default();
        v.retain(|o| o.get("id").and_then(|x| x.as_str()) != Some(id));
        self.db.set_setting("shelves", &serde_json::to_string(&v)?)?;
        Ok(())
    }
}

pub fn describe(e: &anyhow::Error) -> String {
    if pgp::needs_passphrase(e) {
        return "your key is locked: the passphrase is needed".into();
    }
    jmap::describe(e)
}
