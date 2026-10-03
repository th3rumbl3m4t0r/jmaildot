//! shelf's folder API (`/api/v1`, docs/API.md in the shelf repo): a key gives one folder;
//! we list, download, upload and make folders there. Attachments and whole messages go to
//! shelf, files from shelf come into the editor as attachments.

use anyhow::{anyhow, bail, Result};
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Debug, Deserialize)]
pub struct KeyInfo {
    pub name: String,
    pub user: String,
    pub folder: String,
    pub write: bool,
    pub expires: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: i64,
    #[serde(default)]
    pub etag: String,
}

#[derive(Deserialize)]
struct Listing {
    entries: Vec<Entry>,
}

#[derive(Clone)]
pub struct Shelf {
    pub base: String,
    key: String,
    http: reqwest::Client,
}

/// `/a b/c#d` → `/a%20b/c%23d`, segment by segment.
pub fn enc_path(p: &str) -> String {
    let mut out = String::new();
    for seg in p.split('/').filter(|s| !s.is_empty()) {
        out.push('/');
        for b in seg.bytes() {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
}

/// `/x/` → `/x`, `x` → `/x`, `` → `/`.
pub fn norm(p: &str) -> String {
    let segs: Vec<&str> = p.split('/').filter(|s| !s.is_empty()).collect();
    format!("/{}", segs.join("/"))
}

impl Shelf {
    /// [url] is the shelf address (`https://shelf.example.com`) with or without `/api/v1`.
    pub fn new(url: &str, key: &str) -> Result<Shelf> {
        let u = url.trim().trim_end_matches('/');
        if u.is_empty() {
            bail!("no shelf address");
        }
        let u = if u.contains("://") { u.to_string() } else { format!("http://{u}") };
        let base = if u.ends_with("/api/v1") { u } else { format!("{u}/api/v1") };
        Ok(Shelf { base, key: key.trim().to_string(), http: crate::jmap::http_client() })
    }

    fn err(code: u16, body: &str) -> anyhow::Error {
        let msg = serde_json::from_str::<serde_json::Value>(body).ok().and_then(|v| v.get("error").and_then(|e| e.as_str()).map(String::from));
        match code {
            401 => anyhow!("shelf: the key is unknown, expired or removed"),
            403 => anyhow!("shelf: {}", msg.unwrap_or_else(|| "not allowed (read-only key, or outside access)".into())),
            404 => anyhow!("shelf: no such file or folder"),
            409 => anyhow!("shelf: {}", msg.unwrap_or_else(|| "something of that name is in the way".into())),
            412 => anyhow!("shelf: the file changed or exists already"),
            _ => anyhow!("shelf said {code}{}", msg.map(|m| format!(": {m}")).unwrap_or_default()),
        }
    }

    async fn check(r: reqwest::Response) -> Result<reqwest::Response> {
        let code = r.status().as_u16();
        if (200..300).contains(&code) {
            return Ok(r);
        }
        let t = r.text().await.unwrap_or_default();
        Err(Self::err(code, &t))
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.http.get(format!("{}{}", self.base, path)).bearer_auth(&self.key).timeout(Duration::from_secs(60))
    }

    pub async fn info(&self) -> Result<KeyInfo> {
        let r = Self::check(self.get("").send().await.map_err(crate::jmap::describe_reqwest)?).await?;
        Ok(r.json().await?)
    }

    /// The entries of a folder: folders first, then files, by name.
    pub async fn list(&self, folder: &str) -> Result<Vec<Entry>> {
        let r = Self::check(self.get(&format!("/list{}", enc_path(folder))).send().await.map_err(crate::jmap::describe_reqwest)?).await?;
        let mut l: Listing = r.json().await?;
        l.entries.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(l.entries)
    }

    pub async fn download(&self, path: &str) -> Result<Vec<u8>> {
        let r = Self::check(
            self.http
                .get(format!("{}/file{}", self.base, enc_path(path)))
                .bearer_auth(&self.key)
                .timeout(Duration::from_secs(1800))
                .send()
                .await
                .map_err(crate::jmap::describe_reqwest)?,
        )
        .await?;
        Ok(r.bytes().await?.to_vec())
    }

    /// Puts a file at [path]; with [replace] false an existing file is not overwritten (412).
    pub async fn upload(&self, path: &str, data: Vec<u8>, replace: bool, mtime: Option<i64>) -> Result<Entry> {
        let mut url = format!("{}/file{}?mkdirs=1", self.base, enc_path(path));
        if let Some(t) = mtime {
            url.push_str(&format!("&mtime={t}"));
        }
        let mut rb = self.http.put(url).bearer_auth(&self.key).body(data).timeout(Duration::from_secs(1800));
        if !replace {
            rb = rb.header("If-None-Match", "*");
        }
        let r = Self::check(rb.send().await.map_err(crate::jmap::describe_reqwest)?).await?;
        Ok(r.json().await?)
    }

    /// Uploads under a free name: `a.pdf`, then `a (2).pdf`, `a (3).pdf`…
    pub async fn upload_free(&self, folder: &str, name: &str, data: Vec<u8>, mtime: Option<i64>) -> Result<Entry> {
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        for n in 1..100 {
            let candidate = if n == 1 { name.to_string() } else { format!("{stem} ({n}){ext}") };
            let path = format!("{}/{}", norm(folder).trim_end_matches('/'), candidate);
            match self.upload(&path, data.clone(), false, mtime).await {
                Ok(e) => return Ok(e),
                Err(e) if e.to_string().contains("exists already") => continue,
                Err(e) => return Err(e),
            }
        }
        bail!("shelf: no free name for {name}")
    }

    pub async fn mkdir(&self, path: &str) -> Result<()> {
        let r = self
            .http
            .post(format!("{}/mkdir{}?parents=1", self.base, enc_path(path)))
            .bearer_auth(&self.key)
            .timeout(Duration::from_secs(60))
            .send()
            .await
            .map_err(crate::jmap::describe_reqwest)?;
        Self::check(r).await?;
        Ok(())
    }
}

/// A file name shelf (and most file systems) will take: no slashes, no control characters.
pub fn safe_name(s: &str) -> String {
    let mut out: String = s.chars().map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c }).collect();
    out = out.trim().trim_matches('.').to_string();
    if out.is_empty() {
        out = "file".into();
    }
    if out.len() > 200 {
        let mut cut = 200;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paths() {
        assert_eq!(enc_path("/a b/c#d"), "/a%20b/c%23d");
        assert_eq!(norm("x//y/"), "/x/y");
        assert_eq!(norm(""), "/");
        assert_eq!(safe_name("re: a/b.pdf"), "re: a_b.pdf");
    }
}
