//! PGP and MIME against messages and keys made with GnuPG 2.4 (the Android app's test set):
//! alice (ed25519/cv25519, passphrase "alicekey"), bob (rsa3072, no passphrase).

use maildot_core::mime::{self, FileAtt, Headers};
use maildot_core::model::Addr;
use maildot_core::pgp::{self, KeyRing};

fn data(name: &str) -> Vec<u8> {
    std::fs::read(format!("{}/tests/data/{name}", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

fn ring(files: &[&str]) -> KeyRing {
    KeyRing::new(files.iter().flat_map(|f| pgp::parse_certs(&data(f)).unwrap()).collect())
}

#[test]
fn key_info_and_passphrase() {
    let a = pgp::parse_certs(&data("alice.sec.asc")).unwrap();
    let i = pgp::info(&a[0]);
    assert_eq!(i.user_ids, vec!["Alice <alice@example.com>"]);
    assert_eq!(i.emails, vec!["alice@example.com"]);
    assert!(i.secret);
    assert_eq!(i.pretty_fpr(), "C149 DCDC C9F8 03F4 299B 28EB 7C58 A39F A0FD 7A20");
    assert!(i.can_encrypt && i.can_sign);
    let b = pgp::info(&pgp::parse_certs(&data("bob.pub.asc")).unwrap()[0]);
    assert!(!b.secret);
    let mut r = ring(&["alice.sec.asc", "bob.sec.asc"]);
    assert!(r.locked());
    assert!(!r.unlock("nope"));
    assert!(r.unlock("alicekey"));
    assert!(!r.locked());
}

#[test]
fn encrypted_gpg_message_is_stored_decrypted_with_bobs_signature() {
    let mut r = ring(&["alice.sec.asc", "bob.pub.asc"]);
    // locked: kept as it came, marked
    let p = mime::process(&data("m4.eml"), &r);
    assert_eq!(p.fetched, 2);
    assert!(p.err.unwrap().starts_with("locked"));
    assert!(r.unlock("alicekey"));
    let p = mime::process(&data("m4.eml"), &r);
    assert_eq!(p.fetched, 1);
    assert!(p.encrypted);
    let note = p.note.unwrap();
    assert_eq!(note.level, "ok", "{}", note.line);
    assert!(note.line.contains("encrypted") && note.line.contains("signed by Bob <bob@example.com>"), "{}", note.line);
    // the stored file is a plain message: outer headers + the decrypted entity
    let eml = String::from_utf8_lossy(&p.eml);
    assert!(eml.starts_with("From: Bob <bob@example.com>\r\n"));
    assert!(eml.contains("Subject: Secret plan"));
    assert!(!eml.contains("BEGIN PGP MESSAGE"));
    let parsed = mime::parse(&p.eml);
    assert_eq!(parsed.text.unwrap().trim(), "See the attached table — 2 rows.");
    assert_eq!(parsed.attachments.len(), 1);
    assert_eq!(parsed.attachments[0].name, "table.csv");
    assert_eq!(parsed.attachments[0].data, b"col1,col2\n1,2\n3,4\n");
    assert!(p.text.body.contains("See the attached table"));
    assert!(p.text.attach.contains("table.csv") && p.text.attach.contains("col1,col2"));
    assert!(p.has_att);
}

#[test]
fn wrong_key_is_reported() {
    let r = ring(&["bob.sec.asc"]);
    let p = mime::process(&data("m4.eml"), &r);
    assert_eq!(p.fetched, 2);
    assert_eq!(p.err.as_deref(), Some("encrypted to a key we don't have"));
}

#[test]
fn signed_only_is_verified() {
    let r = ring(&["bob.pub.asc"]);
    let p = mime::process(&data("m5.eml"), &r);
    let note = p.note.unwrap();
    assert!(!note.encrypted && note.signed);
    assert_eq!(note.level, "ok", "{}", note.line);
    // unknown signer
    let p = mime::process(&data("m5.eml"), &ring(&["alice.pub.asc"]));
    let note = p.note.unwrap();
    assert_eq!(note.level, "warn");
    assert!(note.line.contains("unknown key"), "{}", note.line);
}

#[test]
fn html_only_and_attachment() {
    let r = KeyRing::default();
    let p = mime::process(&data("m2.eml"), &r);
    assert!(p.text.body.contains("Hello Alice"), "{}", p.text.body);
    let parsed = mime::parse(&data("m2.eml"));
    assert!(parsed.html.unwrap().contains("<b>Alice</b>"));
    let p = mime::process(&data("m3.eml"), &r);
    assert!(p.has_att);
    let p = mime::process(&data("m1.eml"), &r);
    assert!(p.text.body.contains("obyčejný text"));
    assert!(p.note.is_none());
}

#[test]
fn encrypt_and_sign_round_trip() {
    let mut alice = ring(&["alice.sec.asc", "bob.pub.asc"]);
    assert!(alice.unlock("alicekey"));
    let bob_pub = pgp::parse_certs(&data("bob.pub.asc")).unwrap().remove(0);
    let me = alice.ours().unwrap().clone();
    let entity = mime::body_entity("Ahoj Bobe,\ntajný plán — žluťoučký kůň.\n", &[FileAtt { name: "plán č.1.txt".into(), mime: "text/plain".into(), data: b"x=1\n".to_vec() }]);
    let armored = alice.encrypt(&entity, &[&bob_pub, &me], Some(&me)).unwrap();
    assert!(armored.starts_with(b"-----BEGIN PGP MESSAGE-----"));
    let msg = mime::message(
        &Headers {
            from: Addr::new(Some("Alice"), "alice@example.com"),
            to: vec![Addr::new(Some("Bob Bobovič"), "bob@example.com")],
            cc: vec![],
            subject: "Tajný plán".into(),
            message_id: mime::message_id("example.com"),
            in_reply_to: None,
            references: None,
        },
        &mime::encrypted_entity(&armored),
    );
    let bob = ring(&["bob.sec.asc", "alice.pub.asc"]);
    let p = mime::process(&msg, &bob);
    assert_eq!(p.fetched, 1, "{:?}", p.err);
    let note = p.note.unwrap();
    assert_eq!(note.level, "ok", "{}", note.line);
    assert!(note.line.contains("signed by Alice"), "{}", note.line);
    let parsed = mime::parse(&p.eml);
    assert_eq!(parsed.subject, "Tajný plán");
    assert_eq!(parsed.to[0].name.as_deref(), Some("Bob Bobovič"));
    assert!(parsed.text.unwrap().contains("žluťoučký kůň"));
    assert_eq!(parsed.attachments[0].name, "plán č.1.txt");

    // signed only (multipart/signed over the exact bytes)
    let canon = mime::crlf(&mime::body_entity("Trailing space here \nand a line\n", &[]));
    let sig = alice.sign_detached(&canon, &me).unwrap();
    let msg = mime::message(
        &Headers {
            from: Addr::new(Some("Alice"), "alice@example.com"),
            to: vec![Addr::new(None, "bob@example.com")],
            cc: vec![],
            subject: "signed".into(),
            message_id: mime::message_id("example.com"),
            in_reply_to: None,
            references: None,
        },
        &mime::signed_entity(&canon, &sig),
    );
    let p = mime::process(&msg, &ring(&["alice.pub.asc"]));
    let note = p.note.unwrap();
    assert_eq!(note.level, "ok", "{}", note.line);
    assert!(mime::parse(&p.eml).text.unwrap().contains("Trailing space here"));
}

#[test]
fn detached_signature_of_gpg() {
    let r = ring(&["bob.pub.asc"]);
    let (signed, sig) = mime::split_signed(&data("m5.eml")).unwrap();
    let ok = r.verify_detached(&mime::crlf(&signed), &sig).unwrap();
    assert!(ok[0].valid && ok[0].known);
    let mut bad = mime::crlf(&signed);
    bad[10] ^= 1;
    assert!(!r.verify_detached(&bad, &sig).unwrap()[0].valid);
}
