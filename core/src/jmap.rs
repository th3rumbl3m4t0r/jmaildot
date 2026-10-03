//! JMAP (RFC 8620 / 8621, submission, Sieve RFC 9661, Blob RFC 9404) for one account over
//! Basic auth. Requests are capped at the server's maxConcurrentRequests minus one (the
//! event source is the one left over), so a big download never trips the server's limit.

use crate::db::Meta;
use crate::model::{Addr, Identity, Mailbox};
use anyhow::{anyhow, bail, Result};
use base64::Engine as _;
use serde_json::{json, Map, Value};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, Semaphore};

pub const CORE: &str = "urn:ietf:params:jmap:core";
pub const MAIL: &str = "urn:ietf:params:jmap:mail";
pub const SUBMISSION: &str = "urn:ietf:params:jmap:submission";
pub const SIEVE: &str = "urn:ietf:params:jmap:sieve";
pub const BLOB: &str = "urn:ietf:params:jmap:blob";

/// A method-level error (`["error", {type, description}]`) or a SetError.
#[derive(Debug)]
pub struct MethodError {
    pub kind: String,
    pub description: Option<String>,
}

impl std::fmt::Display for MethodError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.description {
            Some(d) => write!(f, "{}: {}", self.kind, d),
            None => write!(f, "{}", self.kind),
        }
    }
}
impl std::error::Error for MethodError {}

#[derive(Debug)]
pub struct HttpError {
    pub code: u16,
    pub message: String,
}
impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for HttpError {}

pub fn method_error(e: &anyhow::Error) -> Option<&MethodError> {
    e.downcast_ref::<MethodError>()
}

pub struct Session {
    pub account_id: String,
    pub name: String,
    pub username: String,
    pub api: String,
    pub download: String,
    pub upload: String,
    pub event_source: Option<String>,
    pub caps: Vec<String>,
    pub max_get: usize,
    pub max_calls: usize,
    pub max_concurrent: usize,
    pub sem: Arc<Semaphore>,
}

impl Session {
    pub fn has(&self, cap: &str) -> bool {
        self.caps.iter().any(|c| c == cap)
    }
}

pub struct Changes {
    pub created: Vec<String>,
    pub updated: Vec<String>,
    pub destroyed: Vec<String>,
    pub new_state: String,
    pub has_more: bool,
}

#[derive(Clone, Debug)]
pub struct SieveScript {
    pub id: String,
    pub name: String,
    pub blob: String,
    pub active: bool,
}

pub struct Jmap {
    pub base: String,
    user: String,
    pass: String,
    http: reqwest::Client,
    session: RwLock<Option<Arc<Session>>>,
}

/// `mail.example.com` → `https://mail.example.com`; a trailing slash goes.
pub fn base_of(server: &str) -> Result<String> {
    let s = server.trim().trim_end_matches('/');
    if s.is_empty() {
        bail!("no server address");
    }
    let s = if s.contains("://") { s.to_string() } else { format!("https://{s}") };
    let u = reqwest::Url::parse(&s).map_err(|_| anyhow!("bad server address"))?;
    Ok(format!("{}://{}", u.scheme(), u.host_str().unwrap_or("")) + &u.port().map(|p| format!(":{p}")).unwrap_or_default() + u.path().trim_end_matches('/'))
}

/// `https://whatever/jmap/x` → `/jmap/x` (the server may name itself differently from how we reach it).
pub fn path_of(url: &str) -> String {
    if let Some(i) = url.find("://") {
        let rest = &url[i + 3..];
        return match rest.find('/') {
            Some(j) => rest[j..].to_string(),
            None => "/".to_string(),
        };
    }
    url.to_string()
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .user_agent(concat!("maildot/", env!("CARGO_PKG_VERSION")))
        .gzip(true)
        .build()
        .expect("http client")
}

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(|x| x.as_str()).map(String::from)
}

fn ids_of(v: &Value, k: &str) -> Vec<String> {
    v.get(k).and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|i| i.as_str().map(String::from)).collect()).unwrap_or_default()
}

fn true_keys(v: &Value, k: &str) -> Vec<String> {
    v.get(k)
        .and_then(|x| x.as_object())
        .map(|o| o.iter().filter(|(_, b)| b.as_bool() == Some(true)).map(|(k, _)| k.clone()).collect())
        .unwrap_or_default()
}

pub fn addrs(v: Option<&Value>) -> Vec<Addr> {
    v.and_then(|x| x.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|o| {
                    let email = o.get("email")?.as_str()?;
                    Some(Addr::new(o.get("name").and_then(|n| n.as_str()), email))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn time_of(v: Option<&Value>) -> Option<i64> {
    let t = v?.as_str()?;
    chrono::DateTime::parse_from_rfc3339(t).ok().map(|d| d.timestamp())
}

pub const META_PROPS: &[&str] = &[
    "id", "blobId", "threadId", "mailboxIds", "keywords", "hasAttachment", "from", "to", "cc", "subject", "receivedAt", "sentAt", "size",
    "preview", "messageId", "bodyStructure",
];

pub fn meta_of(o: &Value) -> Option<Meta> {
    Some(Meta {
        jid: s(o, "id")?,
        blob: s(o, "blobId").unwrap_or_default(),
        thread: s(o, "threadId").unwrap_or_default(),
        mailboxes: true_keys(o, "mailboxIds"),
        keywords: true_keys(o, "keywords"),
        has_att: o.get("hasAttachment").and_then(|b| b.as_bool()).unwrap_or(false),
        from: addrs(o.get("from")),
        to: addrs(o.get("to")),
        cc: addrs(o.get("cc")),
        subject: s(o, "subject").unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" "),
        received: time_of(o.get("receivedAt")).unwrap_or(0),
        sent: time_of(o.get("sentAt")),
        size: o.get("size").and_then(|n| n.as_i64()).unwrap_or(0),
        preview: s(o, "preview").unwrap_or_default(),
        kind: o.get("bodyStructure").and_then(|b| b.get("type")).and_then(|t| t.as_str()).unwrap_or("").to_lowercase(),
        msgid: o.get("messageId").and_then(|m| m.as_array()).and_then(|a| a.first()).and_then(|x| x.as_str()).map(String::from),
    })
}

fn set_error(v: &Value) -> anyhow::Error {
    anyhow::Error::new(MethodError { kind: s(v, "type").unwrap_or_else(|| "setError".into()), description: s(v, "description") })
}

impl Jmap {
    pub fn new(server: &str, user: &str, pass: &str) -> Result<Jmap> {
        Ok(Jmap { base: base_of(server)?, user: user.to_string(), pass: pass.to_string(), http: http_client(), session: RwLock::new(None) })
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with('/') {
            format!("{}{}", self.base, path)
        } else {
            format!("{}/{}", self.base, path)
        }
    }

    fn http_error(code: u16, body: &str) -> anyhow::Error {
        let detail = regex::Regex::new(r#""detail"\s*:\s*"([^"]*)""#).ok().and_then(|r| r.captures(body)).map(|c| c[1].to_string());
        let message = match code {
            401 => "wrong user name or password".to_string(),
            403 => format!("not allowed{}", detail.map(|d| format!(": {d}")).unwrap_or_default()),
            404 => "no JMAP at this address".to_string(),
            429 => "the server says: too many requests".to_string(),
            _ => format!("server said {code}{}", detail.map(|d| format!(": {d}")).unwrap_or_default()),
        };
        anyhow::Error::new(HttpError { code, message })
    }

    async fn send(&self, rb: reqwest::RequestBuilder) -> Result<reqwest::Response> {
        let mut tries = 0;
        loop {
            let r = rb.try_clone().ok_or_else(|| anyhow!("request cannot be repeated"))?.basic_auth(&self.user, Some(&self.pass)).send().await;
            match r {
                Ok(r) if (r.status().as_u16() == 429 || r.status().as_u16() == 503) && tries < 5 => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(500 * tries)).await;
                }
                Ok(r) => return Ok(r),
                Err(e) if tries < 2 && (e.is_connect() || e.is_timeout()) => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
                Err(e) => return Err(describe_reqwest(e)),
            }
        }
    }

    // ---- session ----

    /// GET /.well-known/jmap (following its redirect); the mail account is the primary one.
    pub async fn connect(&self) -> Result<Arc<Session>> {
        let r = self.send(self.http.get(self.url("/.well-known/jmap")).timeout(Duration::from_secs(30))).await?;
        let code = r.status().as_u16();
        let body = r.text().await?;
        if code != 200 {
            return Err(Self::http_error(code, &body));
        }
        let o: Value = serde_json::from_str(&body).map_err(|_| anyhow!("no JMAP at this address"))?;
        let caps: Vec<String> = o.get("capabilities").and_then(|c| c.as_object()).map(|m| m.keys().cloned().collect()).unwrap_or_default();
        let core = o.get("capabilities").and_then(|c| c.get(CORE));
        let num = |k: &str, d: usize| core.and_then(|c| c.get(k)).and_then(|v| v.as_u64()).map(|v| v as usize).unwrap_or(d);
        let account_id = o.get("primaryAccounts").and_then(|p| s(p, MAIL)).ok_or_else(|| anyhow!("this account has no mail"))?;
        let name = o.get("accounts").and_then(|a| a.get(&account_id)).and_then(|a| s(a, "name")).unwrap_or_default();
        let max_concurrent = num("maxConcurrentRequests", 4).max(1);
        let sess = Session {
            name,
            username: s(&o, "username").unwrap_or_default(),
            api: path_of(&s(&o, "apiUrl").ok_or_else(|| anyhow!("no apiUrl"))?),
            download: path_of(&s(&o, "downloadUrl").ok_or_else(|| anyhow!("no downloadUrl"))?),
            upload: path_of(&s(&o, "uploadUrl").ok_or_else(|| anyhow!("no uploadUrl"))?),
            event_source: s(&o, "eventSourceUrl").map(|u| path_of(&u)),
            max_get: num("maxObjectsInGet", 500).clamp(1, 500),
            max_calls: num("maxCallsInRequest", 16).max(1),
            sem: Arc::new(Semaphore::new(if max_concurrent > 1 { max_concurrent - 1 } else { 1 })),
            max_concurrent,
            caps,
            account_id,
        };
        let a = Arc::new(sess);
        *self.session.write().await = Some(a.clone());
        Ok(a)
    }

    pub async fn session(&self) -> Result<Arc<Session>> {
        if let Some(s) = self.session.read().await.clone() {
            return Ok(s);
        }
        self.connect().await
    }

    // ---- method calls ----

    /// Runs the calls in one request; the arguments of each call's response, in order.
    pub async fn invoke(&self, calls: Vec<(&str, Value)>) -> Result<Vec<Value>> {
        let sess = self.session().await?;
        let mut using = vec![CORE, MAIL, SUBMISSION];
        if sess.has(SIEVE) {
            using.push(SIEVE);
        }
        if sess.has(BLOB) {
            using.push(BLOB);
        }
        let n = calls.len();
        let mc: Vec<Value> = calls
            .into_iter()
            .enumerate()
            .map(|(i, (m, mut a))| {
                if let Some(o) = a.as_object_mut() {
                    o.insert("accountId".into(), Value::String(sess.account_id.clone()));
                }
                json!([m, a, i.to_string()])
            })
            .collect();
        let body = json!({ "using": using, "methodCalls": mc });
        let _permit = sess.sem.acquire().await?;
        let r = self.send(self.http.post(self.url(&sess.api)).json(&body).timeout(Duration::from_secs(300))).await?;
        let code = r.status().as_u16();
        let text = r.text().await.map_err(describe_reqwest)?;
        if code != 200 {
            return Err(Self::http_error(code, &text));
        }
        let resp: Value = serde_json::from_str(&text)?;
        let mut out: Vec<Option<Value>> = vec![None; n];
        for m in resp.get("methodResponses").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let name = m.get(0).and_then(|x| x.as_str()).unwrap_or("");
            let args = m.get(1).cloned().unwrap_or(Value::Null);
            let id: usize = m.get(2).and_then(|x| x.as_str()).and_then(|x| x.parse().ok()).unwrap_or(usize::MAX);
            if name == "error" {
                return Err(anyhow::Error::new(MethodError { kind: s_(&args, "type"), description: s(&args, "description") }));
            }
            if id < n && out[id].is_none() {
                out[id] = Some(args);
            }
        }
        out.into_iter().map(|o| o.ok_or_else(|| anyhow!("the server left a call unanswered"))).collect()
    }

    async fn one(&self, method: &str, args: Value) -> Result<Value> {
        Ok(self.invoke(vec![(method, args)]).await?.remove(0))
    }

    // ---- mail ----

    pub async fn mailboxes(&self, account: i64) -> Result<Vec<Mailbox>> {
        let r = self
            .one("Mailbox/get", json!({ "ids": null, "properties": ["id", "name", "role", "parentId", "sortOrder", "totalEmails", "unreadEmails"] }))
            .await?;
        Ok(r.get("list")
            .and_then(|l| l.as_array())
            .map(|l| {
                l.iter()
                    .filter_map(|o| {
                        Some(Mailbox {
                            account,
                            id: s(o, "id")?,
                            name: s(o, "name").unwrap_or_default(),
                            role: s(o, "role").map(|r| r.to_lowercase()),
                            parent: s(o, "parentId"),
                            sort: o.get("sortOrder").and_then(|x| x.as_i64()).unwrap_or(0),
                            total: o.get("totalEmails").and_then(|x| x.as_i64()).unwrap_or(0),
                            unread: o.get("unreadEmails").and_then(|x| x.as_i64()).unwrap_or(0),
                            notify: None,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn email_state(&self) -> Result<String> {
        let r = self.one("Email/get", json!({ "ids": [] })).await?;
        Ok(s(&r, "state").unwrap_or_default())
    }

    /// The Email and Mailbox states in one small request (what the light poll compares).
    pub async fn states(&self) -> Result<(String, String)> {
        let r = self.invoke(vec![("Email/get", json!({ "ids": [] })), ("Mailbox/get", json!({ "ids": [] }))]).await?;
        Ok((s(&r[0], "state").unwrap_or_default(), s(&r[1], "state").unwrap_or_default()))
    }

    /// Every message id of the account, newest first.
    pub async fn all_ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        loop {
            let r = self
                .one(
                    "Email/query",
                    json!({ "sort": [{ "property": "receivedAt", "isAscending": false }], "position": ids.len(), "limit": 1000, "calculateTotal": true }),
                )
                .await?;
            let got = ids_of(&r, "ids");
            let total = r.get("total").and_then(|t| t.as_u64()).map(|t| t as usize);
            if got.is_empty() {
                break;
            }
            ids.extend(got);
            if total.map(|t| ids.len() >= t).unwrap_or(false) {
                break;
            }
        }
        Ok(ids)
    }

    pub async fn metas(&self, ids: &[String]) -> Result<Vec<Meta>> {
        let s = self.session().await?;
        let mut out = Vec::new();
        for chunk in ids.chunks(s.max_get.min(250)) {
            let r = self.one("Email/get", json!({ "ids": chunk, "properties": META_PROPS, "bodyProperties": ["type"] })).await?;
            if let Some(l) = r.get("list").and_then(|l| l.as_array()) {
                out.extend(l.iter().filter_map(meta_of));
            }
        }
        Ok(out)
    }

    pub async fn changes(&self, since: &str) -> Result<Changes> {
        let r = self.one("Email/changes", json!({ "sinceState": since, "maxChanges": 1000 })).await?;
        Ok(Changes {
            created: ids_of(&r, "created"),
            updated: ids_of(&r, "updated"),
            destroyed: ids_of(&r, "destroyed"),
            new_state: s(&r, "newState").unwrap_or_else(|| since.to_string()),
            has_more: r.get("hasMoreChanges").and_then(|b| b.as_bool()).unwrap_or(false),
        })
    }

    /// Email/set: [update] is id → patch, [destroy] deletes for good.
    pub async fn email_set(&self, update: Map<String, Value>, destroy: Vec<String>) -> Result<()> {
        if update.is_empty() && destroy.is_empty() {
            return Ok(());
        }
        let mut a = Map::new();
        if !update.is_empty() {
            a.insert("update".into(), Value::Object(update));
        }
        if !destroy.is_empty() {
            a.insert("destroy".into(), json!(destroy));
        }
        let r = self.one("Email/set", Value::Object(a)).await?;
        for k in ["notUpdated", "notDestroyed"] {
            if let Some(o) = r.get(k).and_then(|x| x.as_object()) {
                if let Some(e) = o.values().next() {
                    return Err(set_error(e));
                }
            }
        }
        Ok(())
    }

    /// Several raw messages in one request (Blob/get, base64), or one by one over the
    /// download URL when the server has no Blob capability.
    pub async fn blobs(&self, ids: &[String]) -> Result<Vec<(String, Vec<u8>)>> {
        let s = self.session().await?;
        if !s.has(BLOB) {
            let mut out = Vec::new();
            for id in ids {
                out.push((id.clone(), self.download(id, "message.eml", "message/rfc822").await?));
            }
            return Ok(out);
        }
        let r = self.one("Blob/get", json!({ "ids": ids, "properties": ["data:asBase64"] })).await?;
        let mut out = Vec::new();
        for o in r.get("list").and_then(|l| l.as_array()).cloned().unwrap_or_default() {
            let (Some(id), Some(b64)) = (s_opt(&o, "id"), s_opt(&o, "data:asBase64")) else { continue };
            let data = base64::engine::general_purpose::STANDARD.decode(b64.as_bytes()).map_err(|e| anyhow!("bad base64 from the server: {e}"))?;
            out.push((id, data));
        }
        Ok(out)
    }

    pub async fn download(&self, blob: &str, name: &str, typ: &str) -> Result<Vec<u8>> {
        let s = self.session().await?;
        let enc = |x: &str| -> String {
            x.bytes()
                .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
                .collect()
        };
        let path = s
            .download
            .replace("{accountId}", &s.account_id)
            .replace("{blobId}", blob)
            .replace("{name}", &enc(if name.is_empty() { "blob" } else { name }))
            .replace("{type}", &enc(typ));
        let _permit = s.sem.acquire().await?;
        let r = self.send(self.http.get(self.url(&path)).timeout(Duration::from_secs(600))).await?;
        let code = r.status().as_u16();
        if code != 200 {
            let t = r.text().await.unwrap_or_default();
            return Err(Self::http_error(code, &t));
        }
        Ok(r.bytes().await.map_err(describe_reqwest)?.to_vec())
    }

    pub async fn upload(&self, bytes: Vec<u8>, typ: &str) -> Result<String> {
        let s = self.session().await?;
        let path = s.upload.replace("{accountId}", &s.account_id);
        let _permit = s.sem.acquire().await?;
        let r = self
            .send(self.http.post(self.url(&path)).header("Content-Type", typ).body(bytes).timeout(Duration::from_secs(600)))
            .await?;
        let code = r.status().as_u16();
        let t = r.text().await.map_err(describe_reqwest)?;
        if !(200..300).contains(&code) {
            return Err(Self::http_error(code, &t));
        }
        let v: Value = serde_json::from_str(&t)?;
        s_opt(&v, "blobId").ok_or_else(|| anyhow!("upload gave no blobId"))
    }

    /// A finished RFC 5322 message (uploaded) into a folder; returns the new email id.
    pub async fn import(&self, blob: &str, mailbox: &str, keywords: &[&str]) -> Result<String> {
        let kw: Map<String, Value> = keywords.iter().map(|k| (k.to_string(), Value::Bool(true))).collect();
        let r = self
            .one("Email/import", json!({ "emails": { "m": { "blobId": blob, "mailboxIds": { mailbox: true }, "keywords": kw } } }))
            .await?;
        if let Some(e) = r.get("notCreated").and_then(|n| n.get("m")) {
            return Err(set_error(e));
        }
        r.get("created").and_then(|c| c.get("m")).and_then(|m| s_opt(m, "id")).ok_or_else(|| anyhow!("import gave no id"))
    }

    pub async fn identities(&self) -> Result<Vec<Identity>> {
        let r = self.one("Identity/get", json!({ "ids": null })).await?;
        Ok(r.get("list")
            .and_then(|l| l.as_array())
            .map(|l| {
                l.iter()
                    .filter_map(|o| Some(Identity { id: s(o, "id")?, name: s(o, "name").filter(|n| !n.trim().is_empty()), email: s(o, "email")? }))
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Tries to add a sending identity for [email]; None when the server won't have it.
    pub async fn create_identity(&self, name: Option<&str>, email: &str) -> Result<Option<Identity>> {
        let r = self.one("Identity/set", json!({ "create": { "n": { "name": name.unwrap_or(""), "email": email } } })).await?;
        Ok(r.get("created").and_then(|c| c.get("n")).and_then(|n| s_opt(n, "id")).map(|id| Identity {
            id,
            name: name.map(String::from),
            email: email.to_string(),
        }))
    }

    /// Sends a stored message; on success the server moves it from Drafts to Sent.
    pub async fn submit(&self, email_id: &str, identity: &str, from: &str, rcpts: &[String], sent: Option<&str>, drafts: Option<&str>) -> Result<()> {
        let mut upd = Map::new();
        upd.insert("keywords/$draft".into(), Value::Null);
        upd.insert("keywords/$seen".into(), Value::Bool(true));
        if let Some(d) = drafts {
            upd.insert(format!("mailboxIds/{d}"), Value::Null);
        }
        if let Some(s) = sent {
            upd.insert(format!("mailboxIds/{s}"), Value::Bool(true));
        }
        let rc: Vec<Value> = rcpts.iter().map(|r| json!({ "email": r })).collect();
        let r = self
            .one(
                "EmailSubmission/set",
                json!({
                    "create": { "s": { "emailId": email_id, "identityId": identity, "envelope": { "mailFrom": { "email": from }, "rcptTo": rc } } },
                    "onSuccessUpdateEmail": { "#s": upd }
                }),
            )
            .await?;
        if let Some(e) = r.get("notCreated").and_then(|n| n.get("s")) {
            return Err(set_error(e));
        }
        Ok(())
    }

    // ---- sieve ----

    pub async fn has_sieve(&self) -> bool {
        self.session().await.map(|s| s.has(SIEVE)).unwrap_or(false)
    }

    pub async fn sieve_scripts(&self) -> Result<Vec<SieveScript>> {
        let r = self.one("SieveScript/get", json!({ "ids": null })).await?;
        Ok(r.get("list")
            .and_then(|l| l.as_array())
            .map(|l| {
                l.iter()
                    .filter_map(|o| {
                        Some(SieveScript {
                            id: s(o, "id")?,
                            name: s(o, "name").unwrap_or_default(),
                            blob: s(o, "blobId").unwrap_or_default(),
                            active: o.get("isActive").and_then(|b| b.as_bool()).unwrap_or(false),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn sieve_text(&self, blob: &str) -> Result<String> {
        Ok(String::from_utf8_lossy(&self.download(blob, "script.sieve", "application/sieve").await?).into_owned())
    }

    /// None when the script is fine, else the server's complaint.
    pub async fn sieve_validate(&self, text: &str) -> Result<Option<String>> {
        let blob = self.upload(text.as_bytes().to_vec(), "application/sieve").await?;
        let r = self.one("SieveScript/validate", json!({ "blobId": blob })).await?;
        Ok(r.get("error").filter(|e| !e.is_null()).map(|e| s(e, "description").or_else(|| s(e, "type")).unwrap_or_else(|| "invalid".into())))
    }

    /// Creates (id None) or replaces a script; [activate] makes it the one that runs. Returns the id.
    pub async fn sieve_save(&self, id: Option<&str>, name: &str, text: &str, activate: bool) -> Result<String> {
        let blob = self.upload(text.as_bytes().to_vec(), "application/sieve").await?;
        let mut a = Map::new();
        match id {
            None => {
                a.insert("create".into(), json!({ "n": { "name": name, "blobId": blob } }));
            }
            Some(i) => {
                a.insert("update".into(), json!({ i: { "name": name, "blobId": blob } }));
            }
        }
        if activate {
            a.insert("onSuccessActivateScript".into(), Value::String(id.map(String::from).unwrap_or_else(|| "#n".into())));
        }
        let r = self.one("SieveScript/set", Value::Object(a)).await?;
        if let Some(e) = r.get("notCreated").and_then(|n| n.get("n")) {
            return Err(set_error(e));
        }
        if let Some(i) = id {
            if let Some(e) = r.get("notUpdated").and_then(|n| n.get(i)) {
                return Err(set_error(e));
            }
            return Ok(i.to_string());
        }
        r.get("created").and_then(|c| c.get("n")).and_then(|n| s_opt(n, "id")).ok_or_else(|| anyhow!("the server gave the script no id"))
    }

    /// Makes [id] the active script, or none.
    pub async fn sieve_activate(&self, id: Option<&str>) -> Result<()> {
        if id.is_some() {
            self.one("SieveScript/set", json!({ "onSuccessActivateScript": id })).await?;
        } else {
            self.one("SieveScript/set", json!({ "onSuccessDeactivateScript": true })).await?;
        }
        Ok(())
    }

    pub async fn sieve_delete(&self, id: &str) -> Result<()> {
        let r = self.one("SieveScript/set", json!({ "destroy": [id] })).await?;
        if let Some(e) = r.get("notDestroyed").and_then(|n| n.get(id)) {
            return Err(set_error(e));
        }
        Ok(())
    }

    // ---- push ----

    /// The event source stream (RFC 8620 7.3); None when the server has none.
    pub async fn event_source(&self) -> Result<Option<reqwest::Response>> {
        let s = self.session().await?;
        let Some(es) = s.event_source.clone() else { return Ok(None) };
        let path = es.replace("{types}", "*").replace("{closeafter}", "no").replace("{ping}", "60");
        let r = self.send(self.http.get(self.url(&path)).header("Accept", "text/event-stream")).await?;
        let code = r.status().as_u16();
        if code != 200 {
            let t = r.text().await.unwrap_or_default();
            return Err(Self::http_error(code, &t));
        }
        Ok(Some(r))
    }
}

fn s_opt(v: &Value, k: &str) -> Option<String> {
    s(v, k)
}

fn s_(v: &Value, k: &str) -> String {
    s(v, k).unwrap_or_else(|| "error".into())
}

pub fn describe_reqwest(e: reqwest::Error) -> anyhow::Error {
    if e.is_connect() {
        anyhow!("server not reachable")
    } else if e.is_timeout() {
        anyhow!("the server took too long")
    } else {
        anyhow!("network: {e}")
    }
}

/// A short message for the status bar.
pub fn describe(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    s.lines().next().unwrap_or("").chars().take(200).collect()
}
