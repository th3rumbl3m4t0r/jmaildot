//! End-to-end check against a local Stalwart test server (HTTP on 127.0.0.1:8090, SMTP 2525;
//! alice/alicepw with encryption at rest to the test key and a catch-all @example.org,
//! bob/bobpw). Run: MAILDOT_HOME=/tmp/x cargo run --example live
//! Everything is asserted; the first failure stops with a message.

use maildot_core::caldav::Ace;
use maildot_core::engine::{Engine, Outgoing};
use maildot_core::ical::{self, Draft, Repeat};
use maildot_core::jmap::Jmap;
use maildot_core::mime::{self, Headers};
use maildot_core::model::{Addr, Event};
use maildot_core::search::{self, Cond, Field, Query};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SERVER: &str = "http://127.0.0.1:8090";
const KEYS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data");

fn smtp(from: &str, to: &str, msg: &[u8]) {
    let s = TcpStream::connect("127.0.0.1:2525").expect("smtp");
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    let mut line = String::new();
    let mut expect = |r: &mut BufReader<TcpStream>, code: &str| loop {
        line.clear();
        r.read_line(&mut line).unwrap();
        assert!(line.starts_with(code), "smtp: wanted {code}, got {line}");
        if line.as_bytes().get(3) != Some(&b'-') {
            break;
        }
    };
    expect(&mut r, "220");
    w.write_all(b"EHLO test\r\n").unwrap();
    expect(&mut r, "250");
    w.write_all(format!("MAIL FROM:<{from}>\r\n").as_bytes()).unwrap();
    expect(&mut r, "250");
    w.write_all(format!("RCPT TO:<{to}>\r\n").as_bytes()).unwrap();
    expect(&mut r, "250");
    w.write_all(b"DATA\r\n").unwrap();
    expect(&mut r, "354");
    let body = mime::crlf(msg);
    let mut out = Vec::new();
    for l in body.split_inclusive(|b| *b == b'\n') {
        if l.starts_with(b".") {
            out.push(b'.');
        }
        out.extend_from_slice(l);
    }
    if !out.ends_with(b"\r\n") {
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b".\r\n");
    w.write_all(&out).unwrap();
    expect(&mut r, "250");
    w.write_all(b"QUIT\r\n").unwrap();
}

fn plain(from: &str, to: &str, subject: &str, body: &str) -> Vec<u8> {
    format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nDate: {}\r\nMessage-ID: <{}>\r\nMIME-Version: 1.0\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: 8bit\r\n\r\n{body}\r\n",
        chrono::Local::now().to_rfc2822(),
        mime::message_id("test.local")
    )
    .into_bytes()
}

fn wait<F: FnMut() -> bool>(what: &str, secs: u64, mut f: F) -> Duration {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
    let d = t.elapsed();
    println!("  ok  {what} ({} ms)", d.as_millis());
    d
}

fn find(e: &Engine, account: i64, needle: &str) -> Option<i64> {
    let q = Query { conds: vec![Cond::text(needle)], accounts: vec![account], limit: 10, ..Default::default() };
    let hits = Mutex::new(Vec::new());
    search::search(e.db.path(), &q, &AtomicBool::new(false), &|h| hits.lock().unwrap().extend(h)).unwrap();
    let h = hits.into_inner().unwrap();
    h.first().map(|x| x.id)
}

fn count(e: &Engine, q: Query) -> usize {
    let hits = Mutex::new(0usize);
    search::search(e.db.path(), &q, &AtomicBool::new(false), &|h| *hits.lock().unwrap() += h.len()).unwrap();
    hits.into_inner().unwrap()
}

fn main() {
    std::env::set_var("MAILDOT_NO_KEYRING", "1");
    let home = std::env::var("MAILDOT_HOME").expect("set MAILDOT_HOME to a scratch folder");
    let _ = std::fs::remove_dir_all(&home);
    let (e, rx) = Engine::new().unwrap();
    let events: Arc<Mutex<Vec<Event>>> = Arc::new(Mutex::new(Vec::new()));
    {
        let ev = events.clone();
        std::thread::spawn(move || {
            while let Ok(x) = rx.recv_blocking() {
                ev.lock().unwrap().push(x);
            }
        });
    }
    let tag = format!("zebra-{}", rand::random::<u32>());
    println!("run {tag}");

    // ---- keys ----
    e.import_keys(&std::fs::read(format!("{KEYS}/alice.sec.asc")).unwrap()).unwrap();
    e.import_keys(&std::fs::read(format!("{KEYS}/bob.sec.asc")).unwrap()).unwrap();
    assert!(e.locked());
    assert!(!e.unlock("wrong", false));
    assert!(e.unlock("alicekey", false));
    println!("  ok  keys imported, alice unlocked");

    // ---- accounts ----
    let alice = e.rt.block_on(e.add_account(SERVER, "alice", "alicepw")).unwrap();
    let bob = e.rt.block_on(e.add_account(SERVER, "bob", "bobpw")).unwrap();
    assert!(e.rt.block_on(e.add_account(SERVER, "alice", "nope")).is_err());
    e.start_all();
    let synced = |id: i64| {
        let e = e.clone();
        move || {
            let (n, s) = e.db.counts(id);
            n > 0 && n == s && e.db.email_state(id).is_some() && !e.syncing()
        }
    };
    wait("alice: every message listed and stored", 60, synced(alice));
    wait("bob: every message listed and stored", 60, synced(bob));
    let inbox = e.db.mailbox_by_role(alice, "inbox").unwrap();
    println!("      alice: {} folders, inbox {} messages", e.db.mailboxes(alice).len(), e.db.list(alice, &inbox.id, 1000).len());

    // ---- new mail by push; encryption at rest on the server, plain text here ----
    smtp("carol@outside.test", "alice@example.com", &plain("Carol <carol@outside.test>", "alice@example.com", &format!("Hello {tag}"), &format!("Body with the token {tag} and ŽLUŤOUČKÝ kůň.\nSecond line: invoice INV-2026-0042.")));
    let d = wait("plain mail arrives (push or the 30 s poll)", 45, || find(&e, alice, &tag).is_some());
    assert!(d < Duration::from_secs(40));
    let id = find(&e, alice, &tag).unwrap();
    let row = e.db.msg(id).unwrap();
    let file = std::fs::read_to_string(maildot_core::paths::eml_path(alice, id)).unwrap();
    assert!(file.contains("ŽLUŤOUČKÝ") || file.contains("=C5=BD") || file.contains(&tag), "stored file is plain text");
    assert!(!file.contains("BEGIN PGP MESSAGE"), "stored decrypted");
    println!("      stored: encrypted-on-server={} fetched={}", row.encrypted, row.fetched);
    let in_inbox = row.mailboxes.contains(&inbox.id);
    let notified = |i: i64| events.lock().unwrap().iter().any(|x| matches!(x, Event::NewMail(a, ids) if *a == alice && ids.contains(&i)));
    assert_eq!(notified(id), in_inbox, "notify only for the inbox (it went to {:?})", row.mailboxes);
    // into the inbox (the test server files all SMTP mail as junk): imported over JMAP
    {
        let j = Jmap::new(SERVER, "alice", "alicepw").unwrap();
        let blob = e.rt.block_on(j.upload(plain("Bob <bob@example.com>", "alice@example.com", &format!("Local {tag}"), "from bob"), "message/rfc822")).unwrap();
        e.rt.block_on(j.import(&blob, &inbox.id, &[])).unwrap();
    }
    wait("local mail arrives", 45, || find(&e, alice, &format!("Local {tag}")).is_some());
    let lid = find(&e, alice, &format!("Local {tag}")).unwrap();
    let lrow = e.db.msg(lid).unwrap();
    println!("      outside mail went to {:?}, local mail to {:?}", row.mailboxes, lrow.mailboxes);
    assert_eq!(notified(lid), lrow.mailboxes.contains(&inbox.id));
    println!("  ok  new-mail event for the inbox");

    // ---- search ----
    let base = |c: Cond| Query { conds: vec![Cond::text(&tag), c], accounts: vec![alice], limit: 100, ..Default::default() };
    assert_eq!(count(&e, base(Cond::text("žluťoučký KŮŇ"))), 1);
    assert_eq!(count(&e, base(Cond { case: true, ..Cond::text("žluťoučký") })), 0);
    assert_eq!(count(&e, base(Cond { regex: true, ..Cond::text(r"INV-\d{4}-\d{4}") })), 1);
    assert_eq!(count(&e, base(Cond { fields: vec![Field::Subject], ..Cond::text("invoice") })), 0);
    assert_eq!(count(&e, base(Cond { word: true, ..Cond::text("INV") })), 1);
    assert_eq!(count(&e, base(Cond { word: true, ..Cond::text("INVO") })), 0);
    assert_eq!(count(&e, base(Cond { fields: vec![Field::Sender], ..Cond::text("carol@outside") })) >= 1, true);
    assert_eq!(count(&e, base(Cond { fields: vec![Field::Headers], regex: true, ..Cond::text(&format!("(?m)^Subject: Hello {tag}$")) })), 1);
    let neg = Query { conds: vec![Cond::text(&format!("Hello {tag}")), Cond { negate: true, ..Cond::text("Second line") }], accounts: vec![alice], ..Default::default() };
    assert_eq!(count(&e, neg), 0);
    assert!(search::compile(&Cond { regex: true, ..Cond::text("(unclosed") }).is_err());
    println!("  ok  search: plain, case, regex, fields, headers, NOT, bad regex");

    // ---- catch-all: reply from the alias the mail was sent to ----
    smtp("shop@outside.test", "orders@example.org", &plain("Shop <shop@outside.test>", "Orders <orders@example.org>", &format!("Order {tag}"), "Your order 123."));
    wait("mail to the catch-all arrives", 45, || find(&e, alice, &format!("Order {tag}")).is_some());
    let oid = find(&e, alice, &format!("Order {tag}")).unwrap();
    let opened = e.rt.block_on(e.open(oid)).unwrap();
    let from = e.reply_from(alice, &opened.parsed.to, &opened.parsed.cc);
    assert_eq!(from.email, "orders@example.org");
    println!("  ok  reply from {}", from.full());
    assert!(e.our_addresses(alice).iter().any(|a| a.email == "orders@example.org"));
    assert!(e.is_ours(alice, "anything@example.org"));
    let reply = Outgoing {
        account: alice,
        draft: None,
        from: from.clone(),
        to: vec![Addr::new(Some("Bob"), "bob@example.com")],
        cc: vec![],
        bcc: vec![],
        subject: mime::reply_subject(&opened.parsed.subject),
        body: format!("reply body {tag}\n\n{}", mime::quote("Your order 123.")),
        files: vec![("note.txt".into(), "text/plain".into(), b"hi\n".to_vec())],
        encrypt: false,
        sign: false,
        in_reply_to: opened.parsed.message_id.clone(),
        references: opened.parsed.message_id.clone(),
        answering: Some(oid),
    };
    e.rt.block_on(e.send(reply)).unwrap();
    wait("bob gets the reply sent from the alias", 45, || find(&e, bob, &format!("reply body {tag}")).is_some());
    let bid = find(&e, bob, &format!("reply body {tag}")).unwrap();
    let got = e.rt.block_on(e.open(bid)).unwrap();
    assert_eq!(got.parsed.from[0].email, "orders@example.org");
    assert_eq!(got.parsed.attachments[0].name, "note.txt");
    assert_eq!(got.parsed.in_reply_to, vec![opened.parsed.message_id.clone().unwrap()]);
    wait("the original is marked answered", 20, || e.db.msg(oid).map(|m| m.answered()).unwrap_or(false));
    let sent = e.db.mailbox_by_role(alice, "sent").unwrap();
    wait("the copy is in alice's Sent", 20, || e.db.list(alice, &sent.id, 500).iter().any(|m| m.subject.contains(&tag) && m.subject.starts_with("Re:")));

    // ---- encrypted + signed ----
    let (all_have, _) = e.can_encrypt(&[Addr::new(None, "bob@example.com")]);
    assert!(all_have);
    let secret = Outgoing {
        account: alice,
        draft: None,
        from: Addr::new(Some("Alice"), "alice@example.com"),
        to: vec![Addr::new(None, "bob@example.com")],
        cc: vec![],
        bcc: vec![],
        subject: format!("Secret {tag}"),
        body: format!("encrypted body {tag}-enc"),
        files: vec![],
        encrypt: true,
        sign: true,
        in_reply_to: None,
        references: None,
        answering: None,
    };
    e.rt.block_on(e.send(secret)).unwrap();
    wait("bob gets the encrypted message and it is searchable", 45, || find(&e, bob, &format!("{tag}-enc")).is_some());
    let sid = find(&e, bob, &format!("{tag}-enc")).unwrap();
    let o = e.rt.block_on(e.open(sid)).unwrap();
    let note = o.note.clone().unwrap();
    assert!(note.encrypted && note.line.contains("signed by Alice"), "{}", note.line);
    assert_eq!(note.level, "ok");
    println!("  ok  {}", note.line);

    // ---- drafts ----
    let draft = Outgoing { subject: format!("Draft {tag}"), body: "draft text".into(), encrypt: false, sign: false, ..o_like(alice) };
    e.rt.block_on(e.save_draft(draft)).unwrap();
    let drafts = e.db.mailbox_by_role(alice, "drafts").unwrap();
    wait("the draft shows in Drafts (encrypted to self, readable here)", 30, || {
        e.db.list(alice, &drafts.id, 500).iter().any(|m| m.subject == format!("Draft {tag}") && m.fetched == 1)
    });

    // ---- flags, moves, trash, checked on the server ----
    let j = Jmap::new(SERVER, "alice", "alicepw").unwrap();
    let jid = e.db.msg(id).unwrap().jid;
    e.rt.block_on(e.set_keyword(vec![id], "$flagged", true)).unwrap();
    e.rt.block_on(e.set_keyword(vec![id], "$seen", true)).unwrap();
    let m = e.rt.block_on(j.metas(&[jid.clone()])).unwrap();
    assert!(m[0].keywords.iter().any(|k| k == "$flagged") && m[0].keywords.iter().any(|k| k == "$seen"));
    let junk = e.db.mailbox_by_role(alice, "junk").unwrap();
    e.rt.block_on(e.move_to(vec![id], alice, &junk.id)).unwrap();
    let m = e.rt.block_on(j.metas(&[jid.clone()])).unwrap();
    assert_eq!(m[0].mailboxes, vec![junk.id.clone()]);
    e.rt.block_on(e.trash(vec![id])).unwrap();
    let trash = e.db.mailbox_by_role(alice, "trash").unwrap();
    let m = e.rt.block_on(j.metas(&[jid.clone()])).unwrap();
    assert_eq!(m[0].mailboxes, vec![trash.id.clone()]);
    e.rt.block_on(e.trash(vec![id])).unwrap();
    let m = e.rt.block_on(j.metas(&[jid.clone()])).unwrap();
    assert!(m.is_empty(), "deleted for good from the trash");
    assert!(e.db.msg(id).is_none());
    println!("  ok  flag, read, move, trash, delete (server agrees)");

    // ---- sieve ----
    let a = e.acct(alice).unwrap();
    let scripts = e.rt.block_on(a.jmap.sieve_scripts()).unwrap();
    let was_active = scripts.iter().find(|s| s.active).map(|s| s.id.clone());
    let bad = e.rt.block_on(a.jmap.sieve_validate("if header :contains \"subject\" \"x\" { fileinto \"y\" }")).unwrap();
    assert!(bad.is_some());
    println!("  ok  invalid script refused: {}", bad.unwrap());
    let rule = maildot_core::sieve::Rule {
        field: maildot_core::sieve::Field::Subject,
        header: String::new(),
        matcher: maildot_core::sieve::Match::Contains,
        value: format!("SIEVE-{tag}"),
        action: maildot_core::sieve::Action::Move,
        target: junk.name.clone(),
        also_read: true,
        stop: true,
    };
    let text = maildot_core::sieve::add_rule(maildot_core::sieve::TEMPLATE, &rule);
    assert!(e.rt.block_on(a.jmap.sieve_validate(&text)).unwrap().is_none(), "{text}");
    let sid = e.rt.block_on(a.jmap.sieve_save(None, &format!("maildot-{tag}"), &text, true)).unwrap();
    smtp("x@outside.test", "alice@example.com", &plain("x@outside.test", "alice@example.com", &format!("SIEVE-{tag} filtered"), "filtered"));
    wait("the sieve rule filed the message into junk, read", 45, || {
        e.db.list(alice, &junk.id, 500).iter().any(|m| m.subject.contains(&format!("SIEVE-{tag}")) && m.seen())
    });
    e.rt.block_on(a.jmap.sieve_activate(was_active.as_deref())).unwrap();
    e.rt.block_on(a.jmap.sieve_delete(&sid)).unwrap();
    let after = e.rt.block_on(a.jmap.sieve_scripts()).unwrap();
    assert_eq!(after.iter().find(|s| s.active).map(|s| s.id.clone()), was_active);
    println!("  ok  sieve: validate, save, activate, filter, restore, delete");

    // ---- calendar ----
    wait("alice's calendars", 60, || e.calendars().iter().any(|c| c.account == alice && c.owner.is_none()));
    let cal = e.calendars().into_iter().find(|c| c.account == alice && c.owner.is_none() && !c.read_only).unwrap();
    println!("      calendar {} ({})", cal.name, cal.href);
    let local = ical::local_tz();
    let day = chrono::Local::now().date_naive() + chrono::Duration::days(1);
    let d = Draft {
        title: format!("Weekly {tag}"),
        location: "Brno".into(),
        notes: "notes".into(),
        all_day: false,
        start: day.and_hms_opt(9, 0, 0).unwrap(),
        end: day.and_hms_opt(10, 0, 0).unwrap(),
        repeat: Repeat::Weekly,
        reminder_min: Some(10),
        rule_text: None,
    };
    let eid = e.save_event(alice, &cal.href, None, &d).unwrap();
    let href = e.event(eid).unwrap().href;
    wait("the event reaches the server", 30, || !e.event_pending(eid));
    let on_server = e.rt.block_on(a.dav.list(&cal.href)).unwrap();
    assert!(on_server.contains_key(&href));
    let lo = ical::midnight(day, local);
    let shown = e.occurrences(lo, lo + 15 * 86400);
    let mine: Vec<_> = shown.iter().filter(|s| s.ev.id == eid).collect();
    assert_eq!(mine.len(), 3, "weekly: three in 15 days");
    e.skip_instance(eid, mine[1].occ.instance.unwrap(), false).unwrap();
    let mut d2 = ical::draft_of(&e.event(eid).unwrap().ics, local).unwrap();
    d2.title = format!("Weekly {tag} renamed");
    e.save_event(alice, &cal.href, Some(eid), &d2).unwrap();
    wait("edit and skipped instance reach the server", 30, || !e.event_pending(eid));
    let fetched = e.rt.block_on(a.dav.multiget(&cal.href, &[href.clone()])).unwrap();
    assert!(fetched[0].ics.contains("renamed") && fetched[0].ics.contains("EXDATE"), "{}", fetched[0].ics);
    assert_eq!(e.occurrences(lo, lo + 15 * 86400).iter().filter(|s| s.ev.id == eid).count(), 2);
    println!("  ok  calendar: create, sync, skip one, edit");

    // ---- sharing: bob sees alice's calendar as a calendar of its own ----
    let mut acl = e.rt.block_on(e.calendar_acl(alice, &cal.href)).unwrap_or_default();
    acl.retain(|x| x.user != "bob");
    acl.push(Ace { user: "bob".into(), write: false });
    e.rt.block_on(e.set_calendar_acl(alice, &cal.href, acl)).unwrap();
    e.sync_now(Some(bob));
    wait("bob sees alice's calendar, owner alice, read only", 60, || {
        e.calendars().iter().any(|c| c.account == bob && c.owner.as_deref() == Some("alice") && c.read_only)
    });
    wait("and its events", 60, || e.occurrences(lo, lo + 15 * 86400).iter().any(|s| s.cal.account == bob && s.ev.summary.contains("renamed")));
    assert!(e.rt.block_on(e.set_calendar_acl(alice, &cal.href, vec![Ace { user: "nobody-here".into(), write: false }])).is_err());
    println!("  ok  sharing (and an unknown user refused)");
    e.delete_event(eid).unwrap();
    wait("the deleted event leaves the server", 30, || !e.rt.block_on(a.dav.list(&cal.href)).unwrap().contains_key(&href));

    let st = e.statuses();
    println!("status: {st:?}");
    assert!(!st.values().any(|(_, bad)| *bad), "no account in error");
    // headers for an alias identity left behind? (fine either way)
    let _ = Headers { from: Addr::default(), to: vec![], cc: vec![], subject: String::new(), message_id: String::new(), in_reply_to: None, references: None };
    e.stop_all();
    println!("ALL OK");
}

fn o_like(account: i64) -> Outgoing {
    Outgoing {
        account,
        draft: None,
        from: Addr::new(Some("Alice"), "alice@example.com"),
        to: vec![Addr::new(None, "bob@example.com")],
        cc: vec![],
        bcc: vec![],
        subject: String::new(),
        body: String::new(),
        files: vec![],
        encrypt: false,
        sign: false,
        in_reply_to: None,
        references: None,
        answering: None,
    }
}
