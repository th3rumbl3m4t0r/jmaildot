//! Settings: accounts, keys, filters, html mail, notifications, calendar, shelf, look, general.

use crate::util::*;
use crate::{sieveview, style};
use gtk::prelude::*;
use gtk::glib;
use maildot_core::{paths, pgp, secrets};
use std::rc::Rc;

thread_local! {
    static WIN: std::cell::RefCell<Option<(gtk::Window, gtk::Stack)>> = const { std::cell::RefCell::new(None) };
}

pub fn open(page: Option<&str>) {
    if let Some((w, st)) = WIN.with(|x| x.borrow().clone()) {
        if let Some(p) = page {
            st.set_visible_child_name(p);
        }
        w.present();
        return;
    }
    let w = gtk::Window::builder().title("settings").default_width(940).default_height(680).build();
    w.set_widget_name("settings");
    if let Some(u) = crate::ui::try_ui() {
        w.set_transient_for(Some(&u.win));
    }
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::None);
    let side = gtk::StackSidebar::new();
    side.set_stack(&stack);
    side.set_size_request(160, -1);
    for (name, title, build) in [
        ("accounts", "accounts", accounts_page as fn() -> gtk::Widget),
        ("keys", "keys (pgp)", keys_page),
        ("filters", "filters (sieve)", filters_page),
        ("html", "html mail", html_page),
        ("notify", "notifications", notify_page),
        ("calendar", "calendar", calendar_page),
        ("shelf", "shelf", shelf_page),
        ("look", "look", look_page),
        ("general", "general", general_page),
    ] {
        let p = build();
        stack.add_titled(&p, Some(name), title);
    }
    let root = hbox(0);
    root.append(&side);
    root.append(&gtk::Separator::new(gtk::Orientation::Vertical));
    stack.set_hexpand(true);
    root.append(&stack);
    w.set_child(Some(&root));
    if let Some(p) = page {
        stack.set_visible_child_name(p);
    }
    WIN.with(|x| *x.borrow_mut() = Some((w.clone(), stack.clone())));
    w.connect_close_request(|_| {
        WIN.with(|x| *x.borrow_mut() = None);
        glib::Propagation::Proceed
    });
    w.present();
}

fn rebuild(page: &str) {
    if let Some((_, st)) = WIN.with(|x| x.borrow().clone()) {
        if let Some(old) = st.child_by_name(page) {
            let title = st.page(&old).title().map(|t| t.to_string()).unwrap_or_default();
            let visible = st.visible_child_name().as_deref() == Some(page);
            st.remove(&old);
            let w = match page {
                "accounts" => accounts_page(),
                "keys" => keys_page(),
                "filters" => filters_page(),
                "html" => html_page(),
                "notify" => notify_page(),
                "shelf" => shelf_page(),
                "general" => general_page(),
                _ => return,
            };
            st.add_titled(&w, Some(page), &title);
            // keep the order of the sidebar: move it back where it was
            if visible {
                st.set_visible_child_name(page);
            }
        }
    }
}

fn page(title: &str) -> (gtk::ScrolledWindow, gtk::Box) {
    let b = vbox(8);
    b.set_margin_top(10);
    b.set_margin_bottom(10);
    b.set_margin_start(14);
    b.set_margin_end(14);
    let (tb, _) = titlebar(title);
    b.append(&tb);
    let s = scrolled(&b);
    (s, b)
}

fn section(b: &gtk::Box, title: &str) {
    let l = lbl(title);
    l.set_margin_top(10);
    b.append(&l);
}

fn note(b: &gtk::Box, text: &str) {
    let l = label_c(text, &["dim", "small"]);
    l.set_wrap(true);
    l.set_max_width_chars(100);
    b.append(&l);
}

fn check(b: &gtk::Box, text: &str, key: &str, default: bool, after: impl Fn(bool) + 'static) {
    let c = gtk::CheckButton::with_label(text);
    c.set_active(eng().db.flag(key, default));
    let k = key.to_string();
    c.connect_toggled(move |c| {
        let _ = eng().db.set_flag(&k, c.is_active());
        after(c.is_active());
    });
    b.append(&c);
}

fn after_accounts_change() {
    rebuild("accounts");
    rebuild("filters");
    if let Some(u) = crate::ui::try_ui() {
        u.mail.reload_folders();
        u.search.accounts_changed();
        u.cal.schedule_refresh();
        u.refresh_status();
    }
}

// ---- accounts ----

fn accounts_page() -> gtk::Widget {
    let (s, b) = page("accounts");
    let accounts = eng().accounts();
    let st = eng().statuses();
    if accounts.is_empty() {
        note(&b, "No account yet. Add your mail server below (Stalwart, or any server with JMAP). Calendars come from the same server over CalDAV.");
    }
    for a in &accounts {
        let frame = vbox(4);
        frame.add_css_class("win");
        let (tb, _) = titlebar(&a.name);
        frame.append(&tb);
        let inner = vbox(4);
        inner.set_margin_start(8);
        inner.set_margin_end(8);
        inner.set_margin_bottom(6);
        let (stext, bad) = st.get(&a.id).cloned().unwrap_or_default();
        let (n, stored) = eng().db.counts(a.id);
        let l = label(&format!("{} on {} · {} · {stored} of {n} messages stored", a.user, a.server, if stext.is_empty() { "not synced yet" } else { &stext }));
        l.set_wrap(true);
        if bad {
            l.add_css_class("bad");
        }
        inner.append(&l);
        inner.append(&label_c(&format!("password kept in: {}", secrets::where_stored(&maildot_core::engine::password_key(a.id))), &["dim", "small"]));
        let g = gtk::Grid::new();
        g.set_row_spacing(4);
        g.set_column_spacing(8);
        let name = entry("what to call it");
        name.set_text(&a.name);
        let disp = entry("your name on mail you send");
        disp.set_text(a.display_name.as_deref().unwrap_or(""));
        let doms = entry("example.org shop.example.com");
        doms.set_text(&a.sender_domains.join(" "));
        for (r, (t, e)) in [("name", &name), ("sender name", &disp), ("my domains", &doms)].iter().enumerate() {
            let l = lbl(t);
            l.set_xalign(1.0);
            g.attach(&l, 0, r as i32, 1, 1);
            e.set_hexpand(true);
            g.attach(*e, 1, r as i32, 1, 1);
        }
        inner.append(&g);
        note(&inner, "my domains: catch-all domains of yours. Addresses mail was sent to are learned by themselves (an address that was the only recipient of a message is yours, and so is its domain); replies go out from the address the message came to.");
        let ours = eng().our_addresses(a.id);
        let ol = label_c(&format!("addresses known as yours: {}", ours.iter().map(|x| x.email.clone()).collect::<Vec<_>>().join(", ")), &["dim", "small"]);
        ol.set_wrap(true);
        ol.set_selectable(true);
        inner.append(&ol);
        let row = hbox(6);
        let save = button_c("save", &["primary", "small"]);
        let pass = button_c("password…", &["small"]);
        let sync = button_c("sync now", &["small"]);
        let folders = button_c("notify for folders…", &["small"]);
        let rm = button_c("remove", &["small", "danger"]);
        row.append(&save);
        row.append(&pass);
        row.append(&sync);
        row.append(&folders);
        row.append(&spacer());
        row.append(&rm);
        inner.append(&row);
        frame.append(&inner);
        b.append(&frame);
        let id = a.id;
        let acc = a.clone();
        save.connect_clicked(move |_| {
            let mut x = acc.clone();
            x.name = name.text().trim().to_string();
            if x.name.is_empty() {
                x.name = x.email.clone();
            }
            x.display_name = Some(disp.text().trim().to_string()).filter(|s| !s.is_empty());
            x.sender_domains = doms.text().split([' ', ',']).map(|d| d.trim().trim_start_matches('@').to_lowercase()).filter(|d| !d.is_empty()).collect();
            match eng().save_account(&x) {
                Ok(()) => {
                    toast("saved");
                    after_accounts_change();
                }
                Err(e) => toast_err(&err_text(&e)),
            }
        });
        pass.connect_clicked(move |_| {
            prompt("password", "The account's password (or an app password):", "", true, move |p| {
                let Some(p) = p else { return };
                run(async move { eng().set_password(id, &p).await }, |r| match r {
                    Ok(()) => {
                        toast("password changed; syncing");
                        after_accounts_change();
                    }
                    Err(e) => toast_err(&err_text(&e)),
                });
            });
        });
        sync.connect_clicked(move |_| {
            eng().sync_now(Some(id));
            toast("syncing");
        });
        folders.connect_clicked(move |_| notify_folders(id));
        let nm = a.name.clone();
        rm.connect_clicked(move |_| {
            ask("remove account", &format!("Remove {nm} from this computer? Its stored messages are deleted here (not on the server)."), &["cancel", "remove"], move |i| {
                if i == Some(1) {
                    match eng().remove_account(id) {
                        Ok(()) => after_accounts_change(),
                        Err(e) => toast_err(&err_text(&e)),
                    }
                }
            });
        });
    }
    section(&b, "add an account");
    let g = gtk::Grid::new();
    g.set_row_spacing(4);
    g.set_column_spacing(8);
    let server = named(entry("mail.example.com or https://mail.example.com"), "acc-server");
    let user = named(entry("user name (often the address)"), "acc-user");
    let pass = named(entry("password or app password"), "acc-pass");
    pass.set_visibility(false);
    for (r, (t, e)) in [("server", &server), ("user", &user), ("password", &pass)].iter().enumerate() {
        let l = lbl(t);
        l.set_xalign(1.0);
        g.attach(&l, 0, r as i32, 1, 1);
        e.set_hexpand(true);
        g.attach(*e, 1, r as i32, 1, 1);
    }
    b.append(&g);
    let add = named(button_c("sign in", &["primary"]), "acc-add");
    add.set_halign(gtk::Align::Start);
    let msg = label_c("", &["small"]);
    msg.set_wrap(true);
    b.append(&add);
    b.append(&msg);
    note(&b, "The whole mailbox is downloaded and kept here, decrypted, as one .eml file per message plus an SQLite index (see general), so everything is searchable offline. Encrypted-at-rest mail needs your PGP key (keys).");
    {
        let pass2 = pass.clone();
        add.connect_clicked(move |b| {
            let (s, u, p) = (server.text().to_string(), user.text().trim().to_string(), pass2.text().to_string());
            if s.trim().is_empty() || u.is_empty() || p.is_empty() {
                msg.set_text("fill in all three");
                return;
            }
            b.set_sensitive(false);
            msg.set_text("signing in…");
            let msg2 = msg.clone();
            let b2 = b.clone();
            run(async move { eng().add_account(&s, &u, &p).await }, move |r| {
                b2.set_sensitive(true);
                match r {
                    Ok(_) => {
                        msg2.set_text("added; the first sync runs now");
                        after_accounts_change();
                    }
                    Err(e) => msg2.set_text(&err_text(&e)),
                }
            });
        });
        let add2 = add.clone();
        pass.connect_activate(move |_| add2.emit_clicked());
    }
    s.upcast()
}

fn notify_folders(account: i64) {
    let w = gtk::Window::builder().title("notify for").default_width(360).default_height(420).build();
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let b = vbox(4);
    b.set_margin_top(8);
    b.set_margin_start(10);
    b.set_margin_end(10);
    note(&b, "New mail in these folders makes a desktop notification (inbox only unless changed).");
    for (m, depth) in crate::mail::tree(&eng().db.mailboxes(account)) {
        let c = gtk::CheckButton::with_label(&m.name.to_lowercase());
        c.set_margin_start((depth * 14) as i32);
        c.set_active(m.notifies());
        let id = m.id.clone();
        c.connect_toggled(move |c| {
            let _ = eng().db.set_notify(account, &id, c.is_active());
        });
        b.append(&c);
    }
    w.set_child(Some(&scrolled(&b)));
    w.present();
}

// ---- keys ----

fn keys_page() -> gtk::Widget {
    let (s, b) = page("keys (openpgp)");
    note(&b, "Your private key decrypts mail (also what the server encrypts at rest) and signs; other people's public keys encrypt to them and check their signatures. Import the key you gave the server. Keys are kept in the local database; a secret key stays protected by its passphrase.");
    let infos = eng().key_infos();
    if infos.is_empty() {
        b.append(&label_c("no keys", &["dim"]));
    }
    for k in &infos {
        let fr = vbox(2);
        fr.add_css_class("win");
        let top = hbox(6);
        let t = label(&format!("{}{}", if k.secret { "YOURS · " } else { "" }, k.user_ids.join(", ")));
        t.set_hexpand(true);
        t.set_wrap(true);
        if k.secret {
            t.add_css_class("accent");
        }
        top.append(&t);
        top.set_margin_start(6);
        top.set_margin_end(6);
        top.set_margin_top(4);
        fr.append(&top);
        let d = label_c(
            &format!(
                "{} · {} · created {}{}{}",
                k.pretty_fpr(),
                k.algorithm,
                short_date(k.created),
                k.expires.map(|e| format!(" · expires {}", short_date(e))).unwrap_or_default(),
                if k.expired() { " · EXPIRED" } else { "" }
            ),
            &["dim", "small"],
        );
        d.set_selectable(true);
        d.set_wrap(true);
        d.set_margin_start(6);
        fr.append(&d);
        let row = hbox(6);
        row.set_margin_start(6);
        row.set_margin_bottom(4);
        let copy = button_c("copy public key", &["small"]);
        let rm = button_c("remove", &["small", "danger"]);
        row.append(&copy);
        row.append(&spacer());
        row.append(&rm);
        fr.append(&row);
        let fpr = k.fingerprint.clone();
        copy.connect_clicked(move |_| {
            let ring = eng().keys.read().unwrap();
            if let Some(c) = ring.certs.iter().find(|c| c.fingerprint().to_hex() == fpr) {
                if let Ok(a) = pgp::public_armored(c) {
                    copy_text(&a);
                    toast("public key copied");
                }
            }
        });
        let fpr = k.fingerprint.clone();
        let secret = k.secret;
        rm.connect_clicked(move |_| {
            let fpr = fpr.clone();
            ask("remove key", if secret { "Remove YOUR private key from this computer? Encrypted mail cannot be read here without it." } else { "Remove this public key?" }, &["cancel", "remove"], move |i| {
                if i == Some(1) {
                    let _ = eng().remove_key(&fpr);
                    rebuild("keys");
                }
            });
        });
        b.append(&fr);
    }
    section(&b, "import");
    let row = hbox(6);
    let from_file = named(button("from a file…"), "key-file");
    let paste = named(button("paste…"), "key-paste");
    row.append(&from_file);
    row.append(&paste);
    b.append(&row);
    from_file.connect_clicked(|_| {
        pick_files("key file", |paths| {
            for p in paths {
                match std::fs::read(&p) {
                    Ok(d) => import(&d),
                    Err(e) => toast_err(&format!("cannot read: {e}")),
                }
            }
        });
    });
    paste.connect_clicked(|_| {
        let w = gtk::Window::builder().title("paste a key").default_width(640).default_height(420).build();
        if let Some(p) = parent() {
            w.set_transient_for(Some(&p));
        }
        let bx = vbox(6);
        bx.set_margin_top(8);
        bx.set_margin_bottom(8);
        bx.set_margin_start(8);
        bx.set_margin_end(8);
        let tv = gtk::TextView::new();
        tv.add_css_class("edit");
        tv.set_monospace(true);
        bx.append(&scrolled(&tv));
        let ok = button_c("import", &["primary"]);
        ok.set_halign(gtk::Align::End);
        bx.append(&ok);
        w.set_child(Some(&bx));
        let w2 = w.clone();
        ok.connect_clicked(move |_| {
            let buf = tv.buffer();
            let (s, e) = buf.bounds();
            import(buf.text(&s, &e, false).as_bytes());
            w2.close();
        });
        w.present();
    });
    section(&b, "passphrase");
    let locked = eng().locked();
    b.append(&label(if locked { "your key is locked" } else if infos.iter().any(|k| k.secret) { "your key is open for this session" } else { "no private key" }));
    let row = hbox(6);
    let unlock = button_c("unlock", &["small"]);
    unlock.set_sensitive(locked);
    let forget = button_c("forget the passphrase", &["small"]);
    row.append(&unlock);
    row.append(&forget);
    b.append(&row);
    unlock.connect_clicked(|_| crate::ui::ask_passphrase(|| rebuild("keys")));
    forget.connect_clicked(|_| {
        eng().keys.write().unwrap().forget_passwords();
        secrets::remove("pgp-passphrase");
        let _ = eng().db.set_flag("pgp.remember", false);
        toast("forgotten");
        rebuild("keys");
    });
    check(&b, "remember the passphrase (in the desktop keyring)", "pgp.remember", false, |on| {
        if !on {
            secrets::remove("pgp-passphrase");
        } else {
            toast("it is remembered the next time you unlock");
        }
    });
    check(&b, "sign what I send when I have a key", "pgp.sign", true, |_| {});
    s.upcast()
}

fn import(data: &[u8]) {
    match eng().import_keys(data) {
        Ok(list) => {
            toast(&format!("imported {}", list.iter().map(|k| k.user_ids.first().cloned().unwrap_or_else(|| k.fingerprint.clone())).collect::<Vec<_>>().join(", ")));
            rebuild("keys");
            if list.iter().any(|k| k.secret) && eng().locked() {
                crate::ui::ask_passphrase(|| rebuild("keys"));
            }
        }
        Err(e) => toast_err(&format!("not imported: {}", err_text(&e))),
    }
}

// ---- filters ----

fn filters_page() -> gtk::Widget {
    let (s, b) = page("filters (sieve)");
    note(&b, "Sieve scripts run on the server when mail arrives (RFC 5228; managed over JMAP, RFC 9661). One script is active per account. The editor validates on the server and has a rule builder.");
    for a in eng().accounts() {
        let row = hbox(6);
        let l = label(&a.name);
        l.set_hexpand(true);
        row.append(&l);
        let open = named(button_c("edit filters", &["small"]), &format!("sieve-{}", a.id));
        let id = a.id;
        open.connect_clicked(move |_| sieveview::open(id));
        row.append(&open);
        b.append(&row);
    }
    s.upcast()
}

// ---- html ----

fn html_page() -> gtk::Widget {
    let (s, b) = page("html mail");
    note(&b, "HTML mail is laid out here as formatted text: no scripts, no web engine, nothing loads from the network until you press \"load online content\" (or the sender's domain is listed below). \"open in browser\" shows the exact layout in a locked-down copy.");
    let row = hbox(6);
    row.append(&label("show"));
    let dd = dropdown(&["html when there is html", "text"], if eng().db.setting_or("html.default", "html") == "html" { 0 } else { 1 });
    dd.connect_selected_notify(|d| {
        let _ = eng().db.set_setting("html.default", if d.selected() == 0 { "html" } else { "text" });
    });
    row.append(&dd);
    b.append(&row);
    note(&b, "The junk folder always shows text first.");
    check(&b, "mark messages read when opened", "read.mark", true, |_| {});
    check(&b, "show a preview line in the lists", "list.preview", true, |_| {
        if let Some(u) = crate::ui::try_ui() {
            u.mail.reload_list();
        }
    });
    section(&b, "online content always loads from");
    let list: Vec<String> = eng().db.setting_or("html.allow", "").split_whitespace().map(String::from).collect();
    if list.is_empty() {
        b.append(&label_c("no domains", &["dim"]));
    }
    for d in list {
        let row = hbox(6);
        let l = label(&d);
        l.set_hexpand(true);
        row.append(&l);
        let rm = button_c("remove", &["small"]);
        let d2 = d.clone();
        rm.connect_clicked(move |_| {
            let rest: Vec<String> = eng().db.setting_or("html.allow", "").split_whitespace().filter(|x| *x != d2).map(String::from).collect();
            let _ = eng().db.set_setting("html.allow", &rest.join(" "));
            rebuild("html");
        });
        row.append(&rm);
        b.append(&row);
    }
    let row = hbox(6);
    let dom = named(entry("example.com"), "html-allow-domain");
    dom.set_hexpand(true);
    let add = named(button_c("add", &["small"]), "html-allow-add");
    {
        let a2 = add.clone();
        dom.connect_activate(move |_| a2.emit_clicked());
    }
    row.append(&dom);
    row.append(&add);
    b.append(&row);
    note(&b, "Mail from a sender at that domain (and only that exact domain) loads its images and other online content when opened. The reader itself only offers loading it once.");
    let go = move || {
        let d = dom.text().trim().trim_start_matches('@').to_lowercase();
        let d = d.rsplit('@').next().unwrap_or("").to_string();
        if d.is_empty() || !d.contains('.') || d.contains(char::is_whitespace) {
            toast_err("a domain like example.com");
            return;
        }
        let mut list: Vec<String> = eng().db.setting_or("html.allow", "").split_whitespace().map(String::from).collect();
        if !list.contains(&d) {
            list.push(d);
        }
        let _ = eng().db.set_setting("html.allow", &list.join(" "));
        rebuild("html");
    };
    let go = Rc::new(go);
    {
        let g = go.clone();
        add.connect_clicked(move |_| g());
    }
    s.upcast()
}

fn notify_page() -> gtk::Widget {
    let (s, b) = page("notifications");
    check(&b, "desktop notification for new mail", "notify.mail", true, |_| {});
    check(&b, "only while the window is not in front", "notify.only_hidden", false, |_| {});
    note(&b, "Which folders count is set per account (accounts → notify for folders); the inbox alone by default, so junk stays quiet. Calendar reminders: see calendar.");
    s.upcast()
}

fn calendar_page() -> gtk::Widget {
    let (s, b) = page("calendar");
    let row = hbox(6);
    row.append(&label("weeks start on"));
    let dd = dropdown(&["monday", "sunday"], if eng().db.setting_or("cal.week", "monday") == "monday" { 0 } else { 1 });
    dd.connect_selected_notify(|d| {
        let _ = eng().db.set_setting("cal.week", if d.selected() == 0 { "monday" } else { "sunday" });
        if let Some(u) = crate::ui::try_ui() {
            u.cal.refresh();
        }
    });
    row.append(&dd);
    b.append(&row);
    check(&b, "reminders (a desktop notification at the reminder time, while the program runs)", "cal.reminders", true, |_| {});
    let row = hbox(6);
    row.append(&label("new events get a reminder"));
    let vals: [Option<i32>; 6] = [None, Some(0), Some(10), Some(30), Some(60), Some(1440)];
    let cur: Option<i32> = eng().db.setting("cal.reminder").and_then(|v| v.parse().ok());
    let dd = dropdown(&["none", "at the time", "10 min before", "30 min before", "1 hour before", "1 day before"], vals.iter().position(|v| *v == cur).unwrap_or(0) as u32);
    dd.connect_selected_notify(move |d| {
        match vals[d.selected() as usize] {
            Some(v) => {
                let _ = eng().db.set_setting("cal.reminder", &v.to_string());
            }
            None => {
                let _ = eng().db.w().execute("DELETE FROM settings WHERE key = 'cal.reminder'", []);
            }
        }
    });
    row.append(&dd);
    b.append(&row);
    note(&b, "Your own calendars show together; each calendar shared with you is a pane of its own, switched on and off in the calendar's sidebar. \"share\" next to one of yours gives other users of the server read or read & write access.");
    s.upcast()
}

// ---- shelf ----

fn shelf_page() -> gtk::Widget {
    let (s, b) = page("shelf");
    note(&b, "shelf folders for attachments and messages: in shelf, open share on a folder, create an API key under \"API keys\" (tick \"can edit\" to save here), and paste it below. Each key gives one folder.");
    let places = eng().shelves();
    if places.is_empty() {
        b.append(&label_c("no shelf folders", &["dim"]));
    }
    for (id, lab, url) in places {
        let row = hbox(6);
        let l = label(&format!("{lab}   {url}"));
        l.set_hexpand(true);
        l.set_wrap(true);
        row.append(&l);
        let rm = button_c("remove", &["small", "danger"]);
        rm.connect_clicked(move |_| {
            let _ = eng().remove_shelf(&id);
            rebuild("shelf");
        });
        row.append(&rm);
        b.append(&row);
    }
    section(&b, "add");
    let url = named(entry("https://shelf.example.com"), "shelf-url");
    let key = named(entry("shk_…"), "shelf-key");
    key.set_visibility(false);
    b.append(&url);
    b.append(&key);
    let add = named(button_c("check and add", &["primary"]), "shelf-add");
    add.set_halign(gtk::Align::Start);
    let msg = label_c("", &["small"]);
    b.append(&add);
    b.append(&msg);
    add.connect_clicked(move |bt| {
        let (u, k) = (url.text().to_string(), key.text().trim().to_string());
        bt.set_sensitive(false);
        msg.set_text("checking…");
        let msg2 = msg.clone();
        let bt2 = bt.clone();
        run(async move { eng().add_shelf(&u, &k).await }, move |r| {
            bt2.set_sensitive(true);
            match r {
                Ok(l) => {
                    msg2.set_text(&format!("added: {l}"));
                    rebuild("shelf");
                }
                Err(e) => msg2.set_text(&err_text(&e)),
            }
        });
    });
    s.upcast()
}

// ---- look ----

fn look_page() -> gtk::Widget {
    let (s, b) = page("look");
    let row = hbox(6);
    let night = gtk::ToggleButton::with_label("night");
    let day = gtk::ToggleButton::with_label("day");
    day.set_group(Some(&night));
    night.add_css_class("tag");
    day.add_css_class("tag");
    if style::night() {
        night.set_active(true);
    } else {
        day.set_active(true);
    }
    night.connect_toggled(|t| {
        let _ = eng().db.set_setting("look.theme", if t.is_active() { "night" } else { "day" });
        style::apply();
        if let Some(u) = crate::ui::try_ui() {
            u.restyled();
        }
    });
    row.append(&night);
    row.append(&day);
    b.append(&row);
    section(&b, "accent");
    let row = hbox(4);
    let hex = entry("#3b8a3e");
    hex.set_text(&style::accent());
    hex.set_width_chars(9);
    let set = |c: &str| {
        if style::valid_hex(c) {
            let _ = eng().db.set_setting("look.accent", c);
            style::apply();
            if let Some(u) = crate::ui::try_ui() {
                u.restyled();
            }
        }
    };
    for c in style::ACCENTS {
        let bt = gtk::Button::new();
        let l = gtk::Label::new(None);
        l.set_markup(&format!("<span foreground=\"{c}\">████</span>"));
        bt.set_child(Some(&l));
        bt.set_tooltip_text(Some(c));
        let h = hex.clone();
        let c = c.to_string();
        bt.connect_clicked(move |_| {
            h.set_text(&c);
            set(&c);
        });
        row.append(&bt);
    }
    row.append(&hex);
    hex.connect_activate(move |e| set(e.text().trim()));
    b.append(&row);
    note(&b, "The x11 look: night by default, one accent, square frames, monospace, nothing animates.");
    s.upcast()
}

// ---- general ----

fn autostart_file() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| paths::home().join(".config"));
    base.join("autostart").join(format!("{}.desktop", crate::ui::APP_ID))
}

fn general_page() -> gtk::Widget {
    let (s, b) = page("general");
    check(&b, "keep running when the window is closed (for notifications and reminders; quit with ctrl+q)", "app.background", false, |_| {});
    let auto = gtk::CheckButton::with_label("start at login, in the background");
    auto.set_active(autostart_file().exists());
    auto.connect_toggled(|c| {
        let f = autostart_file();
        if c.is_active() {
            let _ = std::fs::create_dir_all(f.parent().unwrap());
            let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "maildot".into());
            let text = format!("[Desktop Entry]\nType=Application\nName=mail.\nExec={exe} --background\nIcon={}\nX-GNOME-Autostart-enabled=true\nNoDisplay=true\n", crate::ui::APP_ID);
            match std::fs::write(&f, text) {
                Ok(()) => {
                    let _ = eng().db.set_flag("app.background", true);
                    toast("starts at login; closing the window keeps it running");
                }
                Err(e) => toast_err(&format!("cannot write {}: {e}", f.display())),
            }
        } else {
            let _ = std::fs::remove_file(&f);
        }
    });
    b.append(&auto);
    let row = hbox(6);
    row.append(&label("without push, ask the server for changes every"));
    let poll = entry("30");
    poll.set_width_chars(5);
    poll.set_text(&eng().db.setting_or("mail.poll", "30"));
    poll.connect_changed(|e| {
        if let Ok(v) = e.text().trim().parse::<u64>() {
            if v >= 5 {
                let _ = eng().db.set_setting("mail.poll", &v.to_string());
            }
        }
    });
    row.append(&poll);
    row.append(&label("seconds"));
    b.append(&row);
    section(&b, "data on this computer");
    let dir = paths::data_dir();
    let l = label(&format!("{}\n  maildot.db (index, headers, search text, calendars, keys, settings)\n  mail/<account>/…/<id>.eml (every message, decrypted)", dir.display()));
    l.set_selectable(true);
    b.append(&l);
    let stats = label_c("", &["dim", "small"]);
    b.append(&stats);
    {
        let d2 = dir.clone();
        let st = stats.clone();
        blocking(
            move || {
                fn walk(p: &std::path::Path) -> (u64, u64) {
                    let mut out = (0, 0);
                    if let Ok(rd) = std::fs::read_dir(p) {
                        for e in rd.flatten() {
                            let path = e.path();
                            if path.is_dir() {
                                let (a, b) = walk(&path);
                                out.0 += a;
                                out.1 += b;
                            } else if let Ok(m) = e.metadata() {
                                out.0 += 1;
                                out.1 += m.len();
                            }
                        }
                    }
                    out
                }
                walk(&d2)
            },
            move |(n, bytes)| st.set_text(&format!("{n} files, {} · {} messages searchable", size(bytes as i64), eng().db.total_messages())),
        );
    }
    let open_b = button_c("open the folder", &["small"]);
    open_b.set_halign(gtk::Align::Start);
    open_b.connect_clicked(move |_| open_path(&dir));
    b.append(&open_b);
    note(&b, "Everything here is stored unencrypted on purpose (search, grep). Keep the disk encrypted (LUKS).");
    section(&b, "about");
    b.append(&label_c(&format!("maildot {} — mail. and calendar for the desktop; JMAP, CalDAV, Sieve, OpenPGP, shelf", env!("CARGO_PKG_VERSION")), &["dim", "small"]));
    let _ = Rc::new(());
    s.upcast()
}
