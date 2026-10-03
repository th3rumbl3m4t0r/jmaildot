//! Calendars: CalDAV sync per account, the events table, instances for the views (expanded
//! on all cores), local edits (pushed first, local wins on a conflict), sharing, reminders.

use crate::caldav::{self, Ace, Cal};
use crate::engine::{Acct, Engine};
use crate::ical::{self, Draft, Occ};
use crate::model::Event;
use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use rayon::prelude::*;
use rusqlite::{params, OptionalExtension};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct Ev {
    pub id: i64,
    pub account: i64,
    pub cal: String,
    pub href: String,
    pub etag: Option<String>,
    pub ics: String,
    pub summary: String,
    pub location: Option<String>,
    pub start: i64,
    pub end: i64,
    pub last: i64,
    pub all_day: bool,
    pub recurring: bool,
    pub alarm_min: Option<i32>,
}

/// One instance on screen: the event, its calendar, the instance.
#[derive(Clone, Debug)]
pub struct Shown {
    pub ev: Arc<Ev>,
    pub cal: Cal,
    pub occ: Occ,
}

fn cal_of(r: &rusqlite::Row) -> rusqlite::Result<Cal> {
    Ok(Cal {
        account: r.get(0)?,
        href: r.get(1)?,
        name: r.get(2)?,
        color: r.get(3)?,
        ctag: r.get(4)?,
        sync_token: r.get(5)?,
        read_only: r.get::<_, i64>(6)? != 0,
        owner: r.get(7)?,
        enabled: r.get::<_, i64>(8)? != 0,
        ord: r.get(9)?,
    })
}

const CAL_COLS: &str = "account, href, name, color, ctag, sync_token, read_only, owner, enabled, ord";
const EV_COLS: &str = "id, account, cal, href, etag, ics, summary, location, start, end, last, all_day, recurring, alarm_min";

fn ev_of(r: &rusqlite::Row) -> rusqlite::Result<Ev> {
    Ok(Ev {
        id: r.get(0)?,
        account: r.get(1)?,
        cal: r.get(2)?,
        href: r.get(3)?,
        etag: r.get(4)?,
        ics: r.get(5)?,
        summary: r.get(6)?,
        location: r.get(7)?,
        start: r.get(8)?,
        end: r.get(9)?,
        last: r.get(10)?,
        all_day: r.get::<_, i64>(11)? != 0,
        recurring: r.get::<_, i64>(12)? != 0,
        alarm_min: r.get(13)?,
    })
}

impl Engine {
    pub fn calendars(&self) -> Vec<Cal> {
        let c = self.db.r();
        let mut st = c.prepare(&format!("SELECT {CAL_COLS} FROM calendars ORDER BY account, owner IS NOT NULL, ord")).unwrap();
        st.query_map([], cal_of).unwrap().filter_map(|r| r.ok()).collect()
    }

    pub fn set_calendar_enabled(&self, account: i64, href: &str, on: bool) -> Result<()> {
        self.db.w().execute("UPDATE calendars SET enabled = ? WHERE account = ? AND href = ?", params![on as i64, account, href])?;
        if on {
            if let Some(a) = self.acct(account) {
                a.cal_kick.notify_one();
            }
        }
        self.emit(Event::CalChanged(account));
        Ok(())
    }

    fn save_calendars(&self, account: i64, list: &[Cal]) -> Result<bool> {
        let before: HashMap<String, Cal> = self.calendars().into_iter().filter(|c| c.account == account).map(|c| (c.href.clone(), c)).collect();
        let mut c = self.db.w();
        let tx = c.transaction()?;
        let mut changed = before.len() != list.len();
        for cal in list {
            let enabled = before.get(&cal.href).map(|b| b.enabled).unwrap_or(true);
            let old = before.get(&cal.href);
            if old.map(|o| o.name != cal.name || o.color != cal.color || o.read_only != cal.read_only || o.owner != cal.owner).unwrap_or(true) {
                changed = true;
            }
            tx.execute(
                "INSERT INTO calendars(account, href, name, color, read_only, owner, enabled, ord) VALUES (?,?,?,?,?,?,?,?) ON CONFLICT(account, href) DO UPDATE SET name = excluded.name, color = excluded.color, read_only = excluded.read_only, owner = excluded.owner, ord = excluded.ord",
                params![account, cal.href, cal.name, cal.color, cal.read_only as i64, cal.owner, enabled as i64, cal.ord],
            )?;
        }
        let keep: HashSet<&String> = list.iter().map(|c| &c.href).collect();
        for h in before.keys().filter(|h| !keep.contains(h)) {
            tx.execute("DELETE FROM events WHERE account = ? AND cal = ?", params![account, h])?;
            tx.execute("DELETE FROM calendars WHERE account = ? AND href = ?", params![account, h])?;
        }
        tx.commit()?;
        Ok(changed)
    }

    fn set_cal_state(&self, account: i64, href: &str, ctag: Option<&str>, token: Option<&str>) -> Result<()> {
        self.db.w().execute("UPDATE calendars SET ctag = ?, sync_token = ? WHERE account = ? AND href = ?", params![ctag, token, account, href])?;
        Ok(())
    }

    fn upsert_event(&self, account: i64, cal: &str, href: &str, etag: Option<&str>, ics: &str) -> Result<()> {
        let local = ical::local_tz();
        let Some(i) = ical::info(ics, local) else {
            // not an event we can show (a VTODO, say): forget it
            self.db.w().execute("DELETE FROM events WHERE account = ? AND href = ? AND dirty = 0", params![account, href])?;
            return Ok(());
        };
        self.db.w().execute(
            "INSERT INTO events(account, cal, href, etag, ics, uid, summary, location, start, end, last, all_day, recurring, alarm_min, dirty, deleted) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,0,0) ON CONFLICT(account, href) DO UPDATE SET cal = excluded.cal, etag = excluded.etag, ics = excluded.ics, uid = excluded.uid, summary = excluded.summary, location = excluded.location, start = excluded.start, end = excluded.end, last = excluded.last, all_day = excluded.all_day, recurring = excluded.recurring, alarm_min = excluded.alarm_min, dirty = 0, deleted = 0",
            params![account, cal, href, etag, ics, i.uid, i.summary, i.location, i.start, i.end, i.last, i.all_day as i64, i.recurring as i64, i.alarm_min],
        )?;
        Ok(())
    }

    /// A local change of the event is still waiting to go to the server.
    pub fn event_pending(&self, id: i64) -> bool {
        self.db.r().query_row("SELECT dirty OR deleted FROM events WHERE id = ?", [id], |r| r.get::<_, i64>(0)).map(|v| v != 0).unwrap_or(false)
    }

    pub fn event(&self, id: i64) -> Option<Ev> {
        self.db.r().query_row(&format!("SELECT {EV_COLS} FROM events WHERE id = ?"), [id], ev_of).optional().ok().flatten()
    }

    /// Every instance of the enabled calendars' events in [lo, hi), expanded on all cores.
    pub fn occurrences(&self, lo: i64, hi: i64) -> Vec<Shown> {
        let cals: HashMap<(i64, String), Cal> = self.calendars().into_iter().filter(|c| c.enabled).map(|c| ((c.account, c.href.clone()), c)).collect();
        let evs: Vec<Ev> = {
            let c = self.db.r();
            let mut st = c.prepare(&format!("SELECT {EV_COLS} FROM events WHERE deleted = 0 AND start < ? AND last > ?")).unwrap();
            st.query_map(params![hi + 2 * 86400, lo - 2 * 86400], ev_of).unwrap().filter_map(|r| r.ok()).collect()
        };
        let local = ical::local_tz();
        let mut out: Vec<Shown> = evs
            .into_par_iter()
            .filter_map(|e| cals.get(&(e.account, e.cal.clone())).map(|c| (e, c.clone())))
            .flat_map_iter(|(e, c)| {
                let ev = Arc::new(e);
                ical::expand(&ev.ics, local, lo, hi).into_iter().map(move |occ| Shown { ev: ev.clone(), cal: c.clone(), occ })
            })
            .collect();
        out.sort_by(|a, b| b.occ.all_day.cmp(&a.occ.all_day).then(a.occ.start.cmp(&b.occ.start)).then(a.occ.summary.cmp(&b.occ.summary)));
        out
    }

    /// Saves an edited or new event locally; the sync sends it.
    pub fn save_event(self: &Arc<Self>, account: i64, cal: &str, id: Option<i64>, d: &Draft) -> Result<i64> {
        let local = ical::local_tz();
        let existing = id.and_then(|i| self.event(i));
        let calendar = self.calendars().into_iter().find(|c| c.account == account && c.href == cal).ok_or_else(|| anyhow!("no such calendar"))?;
        if calendar.read_only {
            return Err(anyhow!("that calendar is read only"));
        }
        let ics = ical::write_event(d, local, existing.as_ref().map(|e| e.ics.as_str()));
        let info = ical::info(&ics, local).ok_or_else(|| anyhow!("the event could not be written"))?;
        let new_id = match existing {
            Some(e) if e.cal == cal && e.account == account => {
                self.db.w().execute(
                    "UPDATE events SET ics = ?, summary = ?, location = ?, start = ?, end = ?, last = ?, all_day = ?, recurring = ?, alarm_min = ?, dirty = 1 WHERE id = ?",
                    params![ics, info.summary, info.location, info.start, info.end, info.last, info.all_day as i64, info.recurring as i64, info.alarm_min, e.id],
                )?;
                e.id
            }
            other => {
                // new, or moved to another calendar: a new resource there, the old one goes
                if let Some(old) = other {
                    self.delete_event(old.id)?;
                }
                let name: String = info.uid.chars().filter(|c| c.is_ascii_alphanumeric() || "-_".contains(*c)).collect();
                let href = format!("{}{}.ics", cal, if name.is_empty() { format!("{:x}", rand::random::<u64>()) } else { name });
                let c = self.db.w();
                c.execute(
                    "INSERT INTO events(account, cal, href, etag, ics, uid, summary, location, start, end, last, all_day, recurring, alarm_min, dirty, deleted) VALUES (?,?,?,NULL,?,?,?,?,?,?,?,?,?,?,1,0)",
                    params![account, cal, href, ics, info.uid, info.summary, info.location, info.start, info.end, info.last, info.all_day as i64, info.recurring as i64, info.alarm_min],
                )?;
                c.last_insert_rowid()
            }
        };
        self.after_cal_edit(account);
        Ok(new_id)
    }

    pub fn delete_event(self: &Arc<Self>, id: i64) -> Result<()> {
        let Some(e) = self.event(id) else { return Ok(()) };
        if e.etag.is_none() {
            // never reached the server
            self.db.w().execute("DELETE FROM events WHERE id = ?", [id])?;
        } else {
            self.db.w().execute("UPDATE events SET deleted = 1 WHERE id = ?", [id])?;
        }
        self.after_cal_edit(e.account);
        Ok(())
    }

    /// Leaves one instance of a series out (EXDATE).
    pub fn skip_instance(self: &Arc<Self>, id: i64, instance: i64, all_day: bool) -> Result<()> {
        let e = self.event(id).ok_or_else(|| anyhow!("the event is gone"))?;
        let local = ical::local_tz();
        let ics = ical::exclude(&e.ics, instance, all_day, local).ok_or_else(|| anyhow!("the event could not be changed"))?;
        let info = ical::info(&ics, local).ok_or_else(|| anyhow!("the event could not be changed"))?;
        self.db.w().execute("UPDATE events SET ics = ?, last = ?, dirty = 1 WHERE id = ?", params![ics, info.last, id])?;
        self.after_cal_edit(e.account);
        Ok(())
    }

    fn after_cal_edit(&self, account: i64) {
        self.emit(Event::CalChanged(account));
        if let Some(a) = self.acct(account) {
            a.cal_kick.notify_one();
        }
    }

    pub async fn calendar_acl(&self, account: i64, href: &str) -> Result<Vec<Ace>> {
        let a = self.acct(account).ok_or_else(|| anyhow!("the account is not connected"))?;
        a.dav.acl(href).await
    }

    pub async fn set_calendar_acl(&self, account: i64, href: &str, aces: Vec<Ace>) -> Result<()> {
        let a = self.acct(account).ok_or_else(|| anyhow!("the account is not connected"))?;
        let principal = self.db.setting(&format!("cal.principal.{account}")).ok_or_else(|| anyhow!("calendars not synced yet"))?;
        let root = format!("{}/", principal.trim_end_matches('/').rsplit_once('/').map(|x| x.0).unwrap_or(""));
        a.dav.set_acl(href, &aces, &root).await
    }

    /// Reminders while the program runs: every half minute, what came due since the last look.
    pub async fn reminders(self: Arc<Self>) {
        let mut last = crate::db::now() - 120;
        loop {
            tokio::time::sleep(Duration::from_secs(30)).await;
            let now = crate::db::now();
            if self.db.flag("cal.reminders", true) {
                let local = ical::local_tz();
                for s in self.occurrences(now - 86400, now + 3 * 86400) {
                    let Some(min) = s.ev.alarm_min else { continue };
                    let at = s.occ.start - (min as i64) * 60;
                    if at > last && at <= now {
                        let when = if s.occ.all_day { "all day".to_string() } else { format!("{} – {}", ical::hm(s.occ.start, local), ical::hm(s.occ.end, local)) };
                        let title = if s.occ.summary.is_empty() { "(no title)".to_string() } else { s.occ.summary.clone() };
                        self.emit(Event::Reminder(title, when, s.occ.location.clone()));
                    }
                }
            }
            last = now;
        }
    }
}

pub async fn run(e: Arc<Engine>, a: Arc<Acct>) {
    let mut fails = 0u32;
    loop {
        if a.stop.load(Ordering::Relaxed) {
            break;
        }
        let wait = match sync(&e, &a).await {
            Ok(()) => {
                fails = 0;
                Duration::from_secs(300)
            }
            Err(err) => {
                fails += 1;
                let msg = jmap_describe(&err);
                e.status(a.id, &format!("calendar: {msg}"), true);
                Duration::from_secs((60 * fails as u64).min(1800))
            }
        };
        tokio::select! {
            _ = a.cal_kick.notified() => {}
            _ = tokio::time::sleep(wait) => {}
        }
    }
}

fn jmap_describe(e: &anyhow::Error) -> String {
    crate::jmap::describe(e)
}

async fn sync(e: &Arc<Engine>, a: &Arc<Acct>) -> Result<()> {
    let id = a.id;
    let home = match e.db.setting(&format!("cal.home.{id}")) {
        Some(h) => h,
        None => {
            let (p, h) = a.dav.discover().await?;
            e.db.set_setting(&format!("cal.principal.{id}"), &p)?;
            e.db.set_setting(&format!("cal.home.{id}"), &h)?;
            h
        }
    };
    let list = a.dav.calendars(id, &home).await?;
    let before: HashMap<String, Cal> = e.calendars().into_iter().filter(|c| c.account == id).map(|c| (c.href.clone(), c)).collect();
    let mut changed = e.save_calendars(id, &list)?;
    let enabled: Vec<Cal> = e.calendars().into_iter().filter(|c| c.account == id && c.enabled).collect();
    let results: Vec<Result<bool>> = futures_util::stream::iter(enabled.into_iter().map(|c| {
        let fresh = list.iter().find(|x| x.href == c.href).cloned();
        let old = before.get(&c.href).cloned();
        sync_calendar(e.clone(), a.clone(), c, fresh, old)
    }))
    .buffer_unordered(3)
    .collect()
    .await;
    for r in results {
        changed |= r?;
    }
    if changed {
        e.emit(Event::CalChanged(id));
    }
    Ok(())
}

/// Local changes up, then the server's changes down. True when anything changed here.
async fn sync_calendar(e: Arc<Engine>, a: Arc<Acct>, cal: Cal, fresh: Option<Cal>, old: Option<Cal>) -> Result<bool> {
    let id = a.id;
    let mut changed = false;
    // ---- up ----
    let dirty: Vec<(i64, String, Option<String>, String, bool)> = {
        let c = e.db.r();
        let mut st = c.prepare("SELECT id, href, etag, ics, deleted FROM events WHERE account = ? AND cal = ? AND (dirty = 1 OR deleted = 1)").unwrap();
        st.query_map(params![id, cal.href], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get::<_, i64>(4)? != 0))).unwrap().filter_map(|r| r.ok()).collect()
    };
    let pushed = !dirty.is_empty();
    for (eid, href, etag, ics, deleted) in dirty {
        if deleted {
            let r = a.dav.delete(&href, etag.as_deref()).await;
            match r {
                Err(err) if caldav::is_conflict(&err) => {
                    let cur = a.dav.etag(&href).await?;
                    if cur.is_some() {
                        a.dav.delete(&href, cur.as_deref()).await?;
                    }
                }
                other => other?,
            }
            e.db.w().execute("DELETE FROM events WHERE id = ?", [eid])?;
        } else {
            let r = a.dav.put(&href, &ics, etag.as_deref()).await;
            let new_etag = match r {
                Err(err) if caldav::is_conflict(&err) => {
                    // someone changed it first: ours wins
                    let cur = a.dav.etag(&href).await?;
                    a.dav.put(&href, &ics, cur.as_deref()).await?
                }
                other => other?,
            };
            e.db.w().execute("UPDATE events SET dirty = 0, etag = ? WHERE id = ?", params![new_etag, eid])?;
        }
        changed = true;
    }
    // ---- down ----
    let fresh = fresh.unwrap_or(cal.clone());
    let unchanged = !pushed && old.as_ref().map(|o| o.ctag.is_some() && o.ctag == fresh.ctag && o.sync_token.is_some()).unwrap_or(false);
    if unchanged {
        return Ok(changed);
    }
    let local: HashMap<String, (Option<String>, bool)> = {
        let c = e.db.r();
        let mut st = c.prepare("SELECT href, etag, dirty OR deleted FROM events WHERE account = ? AND cal = ?").unwrap();
        st.query_map(params![id, cal.href], |r| Ok((r.get::<_, String>(0)?, (r.get::<_, Option<String>>(1)?, r.get::<_, i64>(2)? != 0)))).unwrap().filter_map(|r| r.ok()).collect()
    };
    let token = old.as_ref().and_then(|o| o.sync_token.clone()).filter(|_| !local.is_empty() || true);
    let (want, gone, new_token): (Vec<String>, Vec<String>, Option<String>) = match a.dav.sync(&cal.href, token.as_deref()).await? {
        Some(r) => {
            let want = r.changed.into_iter().filter(|(h, et)| local.get(h).map(|(le, busy)| !busy && (le != et || et.is_none())).unwrap_or(true)).map(|(h, _)| h).collect();
            (want, r.gone, r.token)
        }
        None => {
            let all = a.dav.list(&cal.href).await?;
            let want = all.iter().filter(|(h, et)| local.get(*h).map(|(le, busy)| !busy && (le != *et || et.is_none())).unwrap_or(true)).map(|(h, _)| h.clone()).collect();
            let gone = local.iter().filter(|(h, (_, busy))| !busy && !all.contains_key(*h)).map(|(h, _)| h.clone()).collect();
            let t = a.dav.sync(&cal.href, None).await.ok().flatten().and_then(|r| r.token);
            (want, gone, t)
        }
    };
    for h in &gone {
        if local.get(h).map(|(_, busy)| !busy).unwrap_or(false) {
            e.db.w().execute("DELETE FROM events WHERE account = ? AND href = ?", params![id, h])?;
            changed = true;
        }
    }
    for chunk in want.chunks(50) {
        for f in a.dav.multiget(&cal.href, chunk).await? {
            e.upsert_event(id, &cal.href, &f.href, f.etag.as_deref(), &f.ics)?;
            changed = true;
        }
    }
    e.set_cal_state(id, &cal.href, fresh.ctag.as_deref(), new_token.as_deref().or(fresh.sync_token.as_deref()))?;
    Ok(changed)
}
