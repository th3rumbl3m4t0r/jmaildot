//! Messages in and out. Reading: mail-parser, plus PGP/MIME (RFC 3156) and inline PGP.
//! Writing: our own small MIME writer (quoted-printable text, base64 attachments, RFC 2047
//! headers), so the bytes we sign are exactly the bytes we send.

use crate::db::SearchText;
use crate::model::Addr;
use crate::pgp::{self, KeyRing, Sig};
use anyhow::Result;
use mail_parser::{Address, HeaderValue, Message, MessageParser, MimeHeaders, PartType};
use serde::{Deserialize, Serialize};

/// A file inside a message.
#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub mime: String,
    pub cid: Option<String>,
    pub inline: bool,
    pub data: Vec<u8>,
}

/// What the reader shows.
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub subject: String,
    pub from: Vec<Addr>,
    pub sender: Vec<Addr>,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub bcc: Vec<Addr>,
    pub reply_to: Vec<Addr>,
    pub date: Option<i64>,
    pub message_id: Option<String>,
    pub in_reply_to: Vec<String>,
    pub references: Vec<String>,
    pub list_id: Option<String>,
    pub headers: Vec<(String, String)>,
    pub text: Option<String>,
    pub html: Option<String>,
    pub attachments: Vec<Part>,
    /// cid: images and other inline parts the HTML may refer to.
    pub inline: Vec<Part>,
}

/// The PGP line shown above a message, kept in the database as JSON.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PgpNote {
    pub encrypted: bool,
    pub signed: bool,
    /// ok, warn or bad
    pub level: String,
    pub line: String,
}

/// A downloaded message after decryption and text extraction.
pub struct Processed {
    pub eml: Vec<u8>,
    /// 1 stored, 2 stored but still encrypted
    pub fetched: i64,
    pub encrypted: bool,
    pub note: Option<PgpNote>,
    pub err: Option<String>,
    pub text: SearchText,
    pub preview: String,
    pub has_att: bool,
}

fn addrs(a: Option<&Address>) -> Vec<Addr> {
    match a {
        None => vec![],
        Some(a) => a.iter().filter_map(|x| x.address().map(|e| Addr::new(x.name(), e))).collect(),
    }
}

fn text_list(v: &HeaderValue) -> Vec<String> {
    match v {
        HeaderValue::Text(t) => vec![t.to_string()],
        HeaderValue::TextList(l) => l.iter().map(|t| t.to_string()).collect(),
        _ => vec![],
    }
}

/// `Name <a@b>, c@d` for the search columns.
fn addr_text(list: &[Addr]) -> String {
    list.iter().map(|a| a.full()).collect::<Vec<_>>().join(", ")
}

fn unfold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_nl = false;
    for c in s.chars() {
        if c == '\r' {
            continue;
        }
        if c == '\n' {
            prev_nl = true;
            continue;
        }
        if prev_nl {
            if c != ' ' && c != '\t' {
                out.push(' ');
            }
            prev_nl = false;
        }
        out.push(c);
    }
    out.trim().to_string()
}

pub fn parser() -> MessageParser {
    MessageParser::default()
}

pub fn ext_for(mime: &str) -> &'static str {
    match mime {
        "application/pdf" => ".pdf",
        "image/jpeg" => ".jpg",
        "image/png" => ".png",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        "text/plain" => ".txt",
        "text/html" => ".html",
        "text/calendar" => ".ics",
        "message/rfc822" => ".eml",
        "application/zip" => ".zip",
        "application/pgp-keys" => ".asc",
        _ => "",
    }
}

fn part_mime(p: &mail_parser::MessagePart) -> String {
    p.content_type()
        .map(|c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_lowercase())
        .unwrap_or_else(|| if matches!(p.body, PartType::Text(_)) { "text/plain".into() } else { "application/octet-stream".into() })
}

/// Parses a stored message for the reader.
pub fn parse(raw: &[u8]) -> Parsed {
    let Some(m) = parser().parse(raw) else {
        return Parsed { text: Some(String::from_utf8_lossy(raw).into_owned()), ..Default::default() };
    };
    let mut p = Parsed {
        subject: m.subject().unwrap_or("").split_whitespace().collect::<Vec<_>>().join(" "),
        from: addrs(m.from()),
        sender: addrs(m.sender()),
        to: addrs(m.to()),
        cc: addrs(m.cc()),
        bcc: addrs(m.bcc()),
        reply_to: addrs(m.reply_to()),
        date: m.date().map(|d| d.to_timestamp()),
        message_id: m.message_id().map(String::from),
        in_reply_to: text_list(m.in_reply_to()),
        references: text_list(m.references()),
        list_id: m.list_id().as_text().map(String::from),
        headers: m.headers_raw().map(|(n, v)| (n.to_string(), unfold(v))).collect(),
        ..Default::default()
    };
    // text: the plain parts as they are; html: the first html part
    let mut texts = Vec::new();
    for id in &m.text_body {
        if let Some(part) = m.parts.get(*id) {
            if let PartType::Text(t) = &part.body {
                texts.push(t.to_string());
            }
        }
    }
    for id in &m.html_body {
        if let Some(part) = m.parts.get(*id) {
            if let PartType::Html(h) = &part.body {
                p.html = Some(h.to_string());
                break;
            }
        }
    }
    if !texts.is_empty() {
        p.text = Some(texts.join("\n\n"));
    }
    let body_ids: Vec<usize> = m.text_body.iter().chain(m.html_body.iter()).cloned().collect();
    for (n, id) in m.attachments.iter().enumerate() {
        let Some(part) = m.parts.get(*id) else { continue };
        if body_ids.contains(id) {
            continue;
        }
        let mime = part_mime(part);
        let cid = part.content_id().map(|c| c.trim_matches(|x| x == '<' || x == '>').to_string());
        let disp_inline = part.content_disposition().map(|d| d.is_inline()).unwrap_or(false);
        let name = part
            .attachment_name()
            .map(String::from)
            .unwrap_or_else(|| if part.is_message() { format!("message-{}.eml", n + 1) } else { format!("part-{}{}", n + 1, ext_for(&mime)) });
        let data = match &part.body {
            PartType::Message(inner) => inner.raw_message().to_vec(),
            _ => part.contents().to_vec(),
        };
        let att = Part { name, mime: mime.clone(), cid: cid.clone(), inline: disp_inline, data };
        if cid.is_some() && (disp_inline || mime.starts_with("image/")) && part.attachment_name().is_none() {
            p.inline.push(att);
        } else {
            if cid.is_some() {
                p.inline.push(att.clone());
            }
            p.attachments.push(att);
        }
    }
    p
}

// ---- PGP on the way in ----

fn header_block(raw: &[u8]) -> (usize, usize) {
    // (end of headers, start of body)
    if let Some(i) = find(raw, b"\r\n\r\n") {
        let j = find(raw, b"\n\n");
        if let Some(j) = j {
            if j < i {
                return (j, j + 2);
            }
        }
        return (i, i + 4);
    }
    if let Some(j) = find(raw, b"\n\n") {
        return (j, j + 2);
    }
    (raw.len(), raw.len())
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    memfind(hay, needle, 0)
}

fn memfind(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || hay.len() < needle.len() {
        return None;
    }
    (from..=hay.len() - needle.len()).find(|&i| &hay[i..i + needle.len()] == needle)
}

/// The header fields of [raw] (folded lines kept together), without the MIME ones.
fn outer_headers(raw: &[u8]) -> Vec<u8> {
    let (end, _) = header_block(raw);
    let block = &raw[..end];
    let text = String::from_utf8_lossy(block);
    let mut fields: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !fields.is_empty() {
            let l = fields.last_mut().unwrap();
            l.push_str("\r\n");
            l.push_str(line);
        } else if !line.is_empty() {
            fields.push(line.to_string());
        }
    }
    let mut out = Vec::new();
    for f in fields {
        let name = f.split(':').next().unwrap_or("").trim().to_lowercase();
        if name == "content-type" || name == "content-transfer-encoding" || name == "content-disposition" {
            continue;
        }
        out.extend_from_slice(f.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// The ciphertext of a PGP/MIME message, if it is one.
fn pgp_mime_ciphertext(m: &Message) -> Option<Vec<u8>> {
    let root = m.root_part();
    let ct = root.content_type()?;
    if !ct.ctype().eq_ignore_ascii_case("multipart") || !ct.subtype().map(|s| s.eq_ignore_ascii_case("encrypted")).unwrap_or(false) {
        return None;
    }
    for part in m.parts.iter().skip(1) {
        let data = part.contents();
        if find(data, b"-----BEGIN PGP MESSAGE-----").is_some() {
            return Some(data.to_vec());
        }
    }
    for part in m.parts.iter().skip(1) {
        if part_mime(part) == "application/octet-stream" {
            return Some(part.contents().to_vec());
        }
    }
    None
}

/// LF → CRLF where it isn't already (what a signature is made over).
pub fn crlf(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + data.len() / 40);
    let mut prev = 0u8;
    for &b in data {
        if b == b'\n' && prev != b'\r' {
            out.push(b'\r');
        }
        out.push(b);
        prev = b;
    }
    out
}

/// A multipart/signed entity's two halves: the signed bytes (exactly as sent, without the
/// CRLF in front of the next boundary) and the signature part's body.
pub fn split_signed(entity: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let m = parser().parse(entity)?;
    let root = m.root_part();
    let ct = root.content_type()?;
    if !ct.ctype().eq_ignore_ascii_case("multipart") || !ct.subtype().map(|s| s.eq_ignore_ascii_case("signed")).unwrap_or(false) {
        return None;
    }
    let boundary = ct.attribute("boundary")?.to_string();
    let delim = format!("--{boundary}");
    let d = delim.as_bytes();
    // first delimiter at a line start
    let mut pos = 0;
    let mut starts = Vec::new();
    while let Some(i) = memfind(entity, d, pos) {
        if i == 0 || entity[i - 1] == b'\n' {
            starts.push(i);
        }
        pos = i + d.len();
    }
    if starts.len() < 3 {
        return None;
    }
    let after = |i: usize| -> usize {
        match memfind(entity, b"\n", i) {
            Some(n) => n + 1,
            None => entity.len(),
        }
    };
    let s1 = after(starts[0]);
    let mut e1 = starts[1];
    if e1 > 0 && entity[e1 - 1] == b'\n' {
        e1 -= 1;
        if e1 > 0 && entity[e1 - 1] == b'\r' {
            e1 -= 1;
        }
    }
    let signed = entity[s1..e1].to_vec();
    let s2 = after(starts[1]);
    let sigpart = &entity[s2..starts[2]];
    let (_, body) = header_block(sigpart);
    let sig = sigpart[body.min(sigpart.len())..].to_vec();
    Some((signed, sig))
}

fn note_for(encrypted: bool, sigs: &[Sig], sender: &str) -> Option<PgpNote> {
    if !encrypted && sigs.is_empty() {
        return None;
    }
    let mut level = "ok".to_string();
    for s in sigs {
        if s.known && !s.valid {
            level = "bad".into();
        } else if (!s.known || !s.emails().iter().any(|e| e.eq_ignore_ascii_case(sender))) && level == "ok" {
            level = "warn".into();
        }
    }
    let mut line = Vec::new();
    if encrypted {
        line.push("encrypted".to_string());
    }
    if !sigs.is_empty() {
        line.push(pgp::describe_sigs(sigs, sender));
    }
    Some(PgpNote { encrypted, signed: !sigs.is_empty(), level, line: line.join(" · ") })
}

/// Turns a downloaded message into what we store: decrypted when it is PGP/MIME and we hold
/// the key, the signature checked, the searchable text taken out.
pub fn process(raw: &[u8], ring: &KeyRing) -> Processed {
    let mut eml = raw.to_vec();
    let mut fetched = 1;
    let mut encrypted = false;
    let mut note = None;
    let mut err = None;
    let parsed_outer = parser().parse(raw);
    let sender_of = |m: &Message| m.from().and_then(|f| f.first()).and_then(|a| a.address()).unwrap_or("").to_lowercase();
    if let Some(m) = &parsed_outer {
        let sender = sender_of(m);
        if let Some(ct) = pgp_mime_ciphertext(m) {
            encrypted = true;
            match ring.decrypt(&ct) {
                Ok(d) => {
                    let mut full = outer_headers(raw);
                    let inner = d.data;
                    let mut sigs = d.sigs;
                    if let Some((signed, sig)) = split_signed(&inner) {
                        if let Ok(s) = ring.verify_detached(&crlf(&signed), &sig) {
                            sigs.extend(s);
                        }
                    }
                    full.extend_from_slice(&inner);
                    eml = full;
                    note = note_for(true, &sigs, &sender);
                }
                Err(e) => {
                    fetched = 2;
                    err = Some(if pgp::needs_passphrase(&e) {
                        "locked: unlock your key to read it".to_string()
                    } else if pgp::no_key(&e) {
                        "encrypted to a key we don't have".to_string()
                    } else {
                        format!("cannot decrypt: {e}")
                    });
                    note = Some(PgpNote { encrypted: true, signed: false, level: "warn".into(), line: "encrypted".into() });
                }
            }
        } else if let Some((signed, sig)) = split_signed(raw) {
            let sigs = ring.verify_detached(&crlf(&signed), &sig).unwrap_or_default();
            note = note_for(false, &sigs, &sender);
        }
    }
    let p = parse(&eml);
    let mut body = String::new();
    let mut inline_note = None;
    if let Some(m) = parser().parse(&eml[..]) {
        for i in 0..m.text_body_count() {
            if let Some(t) = m.body_text(i) {
                body.push_str(&t);
                body.push('\n');
            }
        }
        if body.trim().is_empty() {
            for i in 0..m.html_body_count() {
                if let Some(h) = m.body_html(i) {
                    body.push_str(&crate::html::to_text(&h));
                    body.push('\n');
                }
            }
        }
        // nested messages (forwards) count as body too
        for part in &m.parts {
            if let PartType::Message(inner) = &part.body {
                for i in 0..inner.text_body_count() {
                    if let Some(t) = inner.body_text(i) {
                        body.push_str(&t);
                        body.push('\n');
                    }
                }
            }
        }
    }
    // inline PGP: the text says what it is once decrypted
    if body.contains("-----BEGIN PGP MESSAGE-----") {
        if let Some(t) = decrypt_inline(&body, ring) {
            encrypted = true;
            inline_note = Some(PgpNote { encrypted: true, signed: false, level: "ok".into(), line: "encrypted (inline)".into() });
            body = t;
        }
    }
    if note.is_none() {
        note = inline_note;
    }
    const MAX_BODY: usize = 4 << 20;
    if body.len() > MAX_BODY {
        let mut cut = MAX_BODY;
        while !body.is_char_boundary(cut) {
            cut -= 1;
        }
        body.truncate(cut);
    }
    let mut attach = String::new();
    for a in &p.attachments {
        attach.push_str(&a.name);
        attach.push_str(" (");
        attach.push_str(&a.mime);
        attach.push_str(")\n");
        if (a.mime.starts_with("text/") || a.mime == "application/json") && a.data.len() < 512 * 1024 {
            attach.push_str(&String::from_utf8_lossy(&a.data));
            attach.push('\n');
        }
    }
    let headers = p.headers.iter().map(|(n, v)| format!("{n}: {v}")).collect::<Vec<_>>().join("\n");
    let preview: String = body.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(240).collect();
    let text = SearchText {
        subject: p.subject.clone(),
        sender: addr_text(&[p.from.clone(), p.sender.clone(), p.reply_to.clone()].concat()),
        rcpts: addr_text(&[p.to.clone(), p.cc.clone(), p.bcc.clone()].concat()),
        headers,
        body,
        attach,
    };
    Processed { has_att: !p.attachments.is_empty(), eml, fetched, encrypted, note, err, text, preview }
}

/// Every inline `-----BEGIN PGP MESSAGE-----` block in [text] decrypted in place; None when none could be.
pub fn decrypt_inline(text: &str, ring: &KeyRing) -> Option<String> {
    let begin = "-----BEGIN PGP MESSAGE-----";
    let end = "-----END PGP MESSAGE-----";
    let mut out = String::new();
    let mut rest = text;
    let mut any = false;
    while let Some(i) = rest.find(begin) {
        let Some(j) = rest[i..].find(end) else { break };
        let block = &rest[i..i + j + end.len()];
        out.push_str(&rest[..i]);
        match ring.decrypt(block.as_bytes()) {
            Ok(d) => {
                out.push_str(&String::from_utf8_lossy(&d.data));
                any = true;
            }
            Err(_) => out.push_str(block),
        }
        rest = &rest[i + j + end.len()..];
    }
    out.push_str(rest);
    if any {
        Some(out)
    } else {
        None
    }
}

// ---- writing ----

pub fn qp_encode(text: &str) -> String {
    let norm = text.replace("\r\n", "\n");
    let mut out = String::new();
    for (li, line) in norm.split('\n').enumerate() {
        if li > 0 {
            out.push_str("\r\n");
        }
        let bytes = line.as_bytes();
        let mut col = 0;
        for (i, &b) in bytes.iter().enumerate() {
            let last = i + 1 == bytes.len();
            let enc = match b {
                b'=' => true,
                b' ' | b'\t' => last,
                33..=126 => false,
                _ => true,
            } || (col == 0 && b == b'.' && last)
                || (col == 0 && line.starts_with("From ") && i == 0);
            let piece = if enc { format!("={b:02X}") } else { (b as char).to_string() };
            if col + piece.len() > 75 {
                out.push_str("=\r\n");
                col = 0;
            }
            out.push_str(&piece);
            col += piece.len();
        }
    }
    out
}

pub fn b64_lines(data: &[u8]) -> String {
    use base64::Engine as _;
    let s = base64::engine::general_purpose::STANDARD.encode(data);
    let mut out = String::with_capacity(s.len() + s.len() / 76 * 2 + 2);
    for chunk in s.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap());
        out.push_str("\r\n");
    }
    out
}

/// RFC 2047 when needed: `=?utf-8?b?…?=` words of at most 75 characters.
pub fn encode_word(s: &str) -> String {
    if s.is_ascii() && !s.contains("=?") && !s.chars().any(|c| c.is_control()) {
        return s.to_string();
    }
    use base64::Engine as _;
    let mut words = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if (cur.len() + c.len_utf8()) > 45 {
            words.push(format!("=?utf-8?b?{}?=", base64::engine::general_purpose::STANDARD.encode(cur.as_bytes())));
            cur.clear();
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        words.push(format!("=?utf-8?b?{}?=", base64::engine::general_purpose::STANDARD.encode(cur.as_bytes())));
    }
    words.join("\r\n ")
}

pub fn addr_header(a: &Addr) -> String {
    match &a.name {
        Some(n) if !n.is_empty() && n != &a.email => {
            if n.is_ascii() {
                let special = n.chars().any(|c| "()<>[]:;@\\,.\"".contains(c));
                if special {
                    format!("\"{}\" <{}>", n.replace('\\', "\\\\").replace('"', "\\\""), a.email)
                } else {
                    format!("{} <{}>", n, a.email)
                }
            } else {
                format!("{} <{}>", encode_word(n), a.email)
            }
        }
        _ => a.email.clone(),
    }
}

fn list_header(name: &str, list: &[Addr]) -> String {
    let mut out = format!("{name}: ");
    let mut col = out.len();
    for (i, a) in list.iter().enumerate() {
        let s = addr_header(a);
        if i > 0 {
            out.push(',');
            col += 1;
            if col + s.len() + 1 > 76 {
                out.push_str("\r\n ");
                col = 1;
            } else {
                out.push(' ');
                col += 1;
            }
        }
        out.push_str(&s);
        col += s.len();
    }
    out.push_str("\r\n");
    out
}

pub struct FileAtt {
    pub name: String,
    pub mime: String,
    pub data: Vec<u8>,
}

fn boundary() -> String {
    use rand::Rng;
    let r: u128 = rand::thread_rng().gen();
    format!("=_maildot_{r:032x}")
}

fn param_name(key: &str, name: &str) -> String {
    if name.is_ascii() && !name.contains('"') {
        format!("{key}=\"{name}\"")
    } else {
        let enc: String = name
            .bytes()
            .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
            .collect();
        format!("{key}*=utf-8''{enc}")
    }
}

/// The body entity: text alone (quoted-printable), or multipart/mixed with the files.
pub fn body_entity(text: &str, files: &[FileAtt]) -> Vec<u8> {
    let text_part = format!("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n{}\r\n", qp_encode(text));
    if files.is_empty() {
        return text_part.into_bytes();
    }
    let b = boundary();
    let mut out = format!("Content-Type: multipart/mixed; boundary=\"{b}\"\r\n\r\nThis is a MIME message.\r\n--{b}\r\n").into_bytes();
    out.extend_from_slice(text_part.as_bytes());
    for f in files {
        out.extend_from_slice(format!("\r\n--{b}\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "Content-Type: {}; {}\r\nContent-Disposition: attachment; {}\r\nContent-Transfer-Encoding: base64\r\n\r\n",
                if f.mime.is_empty() { "application/octet-stream" } else { &f.mime },
                param_name("name", &f.name),
                param_name("filename", &f.name)
            )
            .as_bytes(),
        );
        out.extend_from_slice(b64_lines(&f.data).as_bytes());
    }
    out.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    out
}

/// PGP/MIME encrypted (RFC 3156 section 4) around an armored message.
pub fn encrypted_entity(armored: &[u8]) -> Vec<u8> {
    let b = boundary();
    let mut out = format!(
        "Content-Type: multipart/encrypted; protocol=\"application/pgp-encrypted\"; boundary=\"{b}\"\r\n\r\nThis is an OpenPGP/MIME encrypted message (RFC 4880 and 3156)\r\n--{b}\r\nContent-Type: application/pgp-encrypted\r\nContent-Description: PGP/MIME version identification\r\n\r\nVersion: 1\r\n\r\n--{b}\r\nContent-Type: application/octet-stream; name=\"encrypted.asc\"\r\nContent-Description: OpenPGP encrypted message\r\nContent-Disposition: inline; filename=\"encrypted.asc\"\r\n\r\n"
    )
    .into_bytes();
    out.extend_from_slice(&crlf(armored));
    out.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    out
}

/// PGP/MIME signed (RFC 3156 section 5): [canon] is the signed entity, already CRLF.
pub fn signed_entity(canon: &[u8], sig: &[u8]) -> Vec<u8> {
    let b = boundary();
    let mut out = format!(
        "Content-Type: multipart/signed; micalg=pgp-sha256; protocol=\"application/pgp-signature\"; boundary=\"{b}\"\r\n\r\nThis is an OpenPGP/MIME signed message (RFC 4880 and 3156)\r\n--{b}\r\n"
    )
    .into_bytes();
    out.extend_from_slice(canon);
    out.extend_from_slice(
        format!("\r\n--{b}\r\nContent-Type: application/pgp-signature; name=\"signature.asc\"\r\nContent-Description: OpenPGP digital signature\r\nContent-Disposition: attachment; filename=\"signature.asc\"\r\n\r\n").as_bytes(),
    );
    out.extend_from_slice(&crlf(sig));
    out.extend_from_slice(format!("\r\n--{b}--\r\n").as_bytes());
    out
}

pub struct Headers {
    pub from: Addr,
    pub to: Vec<Addr>,
    pub cc: Vec<Addr>,
    pub subject: String,
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
}

pub fn message_id(domain: &str) -> String {
    use rand::Rng;
    let r: u128 = rand::thread_rng().gen();
    format!("{r:032x}@{}", if domain.is_empty() { "localhost" } else { domain })
}

fn bracket(id: &str) -> String {
    let t = id.trim();
    if t.starts_with('<') {
        t.to_string()
    } else {
        format!("<{t}>")
    }
}

/// The finished RFC 5322 message.
pub fn message(h: &Headers, entity: &[u8]) -> Vec<u8> {
    let mut out = String::new();
    out.push_str(&format!("Date: {}\r\n", chrono::Local::now().to_rfc2822()));
    out.push_str(&format!("From: {}\r\n", addr_header(&h.from)));
    if !h.to.is_empty() {
        out.push_str(&list_header("To", &h.to));
    }
    if !h.cc.is_empty() {
        out.push_str(&list_header("Cc", &h.cc));
    }
    out.push_str(&format!("Subject: {}\r\n", encode_word(&h.subject)));
    out.push_str(&format!("Message-ID: {}\r\n", bracket(&h.message_id)));
    if let Some(r) = &h.in_reply_to {
        out.push_str(&format!("In-Reply-To: {}\r\n", bracket(r)));
    }
    if let Some(r) = &h.references {
        let refs: Vec<String> = r.split_whitespace().map(bracket).collect();
        if !refs.is_empty() {
            out.push_str(&format!("References: {}\r\n", refs.join("\r\n ")));
        }
    }
    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str(concat!("User-Agent: maildot/", env!("CARGO_PKG_VERSION"), "\r\n"));
    let mut v = out.into_bytes();
    v.extend_from_slice(entity);
    v
}

/// `Re: x` stays `Re: x`; anything else gets one `Re: `.
pub fn reply_subject(s: &str) -> String {
    let t = s.trim();
    let l = t.to_lowercase();
    if l.starts_with("re:") || l.starts_with("aw:") || l.starts_with("odp:") {
        t.to_string()
    } else {
        format!("Re: {t}")
    }
}

pub fn forward_subject(s: &str) -> String {
    let t = s.trim();
    let l = t.to_lowercase();
    if l.starts_with("fwd:") || l.starts_with("fw:") {
        t.to_string()
    } else {
        format!("Fwd: {t}")
    }
}

/// `> ` in front of every line.
pub fn quote(text: &str) -> String {
    text.replace("\r\n", "\n").lines().map(|l| if l.starts_with('>') { format!(">{l}") } else { format!("> {l}") }).collect::<Vec<_>>().join("\n")
}

pub fn raw_headers(raw: &[u8]) -> String {
    let (end, _) = header_block(raw);
    String::from_utf8_lossy(&raw[..end]).replace("\r\n", "\n")
}

pub fn result_of<T>(r: Result<T>) -> Option<T> {
    r.ok()
}
