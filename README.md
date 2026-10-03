# jmaildot (mail.)

**A desktop email app (Linux, GTK 4, Rust) that speaks only the new protocols:
JMAP for mail, Sieve for server-side filters, and OpenPGP built right into the
app.** The desktop sibling of [jmailAPK](https://github.com/th3rumbl3m4t0r/jmailAPK),
the Android app.

- **JMAP only** (RFC 8620 / 8621). No IMAP, no POP3, no SMTP: sync (with JMAP
  push), flags, folders, drafts and sending all go over JMAP on HTTPS.
- **Sieve** filters over JMAP Sieve (RFC 9661): an editor with highlighting,
  validation on the server, activate / deactivate, and a rule builder (also
  "filter rule from this" on any message).
- **PGP built in**: Sequoia inside the program, no gpg agent or plugin in
  between. Reads PGP/MIME, inline PGP and GnuPG 2.4's OCB packets, encrypts
  and signs what you send, and reads mail a server keeps encrypted at rest.

Besides mail: calendars over CalDAV (JMAP Calendars is not used yet), several
accounts, and every message kept locally, decrypted, so all of it can be
searched on all cores. Developed and tested against
[Stalwart](https://stalw.art/) 0.13; other JMAP servers should work for mail,
but have not been tested. Built for Rocky Linux 10 (GTK 4.16); any distribution
with GTK >= 4.16 and GtkSourceView 5 should do.

License: **GPL-3.0-or-later** (see [License](#license)).

## What it does

- **accounts**: any number, each its own server and user (settings → accounts). Folders of
  all of them in one tree, plus "all inboxes" and "flagged".
- **sync**: the whole mailbox. The first sync lists every message (Email/query) and fetches
  the headers; then every message is downloaded with several `Blob/get` batches at once (as
  many as the server's `maxConcurrentRequests` allows, one kept free), decrypted and taken
  apart on all cores (rayon), and stored in one transaction per batch. After that
  `Email/changes` since the state we hold (a state too old to replay starts over). New mail:
  the JMAP event source wakes the sync at once; without working push (the 0.13 test server
  sends nothing on it, nor on the WebSocket) a light poll asks for the Email/Mailbox states
  every 30 s (settings → general) and syncs only on a change.
- **storage**: `~/.local/share/maildot/` — `maildot.db` (SQLite, WAL: accounts, folders,
  headers, flags, the search text, calendars, keys, settings) and `mail/<account>/<nnn>/<id>.eml`,
  one plain RFC 5322 file per message, **decrypted** (PGP/MIME from the server's
  encryption at rest is opened with your key, the outer headers put back on the decrypted
  entity). Unencrypted on purpose: keep the disk encrypted. Mail that cannot be decrypted
  yet (key locked or missing) is kept as it came and decrypted when the key is unlocked.
- **search** (the search tab, or the filter box over any folder): local, over every stored
  message, on all cores. Conditions, all of which must hold, each with a pattern and toggles:
  `regex` (else plain text), `Aa` (case sensitive), `word` (whole words), `not` (must not
  match), and the fields `subject`, `from` (from, sender, reply-to), `to/cc` (to, cc, bcc),
  `headers` (every header line, `Name: value`), `body` (text parts, HTML as text, forwarded
  messages), `attachments` (names, types and the text of text attachments). Scope: account,
  folder, since / before, unread, flagged, has attachments. Hits stream in, then sort newest
  first, with the match marked in a snippet and the count; the reader highlights every match.
  Plain text is an escaped regex (Unicode case folding: `ŽLUŤOUČKÝ` finds `žluťoučký`); the
  `regex` crate runs in linear time, so no pattern can hang it.
- **reading**: headers, the PGP line (`encrypted · signed by …`, yellow when the signer is
  unknown or not the sender, red for a bad signature), attachments (`open`, `save`, `shelf`),
  `headers`, `source`, `save as .eml`, `filter rule from this`. HTML mail is laid out as
  formatted text by our own renderer (bold, italic, links, headings, quotes, lists, tables
  as lines, `cid:` and `data:` images): no web engine, no scripts, nothing from the network
  until `load online content` (once, for that message), or always for the sender domains
  listed in settings → html mail (added there only, not from the reader). `open in
  browser` writes a copy with a CSP that blocks scripts and remote content (unless allowed)
  and shows it in the desktop's browser. Junk shows text first.
- **writing**: a window per message. `from` is free; the ▾ next to it lists the account's
  favourites on top (they stay put while the long list scrolls; ☆ / ★ on each row, and a
  button to star the address typed in the field), then every address of the account
  (identities, and catch-all addresses:
  an address that was the only recipient of a received message is yours, and so is its
  domain; more domains under accounts → my domains); a reply goes out from the To / Cc
  address the message came to, with your name when it is a real name and the alias itself
  otherwise. Stalwart wants the envelope sender to be an identity, so mail from an alias goes
  through the account's identity with the alias in `From` (or an identity for the exact
  address when the server lets us make one). `encrypt` is on when every recipient has a key,
  `sign` when you have one; the line under them says what will happen. Address completion,
  attachments from files, by drag and drop, or from shelf; drafts (encrypted to your own key
  when you have one); `mailto:` links (the desktop file registers the handler). Ctrl+Enter sends.
- **Sieve** (settings → filters, or `filter rule from this`): the account's scripts (one
  active), an editor with line numbers and highlighting, validation on the server (the
  cursor goes to the line it complains about), save, activate, deactivate, delete, and a rule
  builder (from / to / subject / header / body; contains / is / matches / regex; move, copy,
  mark read, flag, discard, redirect; and mark read, then stop) that appends an `if` block
  with one `require` line kept in step.
- **PGP** (settings → keys): Sequoia (OpenSSL backend). Import your private key (file or
  paste) and other people's public keys. Reads PGP/MIME, inline PGP, GnuPG 2.4's AEAD (OCB)
  packets, `multipart/signed`; writes AES-256 SEIPD, compressed, signed with SHA-256, or
  detached `multipart/signed`. The passphrase stays in memory for the session, or in the
  desktop keyring with "remember".
- **calendar**: CalDAV: month, week (a time grid, overlapping
  events side by side) and agenda (60 days); new event by `new event` or a double click;
  details on click with `edit`, `skip this one` (EXDATE), `delete` (a second click says
  "sure?"). Our own calendars, from every account, share one pane; **each calendar shared
  with us is a pane of its own**, switched on and off next to its name ("shared with me"),
  so shared calendars never overlap ours or each other. `share` next to one of ours gives a
  user of the server read or read & write (WebDAV ACL). Events keep every property other
  clients wrote (our own iCalendar parser and writer); recurrences go through the `rrule`
  crate in the event's own zone (DST-safe); expansion runs on all cores. Reminders are
  desktop notifications while the program runs.
- **shelf** (settings → shelf), for [shelf](https://github.com/th3rumbl3m4t0r/shelf)
  users: a shelf folder per API key (shelf: share → API keys, "can
  edit" to save). `shelf` on an attachment, `all to shelf`, `message to shelf` (as
  `YYYY-MM-DD subject.eml`) and `from shelf` in the editor. A name in use gets ` (2)`.
- **look**: the x11 look: night / day, one accent,
  square, monospace, nothing animates. Pop-up windows and popovers have a 1px outline in
  the accent (the border colour when another window is in front); the main window's is
  the border colour.
- **background**: settings → general: keep running when the window is closed, start at
  login (an autostart entry with `--background`). One instance: a second start (or a
  `mailto:` link) goes to the running one.

Passwords and shelf keys go to the desktop keyring (`secret-tool`, libsecret) when there is
one, else to `secrets.json` (0600) in the data folder; settings → accounts says which.

## Layout

    core/                 maildot-core: everything but the window
      src/engine.rs       accounts, their loops, actions, open, send / drafts, keys, shelf places
      src/sync.rs         mail sync: full / changes, parallel download + decrypt, push, light poll
      src/jmap.rs         JMAP client (session, methods, Blob/get, upload, submission, Sieve, event source)
      src/db.rs           SQLite schema and queries
      src/search.rs       the parallel search (rayon over id ranges, one read-only connection each)
      src/mime.rs         parsing (mail-parser), PGP/MIME in, our MIME writer out, the search text
      src/pgp.rs          Sequoia: keys, decrypt + verify, encrypt + sign, detached signatures
      src/html.rs         HTML → styled runs (and text); the sandboxed copy for the browser
      src/senders.rs      which addresses are ours, the reply-from rule
      src/sieve.rs        the rule → Sieve generator (require merging)
      src/caldav.rs       CalDAV (discovery, shared homes, sync-collection, multiget, PUT/DELETE, ACL)
      src/ical.rs         iCalendar parse / expand / write (keeps unknown properties)
      src/cal.rs          calendar sync, events table, instances, edits, reminders
      src/shelf.rs        shelf's /api/v1 client
      src/secrets.rs      keyring or file
      tests/pgp_mime.rs   PGP / MIME against gpg-made test keys and messages
      examples/live.rs    end-to-end against the test Stalwart (see Tests)
      examples/bench_search.rs   search speed on a synthetic store
    app/                  maildot: the GTK 4 window
      src/ui.rs           window, top bar, status bar, actions and shortcuts, notifications
      src/mail.rs         folders, message list (ListView), quick filter, move menu
      src/reader.rs       one message, the HTML layout, highlights
      src/compose.rs      the editor
      src/searchview.rs   the search tab
      src/calview.rs      month / week / agenda, panes, event editor, sharing
      src/settings.rs     settings
      src/sieveview.rs    Sieve editor and rule builder (GtkSourceView)
      src/shelfui.rs      shelf browser (save / pick)
      src/debug.rs        the test hook (MAILDOT_DEBUG_SOCK)
      src/style.rs        the x11 look as GTK CSS
    packaging/            spec, desktop file, icon, metainfo, build-rpm.sh
    tests/headless.sh     the window in a headless mutter, for scripted checks (tests/dbg.py)

## Build

    cargo build --release            # target/release/maildot
    cargo test -p maildot-core       # unit tests + PGP/MIME tests

Needs Rust (stable), a C compiler, and the GTK 4, GtkSourceView 5 and OpenSSL
headers: on Rocky / RHEL 10 `dnf --enablerepo=crb install rust cargo gcc
gtk4-devel gtksourceview5-devel openssl-devel` (gtksourceview5-devel is in CRB),
on Fedora the same without the repo flag. SQLite is built in (rusqlite bundled).
At run time: gtk4, gtksourceview5, libsecret, a monospace font (DejaVu Sans Mono).

RPM: `packaging/build-rpm.sh` (needs `rpm-build`) builds the release binary and
`target/rpm/maildot-<version>-1.<dist>.x86_64.rpm`, which installs the binary,
the desktop file (it registers the `mailto:` handler), the icon and the
AppStream metadata.

## Tests

- `cargo test -p maildot-core`: HTML layout, search (plain / case / word / regex / Czech
  case folding / snippets), Sieve generation, iCalendar (weekly with EXDATE and a moved
  instance, DST, all-day with a date UNTIL, writing keeps unknown properties, EXDATE),
  multistatus / ACL parsing, PGP and MIME (decrypting GnuPG 2.4 OCB mail and checking
  Bob's signature, wrong key, signed-only, round trips).
- `MAILDOT_HOME=/tmp/maildot-live cargo run -p maildot-core --example live` against a local
  test Stalwart (HTTP on 127.0.0.1:8090, SMTP on 2525 without auth; users alice / alicepw
  with encryption at rest to the test key in `core/tests/data` and a catch-all
  `@example.org`, and bob / bobpw): two accounts in one engine; first sync;
  mail delivered over SMTP arrives (push or poll) and is stored decrypted; search in all
  its modes; mail to the catch-all is answered from that alias and reaches bob with the
  attachment and In-Reply-To, the original gets `$answered`, the copy is in Sent;
  encrypted + signed mail to bob is decrypted and checked; drafts; flag / read / move /
  trash / delete agree with the server; Sieve validate, save, activate, a real message
  filed by the rule, the old script restored; calendar create, skip one, edit, delete on
  the server; alice shares her calendar with bob, who sees it read only with its events;
  an unknown user is refused. It ends with `ALL OK`.
- `cargo run --release -p maildot-core --example bench_search -- /tmp/maildot-bench 100000`:
  100,000 messages (425 MB store) searched in 110–160 ms on 5 cores (2.5–3.3× one thread).
- The window: `tests/headless.sh start` runs it in a headless mutter with a debug socket;
  `tests/dbg.py 'tree' 'select messages 0' 'shot /tmp/x.png' …` drives it (widget names are
  in the code: `messages`, `filter`, `s-pattern0`, `c-send`, `cal-week`, …).

## Not there yet

- conversations (threads), contacts (CardDAV), JMAP calendars (CalDAV is used), S/MIME,
  key generation, Autocrypt / WKD
- signed packages / a package repository
- push on Stalwart 0.13 (the 30 s poll covers it); untested against newer versions' push

## License

Copyright (C) 2026 th3rumbl3m4t0r

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later
version. It is distributed in the hope that it will be useful, but WITHOUT ANY
WARRANTY; see [LICENSE](LICENSE) for the full text.

All Rust dependencies (see `Cargo.lock`) are under GPLv3-compatible licenses:
mostly MIT and/or Apache-2.0, plus BSD, ISC, Zlib, BSL-1.0, Unicode-3.0, CC0,
MPL-2.0 (`cssparser`, `selectors`) and LGPL-2.0-or-later (`sequoia-openpgp`,
`buffered-reader`). The system libraries it links (GTK 4, GtkSourceView 5,
GLib, libsecret: LGPL; OpenSSL 3: Apache-2.0) are compatible as well.
