//! Local search over every stored message, on all cores.
//!
//! The text table is cut into id ranges; rayon hands the ranges to its workers, each with its
//! own read-only SQLite connection (WAL: nobody waits for the sync), and every row is tested
//! against all conditions with the `regex` crate (linear time, so no pattern can hang it).
//! Plain text is a regex too (escaped), which gives case folding and literal prefilters
//! for free. Hits stream out per range; the caller sorts them.

use anyhow::{bail, Result};
use rayon::prelude::*;
use regex::{Regex, RegexBuilder};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    Subject,
    Sender,
    Rcpts,
    Headers,
    Body,
    Attach,
}

impl Field {
    pub const ALL: [Field; 6] = [Field::Subject, Field::Sender, Field::Rcpts, Field::Headers, Field::Body, Field::Attach];

    pub fn label(&self) -> &'static str {
        match self {
            Field::Subject => "subject",
            Field::Sender => "from",
            Field::Rcpts => "to/cc",
            Field::Headers => "headers",
            Field::Body => "body",
            Field::Attach => "attachments",
        }
    }

    fn col(&self) -> &'static str {
        match self {
            Field::Subject => "t.subject",
            Field::Sender => "t.sender",
            Field::Rcpts => "t.rcpts",
            Field::Headers => "t.headers",
            Field::Body => "t.body",
            Field::Attach => "t.attach",
        }
    }
}

/// One condition: a pattern and where to look for it.
#[derive(Clone, Debug)]
pub struct Cond {
    pub pattern: String,
    pub regex: bool,
    pub case: bool,
    pub word: bool,
    pub fields: Vec<Field>,
    /// The message must NOT match.
    pub negate: bool,
}

impl Cond {
    pub fn text(p: &str) -> Cond {
        Cond { pattern: p.to_string(), regex: false, case: false, word: false, fields: vec![Field::Subject, Field::Sender, Field::Rcpts, Field::Body, Field::Attach], negate: false }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    /// All must hold.
    pub conds: Vec<Cond>,
    /// Empty: every account.
    pub accounts: Vec<i64>,
    /// One folder of one account.
    pub mailbox: Option<(i64, String)>,
    /// Received at or after (seconds).
    pub since: Option<i64>,
    /// Received before (seconds).
    pub before: Option<i64>,
    pub unread: bool,
    pub flagged: bool,
    pub attachments: bool,
    /// At most this many hits are reported (all are counted).
    pub limit: usize,
}

#[derive(Clone, Debug)]
pub struct Hit {
    pub id: i64,
    pub received: i64,
    pub field: Option<Field>,
    /// A line around the first match.
    pub snippet: String,
    /// Byte ranges in [snippet] to highlight.
    pub ranges: Vec<(usize, usize)>,
    /// Matches of the first condition in this message.
    pub count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct Stats {
    pub scanned: usize,
    pub matched: usize,
    pub millis: u128,
    pub threads: usize,
    pub bytes: u64,
}

pub fn compile(c: &Cond) -> Result<Regex> {
    if c.pattern.is_empty() {
        bail!("empty pattern");
    }
    let mut pat = if c.regex { c.pattern.clone() } else { regex::escape(&c.pattern) };
    if c.word {
        pat = format!(r"\b(?:{pat})\b");
    }
    let r = RegexBuilder::new(&pat).case_insensitive(!c.case).multi_line(true).size_limit(64 << 20).dfa_size_limit(64 << 20).build();
    match r {
        Ok(r) => Ok(r),
        Err(regex::Error::Syntax(s)) => bail!("{}", s.lines().last().unwrap_or("bad regex").trim_start_matches("error: ")),
        Err(e) => bail!("{e}"),
    }
}

fn open_ro(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
    c.busy_timeout(std::time::Duration::from_secs(10))?;
    c.execute_batch("PRAGMA mmap_size=1073741824; PRAGMA cache_size=-32768; PRAGMA query_only=1;")?;
    Ok(c)
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// A one-line excerpt around [m] in [text], with every match of [re] in it marked.
pub fn snippet(text: &str, re: &Regex, m: (usize, usize)) -> (String, Vec<(usize, usize)>) {
    let start = floor_char(text, m.0.saturating_sub(60));
    // start at a word if we can
    let start = match text[start..m.0].find(char::is_whitespace) {
        Some(i) if start > 0 && i < 20 => start + i + 1,
        _ => start,
    };
    let end = ceil_char(text, (m.1 + 140).min(text.len()));
    let mut s = String::with_capacity(end - start + 6);
    let lead = if start > 0 { "…" } else { "" };
    s.push_str(lead);
    for c in text[start..end].chars() {
        s.push(if c == '\n' || c == '\r' || c == '\t' { ' ' } else { c });
    }
    if end < text.len() {
        s.push('…');
    }
    let off = lead.len();
    let mut ranges = Vec::new();
    for x in re.find_iter(&text[start..end]) {
        if x.end() > x.start() {
            ranges.push((x.start() + off, x.end() + off));
        }
        if ranges.len() > 50 {
            break;
        }
    }
    (s, ranges)
}

/// Byte ranges of every match of the (non-negated) conditions in [text], for the reader.
pub fn ranges_in(text: &str, conds: &[Cond]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for c in conds.iter().filter(|c| !c.negate) {
        if let Ok(re) = compile(c) {
            for m in re.find_iter(text) {
                if m.end() > m.start() {
                    out.push((m.start(), m.end()));
                }
                if out.len() > 5000 {
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

/// Runs the query; [on_hits] gets the hits of each finished range (from worker threads).
pub fn search(db: &Path, q: &Query, cancel: &AtomicBool, on_hits: &(dyn Fn(Vec<Hit>) + Sync)) -> Result<Stats> {
    let t0 = Instant::now();
    let conds: Vec<(Cond, Regex)> = q.conds.iter().filter(|c| !c.pattern.is_empty()).map(|c| Ok((c.clone(), compile(c)?))).collect::<Result<_>>()?;
    // the columns any condition reads, in a fixed order
    let mut fields: Vec<Field> = Vec::new();
    for f in Field::ALL {
        if conds.iter().any(|(c, _)| c.fields.contains(&f)) {
            fields.push(f);
        }
    }
    if !fields.contains(&Field::Subject) {
        fields.insert(0, Field::Subject);
    }
    let cols: Vec<&str> = fields.iter().map(|f| f.col()).collect();
    let mut wh = vec!["t.id BETWEEN ?1 AND ?2".to_string()];
    if !q.accounts.is_empty() {
        wh.push(format!("m.account IN ({})", q.accounts.iter().map(|a| a.to_string()).collect::<Vec<_>>().join(",")));
    }
    if let Some((a, mb)) = &q.mailbox {
        wh.push(format!("EXISTS (SELECT 1 FROM msg_mailbox b WHERE b.msg = t.id AND b.account = {a} AND b.mailbox = '{}')", mb.replace('\'', "''")));
    }
    if let Some(s) = q.since {
        wh.push(format!("m.received >= {s}"));
    }
    if let Some(b) = q.before {
        wh.push(format!("m.received < {b}"));
    }
    if q.unread {
        wh.push("(' ' || m.keywords || ' ') NOT LIKE '% $seen %'".into());
    }
    if q.flagged {
        wh.push("(' ' || m.keywords || ' ') LIKE '% $flagged %'".into());
    }
    if q.attachments {
        wh.push("m.has_att = 1".into());
    }
    let sql = format!("SELECT t.id, m.received, {} FROM texts t JOIN messages m ON m.id = t.id WHERE {}", cols.join(", "), wh.join(" AND "));

    let first = open_ro(db)?;
    let (lo, hi): (Option<i64>, Option<i64>) = first.query_row("SELECT MIN(id), MAX(id) FROM texts", [], |r| Ok((r.get(0)?, r.get(1)?)))?;
    drop(first);
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Ok(Stats { threads: rayon::current_num_threads(), millis: t0.elapsed().as_millis(), ..Default::default() });
    };
    const CHUNK: i64 = 512;
    let ranges: Vec<(i64, i64)> = (lo..=hi).step_by(CHUNK as usize).map(|a| (a, (a + CHUNK - 1).min(hi))).collect();
    let scanned = AtomicUsize::new(0);
    let matched = AtomicUsize::new(0);
    let bytes = AtomicUsize::new(0);
    let limit = if q.limit == 0 { usize::MAX } else { q.limit };
    let err: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    ranges.par_iter().for_each_init(
        || open_ro(db).ok(),
        |conn, &(a, b)| {
            if cancel.load(Ordering::Relaxed) {
                return;
            }
            let Some(conn) = conn.as_ref() else { return };
            let run = || -> rusqlite::Result<Vec<Hit>> {
                let mut st = conn.prepare_cached(&sql)?;
                let mut rows = st.query([a, b])?;
                let mut hits = Vec::new();
                let mut n = 0usize;
                let mut nb = 0usize;
                while let Some(row) = rows.next()? {
                    n += 1;
                    if n % 64 == 0 && cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let id: i64 = row.get(0)?;
                    let received: i64 = row.get(1)?;
                    let texts: Vec<&str> = (0..fields.len()).map(|i| row.get_ref(i + 2).ok().and_then(|v| v.as_str().ok()).unwrap_or("")).collect();
                    nb += texts.iter().map(|t| t.len()).sum::<usize>();
                    let text_of = |f: Field| -> &str {
                        let i = fields.iter().position(|x| *x == f).unwrap();
                        texts[i]
                    };
                    let mut ok = true;
                    let mut first_hit: Option<(Field, usize, usize, usize)> = None;
                    for (ci, (c, re)) in conds.iter().enumerate() {
                        let mut found = None;
                        for f in &c.fields {
                            if let Some(m) = re.find(text_of(*f)) {
                                found = Some((*f, m.start(), m.end()));
                                break;
                            }
                        }
                        if c.negate {
                            if found.is_some() {
                                ok = false;
                                break;
                            }
                        } else {
                            match found {
                                None => {
                                    ok = false;
                                    break;
                                }
                                Some((f, s, e)) => {
                                    if first_hit.is_none() {
                                        first_hit = Some((f, s, e, ci));
                                    }
                                }
                            }
                        }
                    }
                    if !ok {
                        continue;
                    }
                    let k = matched.fetch_add(1, Ordering::Relaxed);
                    if k >= limit {
                        continue;
                    }
                    let hit = match first_hit {
                        Some((f, s, e, ci)) => {
                            let re = &conds[ci].1;
                            let (snip, ranges) = snippet(text_of(f), re, (s, e));
                            let count = conds[ci].0.fields.iter().map(|f| re.find_iter(text_of(*f)).take(10_000).count()).sum();
                            Hit { id, received, field: Some(f), snippet: snip, ranges, count }
                        }
                        None => Hit { id, received, field: None, snippet: text_of(Field::Subject).to_string(), ranges: vec![], count: 0 },
                    };
                    hits.push(hit);
                }
                scanned.fetch_add(n, Ordering::Relaxed);
                bytes.fetch_add(nb, Ordering::Relaxed);
                Ok(hits)
            };
            match run() {
                Ok(h) if !h.is_empty() => on_hits(h),
                Ok(_) => {}
                Err(e) => {
                    *err.lock().unwrap() = Some(e.to_string());
                }
            }
        },
    );
    if let Some(e) = err.into_inner().unwrap() {
        bail!("search failed: {e}");
    }
    Ok(Stats {
        scanned: scanned.into_inner(),
        matched: matched.into_inner(),
        millis: t0.elapsed().as_millis(),
        threads: rayon::current_num_threads(),
        bytes: bytes.into_inner() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_is_literal_and_folds_case() {
        let c = Cond { pattern: "a.b(".into(), regex: false, case: false, word: false, fields: vec![Field::Body], negate: false };
        let re = compile(&c).unwrap();
        assert!(re.is_match("xx A.B( yy"));
        assert!(!re.is_match("axb("));
    }

    #[test]
    fn case_word_regex() {
        let mut c = Cond { pattern: "Faktura".into(), regex: false, case: true, word: true, fields: vec![Field::Subject], negate: false };
        assert!(!compile(&c).unwrap().is_match("faktura 12"));
        assert!(!compile(&c).unwrap().is_match("Fakturace"));
        assert!(compile(&c).unwrap().is_match("Nová Faktura 12"));
        c.regex = true;
        c.word = false;
        c.pattern = r"inv-\d{4}".into();
        assert!(compile(&c).unwrap().is_match("ref inv-2026"));
        c.pattern = "(".into();
        assert!(compile(&c).is_err());
    }

    #[test]
    fn czech_case_folding() {
        let c = Cond::text("ŽLUŤOUČKÝ");
        assert!(compile(&c).unwrap().is_match("příliš žluťoučký kůň"));
    }

    #[test]
    fn snippets_mark_matches() {
        let text = "line one\nsecond line with the needle and another needle\nthird";
        let re = compile(&Cond::text("needle")).unwrap();
        let m = re.find(text).unwrap();
        let (s, r) = snippet(text, &re, (m.start(), m.end()));
        assert!(!s.contains('\n'));
        assert_eq!(r.len(), 2);
        for (a, b) in r {
            assert_eq!(&s[a..b], "needle");
        }
    }
}
