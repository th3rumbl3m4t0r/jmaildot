//! Just enough CalDAV for one account: discovery, our calendars and the ones shared with
//! us, collection sync (RFC 6578), multiget, conditional PUT / DELETE, and sharing through
//! WebDAV ACL (RFC 3744), which is what Stalwart implements. Hrefs are kept as paths.

use anyhow::{anyhow, bail, Result};
use reqwest::Method;
use std::collections::HashMap;
use std::time::Duration;

pub const D: &str = "DAV:";
pub const C: &str = "urn:ietf:params:xml:ns:caldav";
pub const CS: &str = "http://calendarserver.org/ns/";
pub const A: &str = "http://apple.com/ns/ical/";
const XML: &str = "application/xml; charset=utf-8";

#[derive(Debug)]
pub struct DavError {
    pub code: u16,
    pub message: String,
}
impl std::fmt::Display for DavError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}
impl std::error::Error for DavError {}

/// The precondition failed: someone else changed the resource first.
#[derive(Debug)]
pub struct Conflict;
impl std::fmt::Display for Conflict {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "changed on the server")
    }
}
impl std::error::Error for Conflict {}

/// An owned XML element (namespace, local name, text, children).
#[derive(Clone, Debug, Default)]
pub struct El {
    pub ns: String,
    pub name: String,
    pub text: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<El>,
}

impl El {
    fn of(n: roxmltree::Node) -> El {
        El {
            ns: n.tag_name().namespace().unwrap_or("").to_string(),
            name: n.tag_name().name().to_string(),
            text: n.children().filter(|c| c.is_text()).map(|c| c.text().unwrap_or("")).collect::<String>(),
            attrs: n.attributes().map(|a| (a.name().to_string(), a.value().to_string())).collect(),
            kids: n.children().filter(|c| c.is_element()).map(El::of).collect(),
        }
    }
    pub fn all(&self, ns: &str, name: &str) -> Vec<&El> {
        self.kids.iter().filter(|k| k.ns == ns && k.name == name).collect()
    }
    pub fn first(&self, ns: &str, name: &str) -> Option<&El> {
        self.kids.iter().find(|k| k.ns == ns && k.name == name)
    }
    pub fn txt(&self) -> String {
        self.text.trim().to_string()
    }
    pub fn attr(&self, n: &str) -> Option<&str> {
        self.attrs.iter().find(|(k, _)| k == n).map(|(_, v)| v.as_str())
    }
}

/// One response of a multistatus: the resource, its own status (404 in a sync report =
/// gone) and its 200 props.
#[derive(Clone, Debug)]
pub struct Resp {
    pub href: String,
    pub status: Option<u16>,
    pub props: HashMap<(String, String), El>,
}

impl Resp {
    pub fn el(&self, ns: &str, name: &str) -> Option<&El> {
        self.props.get(&(ns.to_string(), name.to_string()))
    }
    pub fn text(&self, ns: &str, name: &str) -> Option<String> {
        self.el(ns, name).map(|e| e.txt()).filter(|s| !s.is_empty())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Cal {
    pub account: i64,
    pub href: String,
    pub name: String,
    pub color: Option<String>,
    pub ctag: Option<String>,
    pub sync_token: Option<String>,
    pub read_only: bool,
    /// The owner's user name for calendars shared with us; None for ours.
    pub owner: Option<String>,
    pub enabled: bool,
    pub ord: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Ace {
    pub user: String,
    pub write: bool,
}

pub struct SyncResult {
    pub changed: Vec<(String, Option<String>)>,
    pub gone: Vec<String>,
    pub token: Option<String>,
}

pub struct Fetched {
    pub href: String,
    pub etag: Option<String>,
    pub ics: String,
}

pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The server's hrefs may be absolute; we keep the path.
pub fn path_of(href: &str) -> String {
    if href.starts_with("http://") || href.starts_with("https://") {
        reqwest::Url::parse(href).map(|u| u.path().to_string()).unwrap_or_else(|_| href.to_string())
    } else {
        href.to_string()
    }
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The user name in a principal href: `/dav/pal/alice/` → `alice`.
pub fn user_of(href: &str) -> String {
    pct_decode(path_of(href).trim_end_matches('/').rsplit('/').next().unwrap_or(""))
}

/// A path segment for a user name: letters, digits and `@ . - _ ~` as they are.
pub fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"@.-_~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn code_of(status: &str) -> Option<u16> {
    let re = regex::Regex::new(r"HTTP/\d(?:\.\d)? (\d{3})").unwrap();
    re.captures(status).and_then(|c| c[1].parse().ok())
}

pub fn parse_multistatus(xml: &str) -> Result<(Vec<Resp>, El)> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| anyhow!("bad XML from the server: {e}"))?;
    let root = El::of(doc.root_element());
    let mut out = Vec::new();
    for r in root.all(D, "response") {
        let Some(href) = r.first(D, "href").map(|h| h.txt()) else { continue };
        let status = r.first(D, "status").and_then(|s| code_of(&s.txt()));
        let mut props = HashMap::new();
        for ps in r.all(D, "propstat") {
            let st = ps.first(D, "status").and_then(|s| code_of(&s.txt())).unwrap_or(200);
            if st != 200 {
                continue;
            }
            for p in ps.all(D, "prop") {
                for e in &p.kids {
                    props.insert((e.ns.clone(), e.name.clone()), e.clone());
                }
            }
        }
        out.push(Resp { href: path_of(&href), status, props });
    }
    Ok((out, root))
}

/// The user entries of an `acl` element: a grant of `write` (or `all`) means read & write.
pub fn parse_aces(acl: &El) -> Vec<Ace> {
    let mut out: Vec<Ace> = Vec::new();
    for ace in acl.all(D, "ace") {
        if ace.first(D, "deny").is_some() {
            continue;
        }
        let Some(href) = ace.first(D, "principal").and_then(|p| p.first(D, "href")).map(|h| h.txt()) else { continue };
        let privs: Vec<String> = ace.all(D, "grant").iter().flat_map(|g| g.all(D, "privilege")).flat_map(|p| p.kids.iter().map(|k| k.name.clone())).collect();
        let user = user_of(&href);
        if user.is_empty() {
            continue;
        }
        let write = privs.iter().any(|p| p == "write" || p == "all");
        match out.iter_mut().find(|a| a.user == user) {
            Some(a) => a.write |= write,
            None => out.push(Ace { user, write }),
        }
    }
    out
}

pub struct CalDav {
    pub base: String,
    user: String,
    pass: String,
    http: reqwest::Client,
}

impl CalDav {
    pub fn new(server: &str, user: &str, pass: &str) -> Result<CalDav> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("maildot/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(CalDav { base: crate::jmap::base_of(server)?, user: user.to_string(), pass: pass.to_string(), http })
    }

    fn url(&self, href: &str) -> String {
        if href.starts_with("http://") || href.starts_with("https://") {
            href.to_string()
        } else {
            let root = match reqwest::Url::parse(&self.base) {
                Ok(u) => format!("{}://{}{}", u.scheme(), u.host_str().unwrap_or(""), u.port().map(|p| format!(":{p}")).unwrap_or_default()),
                Err(_) => self.base.clone(),
            };
            format!("{root}{href}")
        }
    }

    /// One request, following redirects by hand (a PROPFIND must stay a PROPFIND).
    async fn call(&self, method: &str, href: &str, body: Option<String>, ctype: &str, headers: &[(&str, String)]) -> Result<(u16, HashMap<String, String>, String)> {
        let mut u = self.url(href);
        for _ in 0..5 {
            let m = Method::from_bytes(method.as_bytes())?;
            let mut rb = self.http.request(m, &u).basic_auth(&self.user, Some(&self.pass));
            for (k, v) in headers {
                rb = rb.header(*k, v);
            }
            if let Some(b) = &body {
                rb = rb.header("Content-Type", ctype).body(b.clone());
            }
            let r = rb.send().await.map_err(crate::jmap::describe_reqwest)?;
            let code = r.status().as_u16();
            let hs: HashMap<String, String> = r.headers().iter().map(|(k, v)| (k.as_str().to_lowercase(), v.to_str().unwrap_or("").to_string())).collect();
            if matches!(code, 301 | 302 | 307 | 308) {
                let loc = hs.get("location").cloned().ok_or_else(|| anyhow!("redirect without a location"))?;
                u = reqwest::Url::parse(&u)?.join(&loc)?.to_string();
                continue;
            }
            let text = r.text().await.unwrap_or_default();
            return Ok((code, hs, text));
        }
        bail!("too many redirects")
    }

    fn error(code: u16, body: &str) -> anyhow::Error {
        let detail = regex::Regex::new(r#""detail"\s*:\s*"([^"]*)""#).ok().and_then(|r| r.captures(body)).map(|c| c[1].to_string());
        let message = match code {
            401 => "wrong user name or password".to_string(),
            403 => format!("not allowed{}", detail.map(|d| format!(": {d}")).unwrap_or_default()),
            404 => "not found on the server".to_string(),
            _ => format!("server said {code}{}", detail.map(|d| format!(": {d}")).unwrap_or_default()),
        };
        anyhow::Error::new(DavError { code, message })
    }

    async fn multistatus(&self, method: &str, href: &str, depth: Option<&str>, body: &str) -> Result<(Vec<Resp>, El)> {
        let mut h = Vec::new();
        if let Some(d) = depth {
            h.push(("Depth", d.to_string()));
        }
        let (code, _, text) = self.call(method, href, Some(body.to_string()), XML, &h).await?;
        if code != 207 {
            return Err(Self::error(code, &text));
        }
        parse_multistatus(&text)
    }

    // ---- discovery ----

    /// The principal and the calendar home.
    pub async fn discover(&self) -> Result<(String, String)> {
        let pf = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:current-user-principal/></d:prop></d:propfind>"#;
        let mut principal = None;
        let mut last: Option<anyhow::Error> = None;
        for start in ["/.well-known/caldav", "/dav/cal", "/"] {
            match self.multistatus("PROPFIND", start, Some("0"), pf).await {
                Ok((rs, _)) => {
                    principal = rs.iter().find_map(|r| r.el(D, "current-user-principal").and_then(|e| e.first(D, "href")).map(|h| h.txt()));
                    if principal.is_some() {
                        break;
                    }
                }
                Err(e) => {
                    if e.downcast_ref::<DavError>().map(|d| d.code == 401).unwrap_or(false) {
                        return Err(e);
                    }
                    last = Some(e);
                }
            }
        }
        let p = principal.ok_or_else(|| last.unwrap_or_else(|| anyhow!("no CalDAV principal found")))?;
        let hf = r#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><c:calendar-home-set/></d:prop></d:propfind>"#;
        let (hs, _) = self.multistatus("PROPFIND", &p, Some("0"), hf).await?;
        let home = hs.iter().find_map(|r| r.el(C, "calendar-home-set").and_then(|e| e.first(D, "href")).map(|h| h.txt())).ok_or_else(|| anyhow!("no calendar home"))?;
        let mut home = path_of(&home);
        if !home.ends_with('/') {
            home.push('/');
        }
        Ok((path_of(&p), home))
    }

    /// Our own event calendars, then the ones other users shared with us (Stalwart lists their
    /// homes next to ours under the calendar root once they gave us access).
    pub async fn calendars(&self, account: i64, home: &str) -> Result<Vec<Cal>> {
        let mut out = self.list_calendars(account, home, None).await?;
        let root = format!("{}/", home.trim_end_matches('/').rsplit_once('/').map(|x| x.0).unwrap_or(""));
        if let Ok(homes) = self.homes(&root).await {
            for (h, owner) in homes {
                if h.trim_end_matches('/') == home.trim_end_matches('/') {
                    continue;
                }
                if let Ok(l) = self.list_calendars(account, &h, Some(owner)).await {
                    out.extend(l);
                }
            }
        }
        for (i, c) in out.iter_mut().enumerate() {
            c.ord = i as i64;
        }
        Ok(out)
    }

    async fn homes(&self, root: &str) -> Result<Vec<(String, String)>> {
        let body = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:owner/></d:prop></d:propfind>"#;
        let (rs, _) = self.multistatus("PROPFIND", root, Some("1"), body).await?;
        Ok(rs
            .into_iter()
            .filter(|r| r.href.trim_end_matches('/') != root.trim_end_matches('/') && r.el(D, "resourcetype").map(|t| t.first(D, "collection").is_some()).unwrap_or(false))
            .map(|r| {
                let owner = r.el(D, "owner").and_then(|o| o.first(D, "href")).map(|h| h.txt());
                let user = user_of(owner.as_deref().unwrap_or(&r.href));
                (r.href, user)
            })
            .collect())
    }

    async fn list_calendars(&self, account: i64, home: &str, owner: Option<String>) -> Result<Vec<Cal>> {
        let body = r#"<d:propfind xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav" xmlns:cs="http://calendarserver.org/ns/" xmlns:a="http://apple.com/ns/ical/"><d:prop><d:resourcetype/><d:displayname/><a:calendar-color/><c:supported-calendar-component-set/><cs:getctag/><d:sync-token/><d:current-user-privilege-set/></d:prop></d:propfind>"#;
        let (rs, _) = self.multistatus("PROPFIND", home, Some("1"), body).await?;
        Ok(rs
            .into_iter()
            .filter(|r| r.el(D, "resourcetype").map(|t| t.first(C, "calendar").is_some()).unwrap_or(false))
            .filter(|r| match r.el(C, "supported-calendar-component-set") {
                None => true,
                Some(s) => {
                    let comps = s.all(C, "comp");
                    comps.is_empty() || comps.iter().any(|c| c.attr("name").map(|n| n.eq_ignore_ascii_case("VEVENT")).unwrap_or(false))
                }
            })
            .map(|r| {
                let href = if r.href.ends_with('/') { r.href.clone() } else { format!("{}/", r.href) };
                let writable = match r.el(D, "current-user-privilege-set") {
                    None => true,
                    Some(p) => p.all(D, "privilege").iter().any(|x| x.kids.iter().any(|k| k.name == "write" || k.name == "write-content" || k.name == "all")),
                };
                let color = r.text(A, "calendar-color").filter(|c| c.starts_with('#') && c.len() >= 7).map(|c| c[..7].to_string());
                Cal {
                    account,
                    name: r.text(D, "displayname").unwrap_or_else(|| href.trim_end_matches('/').rsplit('/').next().unwrap_or("").to_string()),
                    color,
                    ctag: r.text(CS, "getctag"),
                    sync_token: r.text(D, "sync-token"),
                    read_only: !writable,
                    owner: owner.clone(),
                    enabled: true,
                    ord: 0,
                    href,
                }
            })
            .collect())
    }

    // ---- sharing ----

    pub async fn acl(&self, cal: &str) -> Result<Vec<Ace>> {
        let body = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:acl/></d:prop></d:propfind>"#;
        let (rs, _) = self.multistatus("PROPFIND", cal, Some("0"), body).await?;
        let el = rs.first().and_then(|r| r.el(D, "acl")).ok_or_else(|| anyhow::Error::new(DavError { code: 403, message: "the server doesn't show the access list".into() }))?;
        Ok(parse_aces(el))
    }

    /// Replaces the calendar's access list; [principal_root] is where users live (`/dav/pal/`).
    pub async fn set_acl(&self, cal: &str, aces: &[Ace], principal_root: &str) -> Result<()> {
        let mut b = String::from(r#"<d:acl xmlns:d="DAV:">"#);
        for a in aces {
            b.push_str(&format!("<d:ace><d:principal><d:href>{}</d:href></d:principal><d:grant><d:privilege><d:read/></d:privilege>", esc(&format!("{principal_root}{}/", encode_segment(&a.user)))));
            if a.write {
                b.push_str("<d:privilege><d:write/></d:privilege>");
            }
            b.push_str("</d:grant></d:ace>");
        }
        b.push_str("</d:acl>");
        let (code, _, text) = self.call("ACL", cal, Some(b), XML, &[]).await?;
        if (200..300).contains(&code) {
            return Ok(());
        }
        if text.contains("allowed-principal") {
            bail!("no such user on this server");
        }
        if code == 403 {
            bail!("not allowed: only the owner can share a calendar");
        }
        Err(Self::error(code, &text))
    }

    // ---- contents ----

    /// What changed since [token]; None when the token is stale (list everything instead).
    pub async fn sync(&self, cal: &str, token: Option<&str>) -> Result<Option<SyncResult>> {
        let body = format!(
            r#"<d:sync-collection xmlns:d="DAV:"><d:sync-token>{}</d:sync-token><d:sync-level>1</d:sync-level><d:prop><d:getetag/></d:prop></d:sync-collection>"#,
            esc(token.unwrap_or(""))
        );
        let (code, _, text) = self.call("REPORT", cal, Some(body), XML, &[("Depth", "0".into())]).await?;
        if token.is_some() && matches!(code, 400 | 403 | 409) {
            return Ok(None);
        }
        if code != 207 {
            return Err(Self::error(code, &text));
        }
        let (rs, root) = parse_multistatus(&text)?;
        let new_token = root.first(D, "sync-token").map(|t| t.txt());
        let mut changed = Vec::new();
        let mut gone = Vec::new();
        for r in rs {
            if r.href.trim_end_matches('/') == cal.trim_end_matches('/') {
                continue;
            }
            match r.status {
                Some(404) => gone.push(r.href),
                Some(c) if !(200..300).contains(&c) => {}
                _ => {
                    let e = r.text(D, "getetag");
                    changed.push((r.href, e));
                }
            }
        }
        Ok(Some(SyncResult { changed, gone, token: new_token }))
    }

    /// href → etag of every resource in the calendar.
    pub async fn list(&self, cal: &str) -> Result<HashMap<String, Option<String>>> {
        let body = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/><d:resourcetype/></d:prop></d:propfind>"#;
        let (rs, _) = self.multistatus("PROPFIND", cal, Some("1"), body).await?;
        Ok(rs
            .into_iter()
            .filter(|r| r.href.trim_end_matches('/') != cal.trim_end_matches('/') && r.el(D, "resourcetype").map(|t| t.first(D, "collection").is_none()).unwrap_or(true))
            .map(|r| {
                let e = r.text(D, "getetag");
                (r.href, e)
            })
            .collect())
    }

    pub async fn multiget(&self, cal: &str, hrefs: &[String]) -> Result<Vec<Fetched>> {
        if hrefs.is_empty() {
            return Ok(vec![]);
        }
        let mut b = String::from(r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop>"#);
        for h in hrefs {
            b.push_str(&format!("<d:href>{}</d:href>", esc(h)));
        }
        b.push_str("</c:calendar-multiget>");
        let (rs, _) = self.multistatus("REPORT", cal, Some("1"), &b).await?;
        Ok(rs
            .into_iter()
            .filter_map(|r| {
                let ics = r.el(C, "calendar-data")?.text.clone();
                let etag = r.text(D, "getetag");
                Some(Fetched { href: r.href, etag, ics })
            })
            .collect())
    }

    /// The current etag of one resource, None when it is gone.
    pub async fn etag(&self, href: &str) -> Result<Option<String>> {
        let body = r#"<d:propfind xmlns:d="DAV:"><d:prop><d:getetag/></d:prop></d:propfind>"#;
        let (code, _, text) = self.call("PROPFIND", href, Some(body.into()), XML, &[("Depth", "0".into())]).await?;
        if code == 404 {
            return Ok(None);
        }
        if code != 207 {
            return Err(Self::error(code, &text));
        }
        Ok(parse_multistatus(&text)?.0.first().and_then(|r| r.text(D, "getetag")))
    }

    /// Creates (no etag) or replaces; the new etag when the server sent one.
    pub async fn put(&self, href: &str, ics: &str, etag: Option<&str>) -> Result<Option<String>> {
        let h = match etag {
            None => vec![("If-None-Match", "*".to_string())],
            Some(e) => vec![("If-Match", e.to_string())],
        };
        let (code, hs, text) = self.call("PUT", href, Some(ics.to_string()), "text/calendar; charset=utf-8", &h).await?;
        if code == 412 {
            return Err(anyhow::Error::new(Conflict));
        }
        if !(200..300).contains(&code) {
            return Err(Self::error(code, &text));
        }
        Ok(hs.get("etag").cloned())
    }

    pub async fn delete(&self, href: &str, etag: Option<&str>) -> Result<()> {
        let h: Vec<(&str, String)> = etag.map(|e| vec![("If-Match", e.to_string())]).unwrap_or_default();
        let (code, _, text) = self.call("DELETE", href, None, XML, &h).await?;
        if code == 404 {
            return Ok(());
        }
        if code == 412 {
            return Err(anyhow::Error::new(Conflict));
        }
        if !(200..300).contains(&code) {
            return Err(Self::error(code, &text));
        }
        Ok(())
    }
}

pub fn is_conflict(e: &anyhow::Error) -> bool {
    e.downcast_ref::<Conflict>().is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multistatus_and_acl() {
        let xml = r#"<?xml version="1.0"?><d:multistatus xmlns:d="DAV:"><d:response><d:href>/dav/cal/alice/default/</d:href><d:propstat><d:prop><d:acl><d:ace><d:principal><d:href>/dav/pal/bob/</d:href></d:principal><d:grant><d:privilege><d:read/></d:privilege><d:privilege><d:write/></d:privilege></d:grant></d:ace><d:ace><d:principal><d:href>/dav/pal/carol%40x.cz/</d:href></d:principal><d:grant><d:privilege><d:read/></d:privilege></d:grant></d:ace></d:acl></d:prop><d:status>HTTP/1.1 200 OK</d:status></d:propstat></d:response><d:response><d:href>/x.ics</d:href><d:status>HTTP/1.1 404 Not Found</d:status></d:response><d:sync-token>t2</d:sync-token></d:multistatus>"#;
        let (rs, root) = parse_multistatus(xml).unwrap();
        assert_eq!(rs.len(), 2);
        assert_eq!(rs[1].status, Some(404));
        assert_eq!(root.first(D, "sync-token").unwrap().txt(), "t2");
        let aces = parse_aces(rs[0].el(D, "acl").unwrap());
        assert_eq!(aces, vec![Ace { user: "bob".into(), write: true }, Ace { user: "carol@x.cz".into(), write: false }]);
    }
}
