//! Keeping one account's mail in step with the server.
//!
//! Each round: identities and folders, then Email/changes since the state we hold (or the
//! full id list the first time, or when the state is too old to replay), then every message
//! not yet stored is downloaded. Downloads go out as several Blob/get batches at once (as
//! many as the server allows), and the batches are decrypted and taken apart on all cores
//! (rayon) before one transaction stores them. The event source (push) wakes the loop as
//! soon as something changes on the server; without push it runs every few minutes.

use crate::db::{Meta, Stored};
use crate::engine::{Acct, Engine};
use crate::jmap::{self, method_error};
use crate::mime;
use crate::model::Event;
use anyhow::Result;
use futures_util::StreamExt;
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

const BATCH_BYTES: i64 = 6 << 20;
const BATCH_COUNT: usize = 40;
const SINGLE_BYTES: i64 = 12 << 20;

pub async fn run(e: Arc<Engine>, a: Arc<Acct>) {
    let push = tokio::spawn(push_loop(a.clone()));
    let poll = tokio::spawn(poll_loop(e.clone(), a.clone()));
    let mut fails = 0u32;
    loop {
        if a.stop.load(Ordering::Relaxed) {
            break;
        }
        a.syncing.store(true, Ordering::Relaxed);
        e.status(a.id, "syncing…", false);
        let r = round(&e, &a).await;
        a.syncing.store(false, Ordering::Relaxed);
        let wait = match r {
            Ok(()) => {
                fails = 0;
                let (n, stored) = e.db.counts(a.id);
                let note = if stored < n { format!(" · {} of {} stored", stored, n) } else { String::new() };
                e.status(a.id, &format!("synced {}{}", chrono::Local::now().format("%H:%M"), note), false);
                Duration::from_secs(900)
            }
            Err(err) => {
                fails += 1;
                e.status(a.id, &jmap::describe(&err), true);
                Duration::from_secs((30 * fails as u64).min(600))
            }
        };
        tokio::select! {
            _ = a.kick.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
    push.abort();
    poll.abort();
}

/// Push may be missing or silent (it is on the 0.13 test server): every half minute one
/// small request compares the server's states with ours and wakes the loop on a change.
async fn poll_loop(e: Arc<Engine>, a: Arc<Acct>) {
    let mut last_mb = String::new();
    loop {
        let secs = e.db.setting("mail.poll").and_then(|v| v.parse::<u64>().ok()).unwrap_or(30).max(5);
        tokio::time::sleep(Duration::from_secs(secs)).await;
        if a.stop.load(Ordering::Relaxed) {
            return;
        }
        if a.syncing.load(Ordering::Relaxed) {
            continue;
        }
        if let Ok((email, mb)) = a.jmap.states().await {
            let ours = e.db.email_state(a.id).unwrap_or_default();
            if email != ours || (!last_mb.is_empty() && mb != last_mb) {
                a.kick.notify_one();
            }
            last_mb = mb;
        }
    }
}

async fn round(e: &Arc<Engine>, a: &Arc<Acct>) -> Result<()> {
    let id = a.id;
    // identities: the editor needs them offline too
    if let Ok(ids) = a.jmap.identities().await {
        let mut acc = a.account();
        if acc.identities != ids {
            acc.identities = ids;
            if acc.identity_id.is_none() {
                acc.identity_id = acc.identities.iter().find(|i| i.email.eq_ignore_ascii_case(&acc.email)).map(|i| i.id.clone());
            }
            e.db.update_account(&acc)?;
            *a.account.write().unwrap() = acc;
        }
    }
    let mbs = a.jmap.mailboxes(id).await?;
    if e.db.save_mailboxes(id, &mbs)? {
        e.emit(Event::Changed(id));
    }
    match e.db.email_state(id) {
        None => full(e, a).await?,
        Some(s) => {
            if let Err(err) = changes(e, a, &s).await {
                match method_error(&err).map(|m| m.kind.as_str()) {
                    Some("cannotCalculateChanges") | Some("invalidArguments") => full(e, a).await?,
                    _ => return Err(err),
                }
            }
        }
    }
    download(e, a).await?;
    Ok(())
}

/// The whole id list: what is gone locally goes, every header row is (re)fetched.
async fn full(e: &Arc<Engine>, a: &Arc<Acct>) -> Result<()> {
    let id = a.id;
    let state = a.jmap.email_state().await?;
    let ids = a.jmap.all_ids().await?;
    let server: HashSet<&String> = ids.iter().collect();
    let gone: Vec<String> = e.db.all_jids(id).into_iter().filter(|j| !server.contains(j)).collect();
    if !gone.is_empty() {
        e.db.remove_jids(id, &gone)?;
    }
    let total = ids.len();
    let mut done = 0;
    for chunk in ids.chunks(500) {
        if a.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let metas = a.jmap.metas(chunk).await?;
        e.db.save_meta(id, &metas)?;
        done += chunk.len();
        e.status(id, &format!("listing {done} of {total} messages…"), false);
        e.emit(Event::Changed(id));
    }
    e.db.set_email_state(id, Some(&state))?;
    Ok(())
}

async fn changes(e: &Arc<Engine>, a: &Arc<Acct>, since: &str) -> Result<()> {
    let id = a.id;
    let mut s = since.to_string();
    let mut any = false;
    let mut fresh_all = Vec::new();
    loop {
        let c = a.jmap.changes(&s).await?;
        let mut ids: Vec<String> = c.created.iter().chain(c.updated.iter()).cloned().collect();
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            let metas: Vec<Meta> = a.jmap.metas(&ids).await?;
            fresh_all.extend(e.db.save_meta(id, &metas)?);
            any = true;
        }
        if !c.destroyed.is_empty() {
            e.db.remove_jids(id, &c.destroyed)?;
            any = true;
        }
        s = c.new_state;
        if !c.has_more {
            break;
        }
    }
    e.db.set_email_state(id, Some(&s))?;
    if any {
        e.emit(Event::Changed(id));
    }
    // new unread mail in a folder we watch (and recent: not a moved old message)
    if !fresh_all.is_empty() {
        let watched: HashSet<String> = e.db.mailboxes(id).into_iter().filter(|m| m.notifies()).map(|m| m.id).collect();
        let now = crate::db::now();
        let new: Vec<i64> = e
            .db
            .msgs(&fresh_all)
            .into_iter()
            .filter(|m| !m.seen() && m.received > now - 86400 && m.mailboxes.iter().any(|b| watched.contains(b)))
            .map(|m| m.id)
            .collect();
        if !new.is_empty() {
            e.emit(Event::NewMail(id, new));
        }
    }
    Ok(())
}

/// Downloads every message not stored yet, newest first.
async fn download(e: &Arc<Engine>, a: &Arc<Acct>) -> Result<()> {
    let id = a.id;
    let sess = a.jmap.session().await?;
    let parallel = sess.sem.available_permits().max(1);
    loop {
        if a.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let pending = e.db.pending(id, 400);
        if pending.is_empty() {
            break;
        }
        // batches by size; big ones alone
        let mut batches: Vec<Vec<(i64, String, i64)>> = Vec::new();
        let mut cur: Vec<(i64, String, i64)> = Vec::new();
        let mut bytes = 0;
        for p in pending {
            if p.2 >= SINGLE_BYTES {
                batches.push(vec![p]);
                continue;
            }
            if !cur.is_empty() && (bytes + p.2 > BATCH_BYTES || cur.len() >= BATCH_COUNT) {
                batches.push(std::mem::take(&mut cur));
                bytes = 0;
            }
            bytes += p.2;
            cur.push(p);
        }
        if !cur.is_empty() {
            batches.push(cur);
        }
        let results: Vec<Result<Vec<(i64, Option<Vec<u8>>)>>> = futures_util::stream::iter(batches.into_iter().map(|b| fetch_batch(a.clone(), b)))
            .buffer_unordered(parallel)
            .collect()
            .await;
        let mut got = Vec::new();
        for r in results {
            got.extend(r?);
        }
        store(e, id, got).await?;
        let (n, stored) = e.db.counts(id);
        e.emit(Event::Progress(id, stored, n));
        e.emit(Event::Changed(id));
    }
    Ok(())
}

async fn fetch_batch(a: Arc<Acct>, batch: Vec<(i64, String, i64)>) -> Result<Vec<(i64, Option<Vec<u8>>)>> {
    if batch.len() == 1 && batch[0].2 >= SINGLE_BYTES {
        let (id, blob, _) = &batch[0];
        return Ok(vec![(*id, a.jmap.download(blob, "message.eml", "message/rfc822").await.ok())]);
    }
    let blobs: Vec<String> = batch.iter().map(|b| b.1.clone()).collect::<HashSet<_>>().into_iter().collect();
    let got: HashMap<String, Vec<u8>> = a.jmap.blobs(&blobs).await?.into_iter().collect();
    Ok(batch.into_iter().map(|(id, blob, _)| (id, got.get(&blob).cloned())).collect())
}

/// Decrypts and takes apart on all cores, writes the files, stores in one transaction.
pub async fn store(e: &Arc<Engine>, account: i64, got: Vec<(i64, Option<Vec<u8>>)>) -> Result<()> {
    let ring = e.keys.read().unwrap().clone();
    let db = e.db.clone();
    let mut locked = false;
    let stored: Vec<Stored> = tokio::task::spawn_blocking(move || {
        got.into_par_iter()
            .map(|(id, raw)| match raw {
                None => Stored { id, fetched: 3, err: Some("not on the server any more".into()), ..Default::default() },
                Some(raw) => {
                    let p = mime::process(&raw, &ring);
                    let path = crate::paths::eml_path(account, id);
                    let write = (|| -> std::io::Result<()> {
                        if let Some(d) = path.parent() {
                            crate::paths::ensure(d)?;
                        }
                        // still-encrypted messages keep the original, decrypted ones the plain text
                        std::fs::write(&path, if p.fetched == 2 { &raw } else { &p.eml })
                    })();
                    match write {
                        Err(err) => Stored { id, fetched: 0, err: Some(format!("cannot write {}: {err}", path.display())), ..Default::default() },
                        Ok(()) => Stored {
                            id,
                            fetched: p.fetched,
                            encrypted: p.encrypted,
                            pgp: p.note.as_ref().and_then(|n| serde_json::to_string(n).ok()),
                            err: p.err,
                            has_att: p.has_att,
                            preview: Some(p.preview),
                            text: Some(p.text),
                        },
                    }
                }
            })
            .collect()
    })
    .await?;
    if stored.iter().any(|s| s.fetched == 2 && s.err.as_deref().map(|x| x.starts_with("locked")).unwrap_or(false)) {
        locked = true;
    }
    let failed_write = stored.iter().any(|s| s.fetched == 0);
    db.store_processed(&stored)?;
    if locked {
        e.emit(Event::NeedPassphrase);
    }
    if failed_write {
        anyhow::bail!("{}", stored.iter().find(|s| s.fetched == 0).and_then(|s| s.err.clone()).unwrap_or_default());
    }
    Ok(())
}

/// After the key was unlocked (or added): what is stored encrypted gets decrypted now.
pub async fn reprocess(e: &Arc<Engine>, account: i64) -> Result<usize> {
    let ids = e.db.still_encrypted(account);
    let mut n = 0;
    for chunk in ids.chunks(200) {
        let got: Vec<(i64, Option<Vec<u8>>)> = chunk.iter().map(|id| (*id, std::fs::read(crate::paths::eml_path(account, *id)).ok())).collect();
        store(e, account, got).await?;
        n += chunk.len();
    }
    if n > 0 {
        e.emit(Event::Changed(account));
    }
    Ok(n)
}

/// The event source: any state change wakes the sync loop.
async fn push_loop(a: Arc<Acct>) {
    loop {
        if a.stop.load(Ordering::Relaxed) {
            return;
        }
        match a.jmap.event_source().await {
            Ok(Some(resp)) => {
                a.push_ok.store(true, Ordering::Relaxed);
                let mut stream = resp.bytes_stream();
                let mut buf = String::new();
                let mut event = String::new();
                loop {
                    let next = tokio::time::timeout(Duration::from_secs(200), stream.next()).await;
                    let Ok(Some(Ok(chunk))) = next else { break };
                    buf.push_str(&String::from_utf8_lossy(&chunk));
                    while let Some(i) = buf.find('\n') {
                        let line = buf[..i].trim_end_matches('\r').to_string();
                        buf.drain(..=i);
                        if let Some(v) = line.strip_prefix("event:") {
                            event = v.trim().to_string();
                        } else if line.starts_with("data:") {
                            if event == "state" || event.is_empty() {
                                a.kick.notify_one();
                                a.cal_kick.notify_one();
                            }
                        } else if line.is_empty() {
                            event.clear();
                        }
                    }
                }
                a.push_ok.store(false, Ordering::Relaxed);
            }
            Ok(None) => {
                a.push_ok.store(false, Ordering::Relaxed);
                return;
            }
            Err(_) => {
                a.push_ok.store(false, Ordering::Relaxed);
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}
