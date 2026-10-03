//! iCalendar (RFC 5545) the way a calendar app needs it: a parser that keeps every property
//! and component it doesn't know (so other clients' data survives our edits), the columns
//! we store per event, the instances of an event in a range (RRULE / RDATE / EXDATE /
//! RECURRENCE-ID, with the rrule crate in the event's own zone), and writing edits back.
//!
//! Conventions: timed values are instants (seconds). All-day values are the day as UTC
//! midnight ("day key"), which is zone-free; the window turns them into local days.

use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use std::str::FromStr;

pub const PRODID: &str = "-//jmaildot//maildot//EN";
const MAX_INSTANCES: u16 = 3000;

#[derive(Clone, Debug, PartialEq)]
pub struct Prop {
    pub name: String,
    pub params: Vec<(String, String)>,
    pub value: String,
}

impl Prop {
    pub fn new(name: &str, value: &str) -> Prop {
        Prop { name: name.to_string(), params: vec![], value: value.to_string() }
    }
    pub fn with(mut self, k: &str, v: &str) -> Prop {
        self.params.push((k.to_string(), v.to_string()));
        self
    }
    pub fn param(&self, k: &str) -> Option<&str> {
        self.params.iter().find(|(n, _)| n.eq_ignore_ascii_case(k)).map(|(_, v)| v.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Comp {
    pub name: String,
    pub props: Vec<Prop>,
    pub comps: Vec<Comp>,
}

impl Comp {
    pub fn new(name: &str) -> Comp {
        Comp { name: name.to_string(), ..Default::default() }
    }
    pub fn prop(&self, n: &str) -> Option<&Prop> {
        self.props.iter().find(|p| p.name.eq_ignore_ascii_case(n))
    }
    pub fn all(&self, n: &str) -> Vec<&Prop> {
        self.props.iter().filter(|p| p.name.eq_ignore_ascii_case(n)).collect()
    }
    pub fn text(&self, n: &str) -> Option<String> {
        self.prop(n).map(|p| unescape(&p.value))
    }
    pub fn remove(&mut self, n: &str) {
        self.props.retain(|p| !p.name.eq_ignore_ascii_case(n));
    }
    /// Replaces the first property of that name (keeping its place), or adds it.
    pub fn set(&mut self, p: Prop) {
        match self.props.iter().position(|x| x.name.eq_ignore_ascii_case(&p.name)) {
            Some(i) => {
                self.props[i] = p.clone();
                let name = p.name.clone();
                let mut seen = false;
                self.props.retain(|x| {
                    if x.name.eq_ignore_ascii_case(&name) {
                        if seen {
                            return false;
                        }
                        seen = true;
                    }
                    true
                });
            }
            None => self.props.push(p),
        }
    }
    pub fn set_text(&mut self, n: &str, v: Option<&str>) {
        match v.map(str::trim).filter(|s| !s.is_empty()) {
            Some(s) => self.set(Prop::new(n, &escape(s))),
            None => self.remove(n),
        }
    }
    pub fn events(&self) -> Vec<&Comp> {
        self.comps.iter().filter(|c| c.name.eq_ignore_ascii_case("VEVENT")).collect()
    }
}

pub fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(x) => out.push(x),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

pub fn escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace('\n', "\\n")
}

fn parse_line(l: &str) -> Option<Prop> {
    // name and params up to the first ':' outside quotes
    let mut quoted = false;
    let mut split = None;
    for (i, c) in l.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                split = Some(i);
                break;
            }
            _ => {}
        }
    }
    let i = split?;
    let (head, value) = (&l[..i], &l[i + 1..]);
    let mut parts = Vec::new();
    let mut cur = String::new();
    quoted = false;
    for c in head.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                cur.push(c)
            }
            ';' if !quoted => parts.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    parts.push(cur);
    let name = parts[0].trim().to_uppercase();
    if name.is_empty() {
        return None;
    }
    let params = parts[1..]
        .iter()
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.trim().to_uppercase(), v.trim().trim_matches('"').to_string()))
        })
        .collect();
    Some(Prop { name, params, value: value.to_string() })
}

/// The first VCALENDAR in the text (None for anything that doesn't parse).
pub fn parse(text: &str) -> Option<Comp> {
    let mut lines: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let l = raw.strip_suffix('\r').unwrap_or(raw);
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            lines.last_mut().unwrap().push_str(&l[1..]);
        } else if !l.trim().is_empty() {
            lines.push(l.to_string());
        }
    }
    let mut stack: Vec<Comp> = Vec::new();
    for l in lines {
        let Some(p) = parse_line(&l) else { continue };
        if p.name == "BEGIN" {
            stack.push(Comp::new(&p.value.trim().to_uppercase()));
        } else if p.name == "END" {
            let c = stack.pop()?;
            match stack.last_mut() {
                Some(parent) => parent.comps.push(c),
                None => return if c.name == "VCALENDAR" { Some(c) } else { None },
            }
        } else if let Some(c) = stack.last_mut() {
            c.props.push(p);
        }
    }
    None
}

fn fold(line: &str) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in line.chars() {
        let l = c.len_utf8();
        if n + l > 75 {
            out.push_str("\r\n ");
            n = 1;
        }
        out.push(c);
        n += l;
    }
    out
}

fn write_comp(c: &Comp, out: &mut String) {
    out.push_str(&format!("BEGIN:{}\r\n", c.name));
    for p in &c.props {
        let mut l = p.name.clone();
        for (k, v) in &p.params {
            let q = v.contains([':', ';', ',']);
            l.push_str(&format!(";{k}={}", if q { format!("\"{v}\"") } else { v.clone() }));
        }
        l.push(':');
        l.push_str(&p.value);
        out.push_str(&fold(&l));
        out.push_str("\r\n");
    }
    for k in &c.comps {
        write_comp(k, out);
    }
    out.push_str(&format!("END:{}\r\n", c.name));
}

pub fn write(c: &Comp) -> String {
    let mut s = String::new();
    write_comp(c, &mut s);
    s
}

// ---- time ----

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Zone {
    Utc,
    Tz(Tz),
    /// No zone: the user's local time.
    Floating,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Dt {
    Date(NaiveDate),
    Time(NaiveDateTime, Zone),
}

pub fn local_tz() -> Tz {
    if let Ok(z) = std::env::var("TZ") {
        if let Ok(t) = Tz::from_str(z.trim_start_matches(':')) {
            return t;
        }
    }
    iana_time_zone::get_timezone().ok().and_then(|z| Tz::from_str(&z).ok()).unwrap_or(Tz::UTC)
}

const WINDOWS: &[(&str, &str)] = &[
    ("Central Europe Standard Time", "Europe/Prague"),
    ("Central European Standard Time", "Europe/Warsaw"),
    ("W. Europe Standard Time", "Europe/Berlin"),
    ("Romance Standard Time", "Europe/Paris"),
    ("GMT Standard Time", "Europe/London"),
    ("E. Europe Standard Time", "Europe/Chisinau"),
    ("FLE Standard Time", "Europe/Kiev"),
    ("Russian Standard Time", "Europe/Moscow"),
    ("Eastern Standard Time", "America/New_York"),
    ("Central Standard Time", "America/Chicago"),
    ("Mountain Standard Time", "America/Denver"),
    ("Pacific Standard Time", "America/Los_Angeles"),
    ("UTC", "UTC"),
    ("Coordinated Universal Time", "UTC"),
    ("China Standard Time", "Asia/Shanghai"),
    ("Tokyo Standard Time", "Asia/Tokyo"),
    ("India Standard Time", "Asia/Kolkata"),
    ("AUS Eastern Standard Time", "Australia/Sydney"),
];

/// A TZID as a zone: an Olson id, a VTIMEZONE's X-LIC-LOCATION, a Windows name, else local.
pub fn zone_of(tzid: &str, cal: Option<&Comp>) -> Zone {
    let id = tzid.trim().trim_start_matches('/');
    if let Ok(t) = Tz::from_str(id) {
        return Zone::Tz(t);
    }
    if let Some(cal) = cal {
        for vt in cal.comps.iter().filter(|c| c.name == "VTIMEZONE") {
            if vt.prop("TZID").map(|p| p.value.trim() == tzid.trim()).unwrap_or(false) {
                if let Some(loc) = vt.prop("X-LIC-LOCATION") {
                    if let Ok(t) = Tz::from_str(loc.value.trim()) {
                        return Zone::Tz(t);
                    }
                }
            }
        }
    }
    for (w, o) in WINDOWS {
        if id.eq_ignore_ascii_case(w) {
            if let Ok(t) = Tz::from_str(o) {
                return Zone::Tz(t);
            }
        }
    }
    // "Europe/Prague" hidden in a longer id (some exporters prefix it)
    for part in id.split(['/', ' ']).collect::<Vec<_>>().windows(2) {
        if let Ok(t) = Tz::from_str(&format!("{}/{}", part[0], part[1])) {
            return Zone::Tz(t);
        }
    }
    Zone::Floating
}

pub fn parse_value(v: &str, date: bool, zone: Zone) -> Option<Dt> {
    let v = v.trim();
    if date || (v.len() == 8 && !v.contains('T')) {
        return NaiveDate::parse_from_str(&v[..8.min(v.len())], "%Y%m%d").ok().map(Dt::Date);
    }
    let (body, utc) = match v.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (v, false),
    };
    let ndt = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").or_else(|_| NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M")).ok()?;
    Some(Dt::Time(ndt, if utc { Zone::Utc } else { zone }))
}

pub fn dt_of(p: &Prop, cal: Option<&Comp>) -> Option<Dt> {
    let date = p.param("VALUE").map(|v| v.eq_ignore_ascii_case("DATE")).unwrap_or(false);
    let zone = p.param("TZID").map(|t| zone_of(t, cal)).unwrap_or(Zone::Floating);
    parse_value(p.value.split(',').next().unwrap_or(""), date, zone)
}

fn all_dts(p: &Prop, cal: Option<&Comp>) -> Vec<Dt> {
    let date = p.param("VALUE").map(|v| v.eq_ignore_ascii_case("DATE")).unwrap_or(false);
    let zone = p.param("TZID").map(|t| zone_of(t, cal)).unwrap_or(Zone::Floating);
    p.value.split(',').filter_map(|v| parse_value(v, date, zone)).collect()
}

fn resolve<T: TimeZone>(tz: &T, ndt: &NaiveDateTime) -> DateTime<T> {
    tz.from_local_datetime(ndt).earliest().or_else(|| tz.from_local_datetime(&(*ndt + Duration::hours(1))).earliest()).unwrap_or_else(|| tz.from_utc_datetime(ndt))
}

pub fn day_key(d: NaiveDate) -> i64 {
    d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp()
}

pub fn date_of_key(k: i64) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(k, 0).map(|d| d.date_naive()).unwrap_or_default()
}

impl Dt {
    pub fn is_date(&self) -> bool {
        matches!(self, Dt::Date(_))
    }
    /// Seconds: the instant, or the day key for a date.
    pub fn key(&self, local: Tz) -> i64 {
        match self {
            Dt::Date(d) => day_key(*d),
            Dt::Time(ndt, Zone::Utc) => ndt.and_utc().timestamp(),
            Dt::Time(ndt, Zone::Tz(t)) => resolve(t, ndt).timestamp(),
            Dt::Time(ndt, Zone::Floating) => resolve(&local, ndt).timestamp(),
        }
    }
    fn rr(&self, local: Tz) -> DateTime<rrule::Tz> {
        match self {
            Dt::Date(d) => resolve(&rrule::Tz::Tz(Tz::UTC), &d.and_hms_opt(0, 0, 0).unwrap()),
            Dt::Time(ndt, Zone::Utc) => resolve(&rrule::Tz::Tz(Tz::UTC), ndt),
            Dt::Time(ndt, Zone::Tz(t)) => resolve(&rrule::Tz::Tz(*t), ndt),
            Dt::Time(ndt, Zone::Floating) => resolve(&rrule::Tz::Tz(local), ndt),
        }
    }
}

/// `P1D`, `-PT15M`, `P1W`, `PT1H30M` → seconds.
pub fn duration(s: &str) -> Option<i64> {
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let s = s.strip_prefix('P')?;
    let mut total = 0i64;
    let mut num = String::new();
    let mut time = false;
    for c in s.chars() {
        match c {
            'T' => time = true,
            '0'..='9' => num.push(c),
            _ => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += n * match (c, time) {
                    ('W', _) => 7 * 86400,
                    ('D', _) => 86400,
                    ('H', true) => 3600,
                    ('M', true) => 60,
                    ('S', true) => 1,
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

// ---- reading ----

pub fn master(cal: &Comp) -> Option<&Comp> {
    let ev = cal.events();
    ev.iter().find(|e| e.prop("RECURRENCE-ID").is_none()).copied().or_else(|| ev.first().copied())
}

fn master_mut(cal: &mut Comp) -> Option<&mut Comp> {
    let i = cal
        .comps
        .iter()
        .position(|e| e.name == "VEVENT" && e.prop("RECURRENCE-ID").is_none())
        .or_else(|| cal.comps.iter().position(|e| e.name == "VEVENT"))?;
    Some(&mut cal.comps[i])
}

fn exceptions(cal: &Comp) -> Vec<&Comp> {
    cal.events().into_iter().filter(|e| e.prop("RECURRENCE-ID").is_some()).collect()
}

struct Span {
    start: i64,
    end: i64,
    all_day: bool,
}

fn span(ev: &Comp, cal: &Comp, local: Tz, fallback: Option<i64>) -> Option<Span> {
    let ds = dt_of(ev.prop("DTSTART")?, Some(cal))?;
    let all_day = ds.is_date();
    let start = ds.key(local);
    let end = if let Some(de) = ev.prop("DTEND").and_then(|p| dt_of(p, Some(cal))) {
        de.key(local)
    } else if let Some(d) = ev.prop("DURATION").and_then(|p| duration(&p.value)) {
        start + d
    } else if let Some(f) = fallback {
        start + f
    } else if all_day {
        start + 86400
    } else {
        start
    };
    Some(Span { start, end: end.max(start), all_day })
}

#[derive(Clone, Debug)]
pub struct Info {
    pub uid: String,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: i64,
    pub end: i64,
    /// When the last instance ends (i64::MAX for an endless series).
    pub last: i64,
    pub all_day: bool,
    pub recurring: bool,
    pub alarm_min: Option<i32>,
}

fn is_recurring(cal: &Comp, m: &Comp) -> bool {
    m.prop("RRULE").is_some() || m.prop("RDATE").is_some() || !exceptions(cal).is_empty()
}

/// Minutes before the start of the first alarm relative to the start.
fn alarm_min(m: &Comp) -> Option<i32> {
    for a in m.comps.iter().filter(|c| c.name == "VALARM") {
        let Some(t) = a.prop("TRIGGER") else { continue };
        if t.param("RELATED").map(|r| r.eq_ignore_ascii_case("END")).unwrap_or(false) {
            continue;
        }
        if t.param("VALUE").map(|v| v.eq_ignore_ascii_case("DATE-TIME")).unwrap_or(false) {
            continue;
        }
        if let Some(d) = duration(&t.value) {
            return Some(if d < 0 { (-d / 60) as i32 } else { 0 });
        }
    }
    None
}

/// UNTIL as the rrule crate wants it: UTC when the start has a zone.
fn fix_until(rule: &str, start: &Dt, local: Tz) -> String {
    let mut parts: Vec<String> = Vec::new();
    for p in rule.split(';') {
        if let Some(v) = p.strip_prefix("UNTIL=").or_else(|| p.strip_prefix("until=")) {
            let fixed = match parse_value(v, false, Zone::Floating) {
                Some(Dt::Date(d)) => {
                    // a date: through the end of that day in the event's zone
                    let ndt = d.and_hms_opt(23, 59, 59).unwrap();
                    let inst = match start {
                        Dt::Time(_, Zone::Tz(t)) => resolve(t, &ndt).with_timezone(&Utc),
                        Dt::Time(_, Zone::Floating) => resolve(&local, &ndt).with_timezone(&Utc),
                        _ => ndt.and_utc(),
                    };
                    inst.format("%Y%m%dT%H%M%SZ").to_string()
                }
                Some(Dt::Time(ndt, Zone::Utc)) => ndt.format("%Y%m%dT%H%M%SZ").to_string(),
                Some(Dt::Time(ndt, _)) => {
                    let inst = match start {
                        Dt::Time(_, Zone::Tz(t)) => resolve(t, &ndt).with_timezone(&Utc),
                        _ => resolve(&local, &ndt).with_timezone(&Utc),
                    };
                    inst.format("%Y%m%dT%H%M%SZ").to_string()
                }
                None => v.to_string(),
            };
            parts.push(format!("UNTIL={fixed}"));
        } else if !p.is_empty() {
            parts.push(p.to_string());
        }
    }
    parts.join(";")
}

/// The start times (as rrule datetimes) of the series' generated instances in [lo, hi].
fn instances(cal: &Comp, m: &Comp, local: Tz, lo: Option<i64>, hi: Option<i64>) -> Option<(Vec<i64>, bool)> {
    let ds = dt_of(m.prop("DTSTART")?, Some(cal))?;
    let start = ds.rr(local);
    let mut set = rrule::RRuleSet::new(start);
    for r in m.all("RRULE") {
        let text = format!("RRULE:{}", fix_until(&r.value, &ds, local));
        let parsed: rrule::RRule<rrule::Unvalidated> = text.parse().ok()?;
        let v = parsed.validate(start).ok()?;
        set = set.rrule(v);
    }
    for p in m.all("RDATE") {
        for d in all_dts(p, Some(cal)) {
            set = set.rdate(d.rr(local));
        }
    }
    for p in m.all("EXDATE") {
        for d in all_dts(p, Some(cal)) {
            set = set.exdate(d.rr(local));
        }
    }
    let to_rr = |s: i64| DateTime::<Utc>::from_timestamp(s, 0).unwrap_or_default().with_timezone(&rrule::Tz::Tz(Tz::UTC));
    if let Some(l) = lo {
        set = set.after(to_rr(l));
    }
    if let Some(h) = hi {
        set = set.before(to_rr(h));
    }
    let r = set.all(MAX_INSTANCES);
    let keys = r
        .dates
        .iter()
        .map(|d| if ds.is_date() { day_key(d.date_naive()) } else { d.timestamp() })
        .collect();
    Some((keys, r.limited))
}

pub fn info(ics: &str, local: Tz) -> Option<Info> {
    let cal = parse(ics)?;
    let m = master(&cal)?;
    let s = span(m, &cal, local, None)?;
    let recurring = is_recurring(&cal, m);
    let last = if recurring { last_end(&cal, m, &s, local) } else { s.end };
    Some(Info {
        uid: m.prop("UID").map(|p| p.value.trim().to_string()).unwrap_or_default(),
        summary: m.text("SUMMARY").map(|s| s.trim().to_string()).unwrap_or_default(),
        location: m.text("LOCATION").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        description: m.text("DESCRIPTION").filter(|s| !s.trim().is_empty()),
        start: s.start,
        end: s.end,
        last,
        all_day: s.all_day,
        recurring,
        alarm_min: alarm_min(m),
    })
}

fn last_end(cal: &Comp, m: &Comp, s: &Span, local: Tz) -> i64 {
    let dur = s.end - s.start;
    let endless = m.all("RRULE").iter().any(|r| {
        let u = r.value.to_uppercase();
        !u.contains("UNTIL=") && !u.contains("COUNT=")
    });
    if endless {
        return i64::MAX;
    }
    let mut last = s.end;
    if m.prop("RRULE").is_some() || m.prop("RDATE").is_some() {
        match instances(cal, m, local, None, None) {
            Some((keys, limited)) => {
                if limited {
                    return i64::MAX;
                }
                for k in keys {
                    last = last.max(k + dur);
                }
            }
            None => return i64::MAX,
        }
    }
    for e in exceptions(cal) {
        if let Some(x) = span(e, cal, local, Some(dur)) {
            last = last.max(x.end);
        }
    }
    last
}

/// One instance of an event, as the views draw it.
#[derive(Clone, Debug)]
pub struct Occ {
    /// Instants; for all-day events the local midnights of the first day and of the day after the last.
    pub start: i64,
    pub end: i64,
    pub all_day: bool,
    pub summary: String,
    pub location: Option<String>,
    pub description: Option<String>,
    /// The instance's original start (day key when all-day), None for a lone event.
    pub instance: Option<i64>,
    pub exception: bool,
}

fn local_midnight(key: i64, local: Tz) -> i64 {
    resolve(&local, &date_of_key(key).and_hms_opt(0, 0, 0).unwrap()).timestamp()
}

fn overlaps(s: i64, e: i64, lo: i64, hi: i64) -> bool {
    s < hi && (e > lo || (e == s && s >= lo))
}

/// The instances of the event that touch [lo, hi) (instants).
pub fn expand(ics: &str, local: Tz, lo: i64, hi: i64) -> Vec<Occ> {
    let Some(cal) = parse(ics) else { return vec![] };
    let Some(m) = master(&cal) else { return vec![] };
    let Some(ms) = span(m, &cal, local, None) else { return vec![] };
    let all_day = ms.all_day;
    let dur = ms.end - ms.start;
    let exs = exceptions(&cal);
    let ex_keys: Vec<i64> = exs.iter().filter_map(|e| e.prop("RECURRENCE-ID").and_then(|p| dt_of(p, Some(&cal))).map(|d| d.key(local))).collect();
    let summary = m.text("SUMMARY").map(|s| s.trim().to_string()).unwrap_or_default();
    let location = m.text("LOCATION").filter(|s| !s.trim().is_empty());
    let desc = m.text("DESCRIPTION");
    let mut out = Vec::new();
    let to_view = |s: i64, e: i64| -> (i64, i64) {
        if all_day {
            (local_midnight(s, local), local_midnight(e, local))
        } else {
            (s, e)
        }
    };
    let has_ex = !exs.is_empty();
    if m.prop("RRULE").is_none() && m.prop("RDATE").is_none() {
        let (a, b) = to_view(ms.start, ms.end);
        if overlaps(a, b, lo, hi) && !ex_keys.contains(&ms.start) {
            out.push(Occ { start: a, end: b, all_day, summary: summary.clone(), location: location.clone(), description: desc.clone(), instance: if has_ex { Some(ms.start) } else { None }, exception: false });
        }
    } else {
        // widen the window by the length (and a day for all-day zone shifts)
        let pad = dur + 86400 * 2;
        match instances(&cal, m, local, Some(lo - pad), Some(hi + 86400)) {
            Some((keys, _)) => {
                for k in keys {
                    if ex_keys.contains(&k) {
                        continue;
                    }
                    let (a, b) = to_view(k, k + dur);
                    if overlaps(a, b, lo, hi) {
                        out.push(Occ { start: a, end: b, all_day, summary: summary.clone(), location: location.clone(), description: desc.clone(), instance: Some(k), exception: false });
                    }
                }
            }
            None => {
                // a rule we can't follow: at least the first instance
                let (a, b) = to_view(ms.start, ms.end);
                if overlaps(a, b, lo, hi) {
                    out.push(Occ { start: a, end: b, all_day, summary: summary.clone(), location: location.clone(), description: desc.clone(), instance: Some(ms.start), exception: false });
                }
            }
        }
    }
    for e in exs {
        if e.prop("STATUS").map(|s| s.value.trim().eq_ignore_ascii_case("CANCELLED")).unwrap_or(false) {
            continue;
        }
        let Some(es) = span(e, &cal, local, Some(dur)) else { continue };
        if es.all_day != all_day {
            continue;
        }
        let (a, b) = to_view(es.start, es.end);
        if overlaps(a, b, lo, hi) {
            out.push(Occ {
                start: a,
                end: b,
                all_day,
                summary: e.text("SUMMARY").map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).unwrap_or_else(|| summary.clone()),
                location: e.text("LOCATION").filter(|s| !s.trim().is_empty()),
                description: e.text("DESCRIPTION").or_else(|| desc.clone()),
                instance: e.prop("RECURRENCE-ID").and_then(|p| dt_of(p, Some(&cal))).map(|d| d.key(local)),
                exception: true,
            });
        }
    }
    out
}

// ---- writing ----

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repeat {
    None,
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    pub const ALL: [Repeat; 5] = [Repeat::None, Repeat::Daily, Repeat::Weekly, Repeat::Monthly, Repeat::Yearly];
    pub fn label(&self) -> &'static str {
        match self {
            Repeat::None => "no",
            Repeat::Daily => "daily",
            Repeat::Weekly => "weekly",
            Repeat::Monthly => "monthly",
            Repeat::Yearly => "yearly",
        }
    }
    fn freq(&self) -> Option<&'static str> {
        match self {
            Repeat::None => None,
            Repeat::Daily => Some("DAILY"),
            Repeat::Weekly => Some("WEEKLY"),
            Repeat::Monthly => Some("MONTHLY"),
            Repeat::Yearly => Some("YEARLY"),
        }
    }
    fn of(rule: Option<&str>) -> Repeat {
        let f = rule.and_then(|r| r.split(';').find_map(|p| p.strip_prefix("FREQ="))).unwrap_or("");
        match f.to_uppercase().as_str() {
            "DAILY" => Repeat::Daily,
            "WEEKLY" => Repeat::Weekly,
            "MONTHLY" => Repeat::Monthly,
            "YEARLY" => Repeat::Yearly,
            _ => Repeat::None,
        }
    }
}

/// What the editor works on. Times are local wall-clock times.
#[derive(Clone, Debug)]
pub struct Draft {
    pub title: String,
    pub location: String,
    pub notes: String,
    pub all_day: bool,
    pub start: NaiveDateTime,
    /// For all-day events: the last day (inclusive).
    pub end: NaiveDateTime,
    pub repeat: Repeat,
    pub reminder_min: Option<i32>,
    /// The rule is more than the plain frequency (BYDAY, INTERVAL…): kept unless the frequency changes.
    pub rule_text: Option<String>,
}

fn local_dt(secs: i64, local: Tz) -> NaiveDateTime {
    DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_default().with_timezone(&local).naive_local()
}

/// The series' own values for the editor.
pub fn draft_of(ics: &str, local: Tz) -> Option<Draft> {
    let i = info(ics, local)?;
    let cal = parse(ics)?;
    let m = master(&cal)?;
    let (start, end) = if i.all_day {
        let s = date_of_key(i.start);
        let e = date_of_key(i.end).pred_opt().unwrap_or(s).max(s);
        (s.and_hms_opt(0, 0, 0).unwrap(), e.and_hms_opt(0, 0, 0).unwrap())
    } else {
        (local_dt(i.start, local), local_dt(i.end, local))
    };
    let rule = m.prop("RRULE").map(|p| p.value.clone());
    Some(Draft {
        title: i.summary,
        location: i.location.unwrap_or_default(),
        notes: i.description.unwrap_or_default(),
        all_day: i.all_day,
        start,
        end,
        repeat: Repeat::of(rule.as_deref()),
        reminder_min: i.alarm_min,
        rule_text: rule,
    })
}

fn stamp() -> String {
    Utc::now().format("%Y%m%dT%H%M%SZ").to_string()
}

fn uid() -> String {
    use rand::Rng;
    let r: u128 = rand::thread_rng().gen();
    format!("{r:032x}@maildot")
}

/// A timed value in the zone the event already uses (an Olson TZID), else the local zone.
fn time_prop(name: &str, instant: i64, zone: Zone, local: Tz) -> Prop {
    let utc = DateTime::<Utc>::from_timestamp(instant, 0).unwrap_or_default();
    match zone {
        Zone::Utc => Prop::new(name, &utc.format("%Y%m%dT%H%M%SZ").to_string()),
        Zone::Tz(t) => Prop::new(name, &utc.with_timezone(&t).format("%Y%m%dT%H%M%S").to_string()).with("TZID", t.name()),
        Zone::Floating => Prop::new(name, &utc.with_timezone(&local).format("%Y%m%dT%H%M%S").to_string()).with("TZID", local.name()),
    }
}

fn start_zone(cal: &Comp, m: &Comp) -> Zone {
    match m.prop("DTSTART").and_then(|p| dt_of(p, Some(cal))) {
        Some(Dt::Time(_, z @ Zone::Tz(_))) => z,
        Some(Dt::Time(_, Zone::Utc)) => Zone::Utc,
        _ => Zone::Floating,
    }
}

/// A new event's ics, or [existing] with the draft applied to the series.
pub fn write_event(d: &Draft, local: Tz, existing: Option<&str>) -> String {
    let mut cal = existing.and_then(parse).unwrap_or_else(|| {
        let mut c = Comp::new("VCALENDAR");
        c.props.push(Prop::new("VERSION", "2.0"));
        c.props.push(Prop::new("PRODID", PRODID));
        c.props.push(Prop::new("CALSCALE", "GREGORIAN"));
        c
    });
    if master(&cal).is_none() {
        let mut ev = Comp::new("VEVENT");
        ev.props.push(Prop::new("UID", &uid()));
        ev.props.push(Prop::new("DTSTAMP", &stamp()));
        cal.comps.push(ev);
    }
    let zone = start_zone(&cal, master(&cal).unwrap());
    let is_new = existing.is_none();
    let m = master_mut(&mut cal).unwrap();
    m.set_text("SUMMARY", Some(&d.title));
    m.set_text("LOCATION", Some(&d.location));
    m.set_text("DESCRIPTION", Some(&d.notes));
    m.remove("DURATION");
    if d.all_day {
        let s = d.start.date();
        let e = d.end.date().max(s).succ_opt().unwrap_or(s);
        m.set(Prop::new("DTSTART", &s.format("%Y%m%d").to_string()).with("VALUE", "DATE"));
        m.set(Prop::new("DTEND", &e.format("%Y%m%d").to_string()).with("VALUE", "DATE"));
    } else {
        let s = resolve(&local, &d.start).timestamp();
        let e = resolve(&local, &d.end).timestamp().max(s);
        m.set(time_prop("DTSTART", s, zone, local));
        m.set(time_prop("DTEND", e, zone, local));
    }
    // a rule with the same frequency stays as it is (it may carry BYDAY and the like)
    let old = m.prop("RRULE").map(|p| p.value.clone());
    match d.repeat.freq() {
        None => m.remove("RRULE"),
        Some(f) => {
            if Repeat::of(old.as_deref()) != d.repeat {
                m.set(Prop::new("RRULE", &format!("FREQ={f}")));
            }
        }
    }
    // the reminder here replaces whatever alarms the event had
    m.comps.retain(|c| c.name != "VALARM");
    if let Some(min) = d.reminder_min {
        let mut a = Comp::new("VALARM");
        a.props.push(Prop::new("ACTION", "DISPLAY"));
        a.props.push(Prop::new("DESCRIPTION", &escape(if d.title.trim().is_empty() { "reminder" } else { d.title.trim() })));
        a.props.push(Prop::new("TRIGGER", &if min == 0 { "PT0M".to_string() } else { format!("-PT{min}M") }));
        m.comps.push(a);
    }
    m.set(Prop::new("DTSTAMP", &stamp()));
    m.set(Prop::new("LAST-MODIFIED", &stamp()));
    if !is_new {
        let seq: i64 = m.prop("SEQUENCE").and_then(|p| p.value.trim().parse().ok()).unwrap_or(0);
        m.set(Prop::new("SEQUENCE", &(seq + 1).to_string()));
    }
    write(&cal)
}

/// The series without the instance that started at [instance] (an EXDATE; an exception
/// VEVENT for it goes too).
pub fn exclude(ics: &str, instance: i64, all_day: bool, local: Tz) -> Option<String> {
    let mut cal = parse(ics)?;
    let zone = start_zone(&cal, master(&cal)?);
    let cal_copy = cal.clone();
    cal.comps.retain(|c| {
        !(c.name == "VEVENT"
            && c.prop("RECURRENCE-ID").and_then(|p| dt_of(p, Some(&cal_copy))).map(|d| d.key(local) == instance).unwrap_or(false))
    });
    let m = master_mut(&mut cal)?;
    let p = if all_day {
        Prop::new("EXDATE", &date_of_key(instance).format("%Y%m%d").to_string()).with("VALUE", "DATE")
    } else {
        time_prop("EXDATE", instance, zone, local)
    };
    // a new EXDATE line of its own: other clients' lists stay as they were
    let at = m.props.iter().rposition(|x| x.name == "EXDATE" || x.name == "RRULE").map(|i| i + 1).unwrap_or(m.props.len());
    m.props.insert(at, p);
    m.set(Prop::new("DTSTAMP", &stamp()));
    m.set(Prop::new("LAST-MODIFIED", &stamp()));
    let seq: i64 = m.prop("SEQUENCE").and_then(|p| p.value.trim().parse().ok()).unwrap_or(0);
    m.set(Prop::new("SEQUENCE", &(seq + 1).to_string()));
    Some(write(&cal))
}

pub fn hm(secs: i64, local: Tz) -> String {
    let t = DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_default().with_timezone(&local);
    format!("{:02}:{:02}", t.hour(), t.minute())
}

pub fn local_date(secs: i64, local: Tz) -> NaiveDate {
    DateTime::<Utc>::from_timestamp(secs, 0).unwrap_or_default().with_timezone(&local).date_naive()
}

/// Local midnight of [d] as an instant.
pub fn midnight(d: NaiveDate, local: Tz) -> i64 {
    resolve(&local, &d.and_hms_opt(0, 0, 0).unwrap()).timestamp()
}

pub fn weekday_index(d: NaiveDate, week_start_monday: bool) -> u32 {
    if week_start_monday {
        d.weekday().num_days_from_monday()
    } else {
        d.weekday().num_days_from_sunday()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prague() -> Tz {
        Tz::Europe__Prague
    }

    const WEEKLY: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//x//y//EN\r\nX-WR-CALNAME:keep me\r\nBEGIN:VEVENT\r\nUID:w1\r\nDTSTART;TZID=Europe/Prague:20261005T090000\r\nDTEND;TZID=Europe/Prague:20261005T100000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE\r\nEXDATE;TZID=Europe/Prague:20261007T090000\r\nSUMMARY:Standup\\, daily-ish\r\nX-CUSTOM;FOO=bar:stays\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:w1\r\nRECURRENCE-ID;TZID=Europe/Prague:20261012T090000\r\nDTSTART;TZID=Europe/Prague:20261012T113000\r\nDTEND;TZID=Europe/Prague:20261012T123000\r\nSUMMARY:Standup moved\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn info_and_expand_weekly() {
        let tz = prague();
        let i = info(WEEKLY, tz).unwrap();
        assert_eq!(i.summary, "Standup, daily-ish");
        assert!(i.recurring);
        assert_eq!(i.last, i64::MAX);
        assert_eq!(i.alarm_min, Some(15));
        let lo = midnight(NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(), tz);
        let hi = midnight(NaiveDate::from_ymd_opt(2026, 10, 19).unwrap(), tz);
        let occ = expand(WEEKLY, tz, lo, hi);
        let mut got: Vec<(String, String)> = occ.iter().map(|o| (local_date(o.start, tz).to_string(), hm(o.start, tz))).collect();
        got.sort();
        assert_eq!(
            got,
            vec![
                ("2026-10-05".into(), "09:00".into()),
                ("2026-10-12".into(), "11:30".into()),
                ("2026-10-14".into(), "09:00".into()),
            ]
        );
    }

    #[test]
    fn dst_change_keeps_wall_time() {
        let tz = prague();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTART;TZID=Europe/Prague:20261020T080000\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=20\r\nSUMMARY:x\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let lo = midnight(NaiveDate::from_ymd_opt(2026, 10, 24).unwrap(), tz);
        let hi = midnight(NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(), tz);
        let occ = expand(ics, tz, lo, hi);
        assert_eq!(occ.len(), 3);
        assert!(occ.iter().all(|o| hm(o.start, tz) == "08:00"));
        let i = info(ics, tz).unwrap();
        assert!(i.last < i64::MAX);
    }

    #[test]
    fn all_day_and_until_date() {
        let tz = prague();
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261001\r\nDTEND;VALUE=DATE:20261003\r\nRRULE:FREQ=MONTHLY;UNTIL=20261231\r\nSUMMARY:two days\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let i = info(ics, tz).unwrap();
        assert!(i.all_day);
        assert_eq!(date_of_key(i.last).to_string(), "2026-12-03");
        let lo = midnight(NaiveDate::from_ymd_opt(2026, 11, 1).unwrap(), tz);
        let hi = midnight(NaiveDate::from_ymd_opt(2026, 11, 2).unwrap(), tz);
        let occ = expand(ics, tz, lo, hi);
        assert_eq!(occ.len(), 1);
        assert_eq!(local_date(occ[0].start, tz).to_string(), "2026-11-01");
        assert_eq!(local_date(occ[0].end, tz).to_string(), "2026-11-03");
    }

    #[test]
    fn write_keeps_unknown_and_rule() {
        let tz = prague();
        let mut d = draft_of(WEEKLY, tz).unwrap();
        assert_eq!(d.repeat, Repeat::Weekly);
        d.title = "Standup; new".into();
        let out = write_event(&d, tz, Some(WEEKLY));
        assert!(out.contains("X-CUSTOM;FOO=bar:stays"));
        assert!(out.contains("X-WR-CALNAME:keep me"));
        assert!(out.contains("RRULE:FREQ=WEEKLY;BYDAY=MO,WE"));
        assert!(out.contains("SUMMARY:Standup\\; new"));
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Prague:20261012T090000"));
        assert!(out.contains("SEQUENCE:1"));
        assert!(out.contains("TRIGGER:-PT15M"));
        let back = info(&out, tz).unwrap();
        assert_eq!(hm(back.start, tz), "09:00");
    }

    #[test]
    fn exclude_instance() {
        let tz = prague();
        let k = Dt::Time(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap().and_hms_opt(9, 0, 0).unwrap(), Zone::Tz(tz)).key(tz);
        let out = exclude(WEEKLY, k, false, tz).unwrap();
        assert!(!out.contains("RECURRENCE-ID"));
        assert!(out.contains("EXDATE;TZID=Europe/Prague:20261012T090000"));
        let lo = midnight(NaiveDate::from_ymd_opt(2026, 10, 12).unwrap(), tz);
        assert!(expand(&out, tz, lo, lo + 86400).is_empty());
    }

    #[test]
    fn new_event_round_trip() {
        let tz = prague();
        let d = Draft {
            title: "Dentist".into(),
            location: "Brno".into(),
            notes: "line1\nline2".into(),
            all_day: false,
            start: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap().and_hms_opt(14, 0, 0).unwrap(),
            end: NaiveDate::from_ymd_opt(2026, 12, 1).unwrap().and_hms_opt(15, 0, 0).unwrap(),
            repeat: Repeat::None,
            reminder_min: Some(60),
            rule_text: None,
        };
        let out = write_event(&d, tz, None);
        assert!(out.contains("DTSTART;TZID=Europe/Prague:20261201T140000"));
        let i = info(&out, tz).unwrap();
        assert_eq!(i.description.as_deref(), Some("line1\nline2"));
        assert_eq!(i.alarm_min, Some(60));
        assert!(!i.recurring);
    }

    #[test]
    fn durations() {
        assert_eq!(duration("-PT15M"), Some(-900));
        assert_eq!(duration("P1DT2H"), Some(93600));
        assert_eq!(duration("P2W"), Some(14 * 86400));
    }
}
