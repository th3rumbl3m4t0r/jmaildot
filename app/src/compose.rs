//! The editor: a window per message. New, reply (from the address the mail was sent to, so
//! catch-all aliases answer as themselves), reply all, forward, a draft again, mailto:.

use crate::util::*;
use crate::shelfui;
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use maildot_core::engine::{Opened, Outgoing};
use maildot_core::mime;
use maildot_core::model::Addr;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Kind {
    New,
    Reply(i64),
    ReplyAll(i64),
    Forward(i64),
    Draft(i64),
    Mailto(String),
}

struct Editor {
    win: gtk::Window,
    accounts: Vec<maildot_core::model::Account>,
    account_dd: gtk::DropDown,
    from: gtk::Entry,
    from_note: gtk::Label,
    to: gtk::Entry,
    cc: gtk::Entry,
    bcc: gtk::Entry,
    subject: gtk::Entry,
    body: gtk::TextView,
    files: RefCell<Vec<(String, String, Vec<u8>)>>,
    files_box: gtk::Box,
    encrypt: gtk::CheckButton,
    sign: gtk::CheckButton,
    pgp_note: gtk::Label,
    draft: Cell<Option<i64>>,
    answering: Cell<Option<i64>>,
    in_reply_to: RefCell<Option<String>>,
    references: RefCell<Option<String>>,
    touched_crypto: Cell<bool>,
    changed: Cell<bool>,
    sending: Cell<bool>,
}

thread_local! {
    /// The open editors: their windows only hold weak references, this keeps them alive.
    static OPEN: RefCell<Vec<Rc<Editor>>> = const { RefCell::new(Vec::new()) };
}

pub fn open(kind: Kind) {
    let accounts = eng().accounts();
    if accounts.is_empty() {
        toast_err("add an account first (settings → accounts)");
        return;
    }
    let ed = build(accounts);
    OPEN.with(|o| o.borrow_mut().push(ed.clone()));
    {
        let w = Rc::downgrade(&ed);
        ed.win.connect_destroy(move |_| {
            if let Some(e) = w.upgrade() {
                OPEN.with(|o| o.borrow_mut().retain(|x| !Rc::ptr_eq(x, &e)));
            }
        });
    }
    let answering = matches!(kind, Kind::Reply(_) | Kind::ReplyAll(_));
    match kind.clone() {
        Kind::New => ed.set_account(0),
        Kind::Mailto(uri) => {
            ed.set_account(0);
            ed.fill_mailto(&uri);
        }
        Kind::Reply(id) | Kind::ReplyAll(id) | Kind::Forward(id) | Kind::Draft(id) => {
            let ed2 = ed.clone();
            run(eng().open(id), move |r| match r {
                Ok(o) => ed2.fill_from(&kind, &o),
                Err(e) => toast_err(&format!("cannot open the message: {}", err_text(&e))),
            });
        }
    }
    ed.win.present();
    if answering {
        ed.body.grab_focus();
    } else {
        ed.to.grab_focus();
    }
}

fn field_row(grid: &gtk::Grid, row: i32, name: &str, w: &impl IsA<gtk::Widget>) {
    let l = lbl(name);
    l.set_xalign(1.0);
    grid.attach(&l, 0, row, 1, 1);
    w.set_hexpand(true);
    grid.attach(w, 1, row, 1, 1);
}

fn build(accounts: Vec<maildot_core::model::Account>) -> Rc<Editor> {
    let win = gtk::Window::builder().title("write").default_width(860).default_height(720).build();
    win.set_widget_name("compose");
    let root = vbox(0);
    let (tb, _) = titlebar("write");
    root.append(&tb);
    let grid = gtk::Grid::new();
    grid.set_row_spacing(4);
    grid.set_column_spacing(8);
    grid.set_margin_start(10);
    grid.set_margin_end(10);
    grid.set_margin_top(8);
    grid.set_margin_bottom(6);
    let names: Vec<String> = accounts.iter().map(|a| a.name.clone()).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let account_dd = dropdown(&refs, 0);
    let mut r = 0;
    if accounts.len() > 1 {
        field_row(&grid, r, "account", &account_dd);
        r += 1;
    }
    let from_row = hbox(4);
    let from = named(gtk::Entry::new(), "c-from");
    from.set_hexpand(true);
    let from_pick = named(gtk::MenuButton::new(), "c-from-pick");
    from_pick.set_label("▾");
    from_row.append(&from);
    from_row.append(&from_pick);
    field_row(&grid, r, "from", &from_row);
    r += 1;
    let from_note = label_c("", &["warn", "small"]);
    from_note.set_visible(false);
    grid.attach(&from_note, 1, r, 1, 1);
    r += 1;
    let to = named(gtk::Entry::new(), "c-to");
    let cc = named(gtk::Entry::new(), "c-cc");
    let bcc = named(gtk::Entry::new(), "c-bcc");
    let subject = named(gtk::Entry::new(), "c-subject");
    field_row(&grid, r, "to", &to);
    field_row(&grid, r + 1, "cc", &cc);
    field_row(&grid, r + 2, "bcc", &bcc);
    field_row(&grid, r + 3, "subject", &subject);
    root.append(&grid);

    let body = gtk::TextView::new();
    body.set_widget_name("c-body");
    body.add_css_class("edit");
    body.set_monospace(true);
    body.set_wrap_mode(gtk::WrapMode::WordChar);
    body.set_left_margin(8);
    body.set_right_margin(8);
    body.set_top_margin(6);
    body.set_bottom_margin(6);
    let sw = scrolled(&body);
    sw.set_margin_start(10);
    sw.set_margin_end(10);
    root.append(&sw);

    let files_box = hbox(4);
    files_box.set_margin_start(10);
    files_box.set_margin_end(10);
    files_box.set_margin_top(4);
    let files_wrap = gtk::ScrolledWindow::new();
    files_wrap.set_child(Some(&files_box));
    files_wrap.set_vscrollbar_policy(gtk::PolicyType::Never);
    root.append(&files_wrap);

    let crypto = hbox(10);
    crypto.set_margin_start(10);
    crypto.set_margin_end(10);
    crypto.set_margin_top(4);
    let encrypt = gtk::CheckButton::with_label("encrypt");
    let sign = gtk::CheckButton::with_label("sign");
    let pgp_note = label_c("", &["dim", "small"]);
    pgp_note.set_hexpand(true);
    pgp_note.set_wrap(true);
    crypto.append(&encrypt);
    crypto.append(&sign);
    crypto.append(&pgp_note);
    root.append(&crypto);

    let bar = hbox(6);
    bar.add_css_class("statusbar");
    let attach_b = button("attach file");
    let shelf_b = named(button("from shelf"), "c-shelf");
    let draft_b = named(button("save draft"), "c-draft");
    let send_b = named(button_c("send", &["primary"]), "c-send");
    bar.append(&attach_b);
    bar.append(&shelf_b);
    bar.append(&spacer());
    bar.append(&label_c("ctrl+enter sends", &["dim"]));
    bar.append(&draft_b);
    bar.append(&send_b);
    root.append(&bar);
    win.set_child(Some(&root));

    let ed = Rc::new(Editor {
        win: win.clone(),
        accounts,
        account_dd: account_dd.clone(),
        from: from.clone(),
        from_note,
        to: to.clone(),
        cc: cc.clone(),
        bcc: bcc.clone(),
        subject: subject.clone(),
        body: body.clone(),
        files: RefCell::new(vec![]),
        files_box,
        encrypt: encrypt.clone(),
        sign: sign.clone(),
        pgp_note,
        draft: Cell::new(None),
        answering: Cell::new(None),
        in_reply_to: RefCell::new(None),
        references: RefCell::new(None),
        touched_crypto: Cell::new(false),
        changed: Cell::new(false),
        sending: Cell::new(false),
    });

    {
        let w = Rc::downgrade(&ed);
        account_dd.connect_selected_notify(move |d| {
            if let Some(e) = w.upgrade() {
                e.set_account(d.selected() as usize);
            }
        });
    }
    from_picker(&ed, &from_pick);
    for entry in [&to, &cc, &bcc] {
        complete(entry);
        let w = Rc::downgrade(&ed);
        entry.connect_changed(move |_| {
            if let Some(e) = w.upgrade() {
                e.changed.set(true);
                e.update_crypto();
            }
        });
    }
    {
        let w = Rc::downgrade(&ed);
        from.connect_changed(move |_| {
            if let Some(e) = w.upgrade() {
                e.check_from();
            }
        });
    }
    {
        let w = Rc::downgrade(&ed);
        subject.connect_changed(move |s| {
            if let Some(e) = w.upgrade() {
                e.changed.set(true);
                e.win.set_title(Some(&if s.text().is_empty() { "write".to_string() } else { s.text().to_string() }));
            }
        });
        let w = Rc::downgrade(&ed);
        body.buffer().connect_changed(move |_| {
            if let Some(e) = w.upgrade() {
                e.changed.set(true);
            }
        });
    }
    for c in [&encrypt, &sign] {
        let w = Rc::downgrade(&ed);
        c.connect_toggled(move |_| {
            if let Some(e) = w.upgrade() {
                e.describe_crypto();
            }
        });
        let w = Rc::downgrade(&ed);
        let g = gtk::GestureClick::new();
        g.connect_pressed(move |_, _, _, _| {
            if let Some(e) = w.upgrade() {
                e.touched_crypto.set(true);
            }
        });
        c.add_controller(g);
    }
    {
        let w = Rc::downgrade(&ed);
        attach_b.connect_clicked(move |_| {
            let w = w.clone();
            pick_files("attach", move |paths| {
                if let Some(e) = w.upgrade() {
                    for p in paths {
                        match std::fs::read(&p) {
                            Ok(d) => {
                                let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                                let mime = guess_mime(&name);
                                e.add_file(name, mime, d);
                            }
                            Err(err) => toast_err(&format!("cannot read {}: {err}", p.display())),
                        }
                    }
                }
            });
        });
        let w = Rc::downgrade(&ed);
        shelf_b.connect_clicked(move |_| {
            let w = w.clone();
            shelfui::pick_files(move |files| {
                if let Some(e) = w.upgrade() {
                    for (name, data) in files {
                        let mime = guess_mime(&name);
                        e.add_file(name, mime, data);
                    }
                }
            });
        });
    }
    // files dropped on the window are attached
    {
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        let w = Rc::downgrade(&ed);
        drop.connect_drop(move |_, v, _, _| {
            let Some(e) = w.upgrade() else { return false };
            let Ok(list) = v.get::<gdk::FileList>() else { return false };
            for f in list.files() {
                if let Some(p) = f.path() {
                    if let Ok(d) = std::fs::read(&p) {
                        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                        let mime = guess_mime(&name);
                        e.add_file(name, mime, d);
                    }
                }
            }
            true
        });
        win.add_controller(drop);
    }
    {
        let w = Rc::downgrade(&ed);
        send_b.connect_clicked(move |_| {
            if let Some(e) = w.upgrade() {
                e.send(false);
            }
        });
        let w = Rc::downgrade(&ed);
        draft_b.connect_clicked(move |_| {
            if let Some(e) = w.upgrade() {
                e.send(true);
            }
        });
        let k = gtk::EventControllerKey::new();
        let w = Rc::downgrade(&ed);
        k.connect_key_pressed(move |_, key, _, state| {
            if state.contains(gdk::ModifierType::CONTROL_MASK) && (key == gdk::Key::Return || key == gdk::Key::KP_Enter) {
                if let Some(e) = w.upgrade() {
                    e.send(false);
                }
                return glib::Propagation::Stop;
            }
            if key == gdk::Key::Escape {
                if let Some(e) = w.upgrade() {
                    e.win.close();
                }
                return glib::Propagation::Stop;
            }
            glib::Propagation::Proceed
        });
        win.add_controller(k);
    }
    {
        let w = Rc::downgrade(&ed);
        win.connect_close_request(move |win| {
            let Some(e) = w.upgrade() else { return glib::Propagation::Proceed };
            if !e.changed.get() || e.sending.get() {
                return glib::Propagation::Proceed;
            }
            let e2 = e.clone();
            let win = win.clone();
            ask("close", "This message is not sent. Keep it?", &["discard", "save draft", "keep editing"], move |i| match i {
                Some(0) => {
                    e2.changed.set(false);
                    win.close();
                }
                Some(1) => e2.send(true),
                _ => {}
            });
            glib::Propagation::Stop
        });
    }
    if let Some(p) = crate::ui::try_ui() {
        let _ = &p;
    }
    ed
}

fn fav_key(account: i64) -> String {
    format!("from.fav.{account}")
}

/// The account's favourite from addresses, in the order they were added (`Name <address>`).
fn favourites(account: i64) -> Vec<String> {
    eng().db.setting(&fav_key(account)).map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect()).unwrap_or_default()
}

fn email_of(s: &str) -> String {
    Addr::parse(s).map(|a| a.email.to_lowercase()).unwrap_or_else(|| s.trim().to_lowercase())
}

fn is_favourite(account: i64, full: &str) -> bool {
    let e = email_of(full);
    favourites(account).iter().any(|f| email_of(f) == e)
}

/// Adds or removes a favourite (by address; the name it was added with is kept).
fn toggle_favourite(account: i64, full: &str) {
    let mut list = favourites(account);
    let e = email_of(full);
    match list.iter().position(|f| email_of(f) == e) {
        Some(i) => {
            list.remove(i);
        }
        None => list.push(full.trim().to_string()),
    }
    let _ = eng().db.set_setting(&fav_key(account), &list.join("\n"));
}

/// The ▾ next to from: the favourites on top (they stay there while the long list scrolls),
/// then every address of the account (identities and the catch-all ones seen). ☆ / ★ on each
/// row adds or removes a favourite; the button at the bottom stars what is typed in the field.
fn from_picker(ed: &Rc<Editor>, button: &gtk::MenuButton) {
    let pop = gtk::Popover::new();
    pop.set_has_arrow(false);
    let root = vbox(4);
    root.set_size_request(380, -1);
    let fav_head = lbl("favourites");
    let favs = gtk::ListBox::new();
    favs.add_css_class("plain");
    favs.set_widget_name("from-favs");
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    let all_head = lbl("all your addresses");
    let all = gtk::ListBox::new();
    all.add_css_class("plain");
    all.set_widget_name("from-all");
    let sw = gtk::ScrolledWindow::new();
    sw.set_child(Some(&all));
    sw.set_propagate_natural_height(true);
    sw.set_max_content_height(360);
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    let star_field = named(button_c("☆ the address in the field", &["small"]), "from-star-field");
    star_field.set_halign(gtk::Align::Start);
    root.append(&fav_head);
    root.append(&favs);
    root.append(&sep);
    root.append(&all_head);
    root.append(&sw);
    root.append(&star_field);
    pop.set_child(Some(&root));
    button.set_popover(Some(&pop));

    let shown_favs: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec![]));
    let shown_all: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(vec![]));
    type Fill = Rc<RefCell<Option<Box<dyn Fn()>>>>;
    let fill: Fill = Rc::new(RefCell::new(None));
    let row = |text: &str, starred: bool, account: i64, fill: &Fill, name: String| -> gtk::Box {
        let b = hbox(4);
        let l = label(text);
        l.set_hexpand(true);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        b.append(&l);
        let star = named(button_c(if starred { "★" } else { "☆" }, &["small", "flat"]), &name);
        star.set_tooltip_text(Some(if starred { "remove from the favourites" } else { "add to the favourites" }));
        if starred {
            star.add_css_class("accent");
        }
        let (t, f) = (text.to_string(), fill.clone());
        star.connect_clicked(move |_| {
            toggle_favourite(account, &t);
            if let Some(f) = f.borrow().as_ref() {
                f();
            }
        });
        b.append(&star);
        b
    };
    {
        let w = Rc::downgrade(ed);
        let (favs, all, fav_head, sep, all_head, star_field) = (favs.clone(), all.clone(), fav_head.clone(), sep.clone(), all_head.clone(), star_field.clone());
        let (sf, sa, f2) = (shown_favs.clone(), shown_all.clone(), fill.clone());
        *fill.borrow_mut() = Some(Box::new(move || {
            let Some(e) = w.upgrade() else { return };
            let account = e.account().id;
            for lb in [&favs, &all] {
                while let Some(r) = lb.row_at_index(0) {
                    lb.remove(&r);
                }
            }
            let fl = favourites(account);
            for (i, f) in fl.iter().enumerate() {
                favs.append(&row(f, true, account, &f2, format!("from-fav-star-{i}")));
            }
            let has = !fl.is_empty();
            fav_head.set_visible(has);
            favs.set_visible(has);
            sep.set_visible(has);
            let addrs: Vec<String> = eng().our_addresses(account).iter().map(|a| a.full()).collect();
            all_head.set_text(&format!("ALL YOUR ADDRESSES ({})", addrs.len()));
            for (i, a) in addrs.iter().enumerate() {
                all.append(&row(a, is_favourite(account, a), account, &f2, format!("from-all-star-{i}")));
            }
            *sf.borrow_mut() = fl;
            *sa.borrow_mut() = addrs;
            let cur = e.from.text().to_string();
            let ok = Addr::parse(&cur).is_some();
            star_field.set_visible(ok && !is_favourite(account, &cur));
        }));
    }
    {
        let f = fill.clone();
        pop.connect_show(move |_| {
            if let Some(f) = f.borrow().as_ref() {
                f();
            }
        });
    }
    for (lb, shown) in [(&favs, shown_favs.clone()), (&all, shown_all.clone())] {
        let w = Rc::downgrade(ed);
        let p2 = pop.clone();
        lb.connect_row_activated(move |_, row| {
            if let (Some(e), Some(a)) = (w.upgrade(), shown.borrow().get(row.index() as usize)) {
                e.from.set_text(a);
            }
            p2.popdown();
        });
    }
    {
        let w = Rc::downgrade(ed);
        let f = fill.clone();
        star_field.connect_clicked(move |_| {
            let Some(e) = w.upgrade() else { return };
            let cur = e.from.text().to_string();
            if let Some(a) = Addr::parse(&cur) {
                toggle_favourite(e.account().id, &a.full());
                if let Some(f) = f.borrow().as_ref() {
                    f();
                }
            }
        });
    }
}

/// Completion for address fields: the last address being typed, from people we wrote with.
fn complete(e: &gtk::Entry) {
    let pop = gtk::Popover::new();
    pop.set_has_arrow(false);
    pop.set_autohide(false);
    pop.set_parent(e);
    pop.set_position(gtk::PositionType::Bottom);
    pop.set_halign(gtk::Align::Start);
    let lb = gtk::ListBox::new();
    lb.add_css_class("plain");
    pop.set_child(Some(&lb));
    let found: Rc<RefCell<Vec<Addr>>> = Rc::new(RefCell::new(vec![]));
    {
        let lb = lb.clone();
        let pop = pop.clone();
        let found = found.clone();
        e.connect_changed(move |e| {
            if !e.has_focus() {
                pop.popdown();
                return;
            }
            let t = e.text().to_string();
            let last = t.rsplit([',', ';']).next().unwrap_or("").trim().to_string();
            while let Some(r) = lb.row_at_index(0) {
                lb.remove(&r);
            }
            if last.len() < 2 || last.contains('>') {
                pop.popdown();
                return;
            }
            let list = eng().db.contacts(&last, 8);
            if list.is_empty() || (list.len() == 1 && list[0].email.eq_ignore_ascii_case(&last)) {
                pop.popdown();
                return;
            }
            for a in &list {
                lb.append(&label(&a.full()));
            }
            *found.borrow_mut() = list;
            pop.popup();
        });
    }
    let pick = {
        let e = e.clone();
        let pop = pop.clone();
        let found = found.clone();
        move |i: usize| {
            if let Some(a) = found.borrow().get(i) {
                let t = e.text().to_string();
                let head = match t.rfind([',', ';']) {
                    Some(k) => format!("{}, ", t[..k].trim()),
                    None => String::new(),
                };
                e.set_text(&format!("{head}{}, ", a.full()));
                e.set_position(-1);
            }
            pop.popdown();
        }
    };
    {
        let pick = pick.clone();
        lb.connect_row_activated(move |_, row| pick(row.index() as usize));
    }
    // down / up / enter / escape while typing
    let k = gtk::EventControllerKey::new();
    k.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let pop = pop.clone();
        let lb = lb.clone();
        k.connect_key_pressed(move |_, key, _, _| {
            if !pop.is_visible() {
                return glib::Propagation::Proceed;
            }
            let cur = lb.selected_row().map(|r| r.index()).unwrap_or(-1);
            match key {
                gdk::Key::Down => {
                    if let Some(r) = lb.row_at_index(cur + 1) {
                        lb.select_row(Some(&r));
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    if let Some(r) = lb.row_at_index((cur - 1).max(0)) {
                        lb.select_row(Some(&r));
                    }
                    glib::Propagation::Stop
                }
                gdk::Key::Return | gdk::Key::Tab if cur >= 0 => {
                    pick(cur as usize);
                    glib::Propagation::Stop
                }
                gdk::Key::Escape => {
                    pop.popdown();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
    }
    e.add_controller(k);
    let f = gtk::EventControllerFocus::new();
    {
        let pop = pop.clone();
        f.connect_leave(move |_| {
            let pop = pop.clone();
            glib::timeout_add_local_once(std::time::Duration::from_millis(200), move || pop.popdown());
        });
    }
    e.add_controller(f);
}

impl Editor {
    fn account(&self) -> maildot_core::model::Account {
        self.accounts[(self.account_dd.selected() as usize).min(self.accounts.len() - 1)].clone()
    }

    fn set_account(&self, i: usize) {
        if self.account_dd.selected() as usize != i {
            self.account_dd.set_selected(i as u32);
        }
        let a = self.account();
        self.from.set_text(&maildot_core::senders::default_from(&a).full());
        self.update_crypto();
    }

    fn check_from(&self) {
        let a = self.account();
        let t = self.from.text().to_string();
        match Addr::parse(&t) {
            None => {
                self.from_note.set_text("not an address");
                self.from_note.set_visible(true);
            }
            Some(addr) => {
                let ours = eng().is_ours(a.id, &addr.email);
                self.from_note.set_text(&format!("{} is none of your addresses or domains: the server may refuse it or the reply may not come back", addr.email));
                self.from_note.set_visible(!ours);
            }
        }
    }

    fn rcpts(&self) -> (Vec<Addr>, Vec<String>) {
        let mut all = Vec::new();
        let mut bad = Vec::new();
        for e in [&self.to, &self.cc, &self.bcc] {
            let (a, b) = Addr::parse_list(&e.text());
            all.extend(a);
            bad.extend(b);
        }
        (all, bad)
    }

    /// Encrypt is on when every recipient has a key, sign when we have one (both can be turned off).
    fn update_crypto(&self) {
        if !self.touched_crypto.get() {
            let (rcpts, _) = self.rcpts();
            let (all, ours) = eng().can_encrypt(&rcpts);
            self.encrypt.set_active(all && ours);
            self.sign.set_active(ours && eng().db.flag("pgp.sign", true));
            self.encrypt.set_sensitive(all || self.encrypt.is_active());
            self.sign.set_sensitive(ours);
        }
        self.describe_crypto();
    }

    fn describe_crypto(&self) {
        let (rcpts, _) = self.rcpts();
        let (all, ours) = eng().can_encrypt(&rcpts);
        let missing: Vec<String> = {
            let ring = eng().keys.read().unwrap();
            rcpts.iter().filter(|a| ring.for_email(&a.email).is_none()).map(|a| a.email.clone()).collect()
        };
        let text = match (self.encrypt.is_active(), self.sign.is_active()) {
            (true, true) => "encrypted to every recipient and to you, and signed".to_string(),
            (true, false) => "encrypted to every recipient and to you, not signed".to_string(),
            (false, true) => "signed, not encrypted: anyone on the way can read it".to_string(),
            (false, false) => {
                if !ours {
                    "plain: you have no key (settings → keys)".to_string()
                } else if !all && !rcpts.is_empty() {
                    format!("plain: no key for {}", missing.join(", "))
                } else {
                    "plain, not signed".to_string()
                }
            }
        };
        self.pgp_note.set_text(&text);
    }

    fn add_file(self: &Rc<Self>, name: String, mime: String, data: Vec<u8>) {
        self.files.borrow_mut().push((name, mime, data));
        self.changed.set(true);
        self.show_files();
    }

    fn show_files(self: &Rc<Self>) {
        clear_box(&self.files_box);
        for (i, (n, _, d)) in self.files.borrow().iter().enumerate() {
            let b = hbox(2);
            b.add_css_class("att");
            b.append(&label(&format!("{n}  {}", size(d.len() as i64))));
            let x = button_c("×", &["small", "flat"]);
            let w = Rc::downgrade(self);
            x.connect_clicked(move |_| {
                if let Some(e) = w.upgrade() {
                    e.files.borrow_mut().remove(i);
                    e.show_files();
                }
            });
            b.append(&x);
            self.files_box.append(&b);
        }
    }

    fn set_body(&self, text: &str, cursor_at_start: bool) {
        let buf = self.body.buffer();
        buf.set_text(text);
        let it = if cursor_at_start { buf.start_iter() } else { buf.end_iter() };
        buf.place_cursor(&it);
    }

    fn fill_mailto(&self, uri: &str) {
        let rest = uri.trim_start_matches("mailto:");
        let (addr, query) = rest.split_once('?').unwrap_or((rest, ""));
        self.to.set_text(&pct(addr));
        for kv in query.split('&') {
            let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
            let v = pct(v);
            match k.to_lowercase().as_str() {
                "subject" => self.subject.set_text(&v),
                "body" => self.set_body(&v, true),
                "cc" => self.cc.set_text(&v),
                "bcc" => self.bcc.set_text(&v),
                "to" => self.to.set_text(&format!("{}, {v}", self.to.text())),
                _ => {}
            }
        }
        self.changed.set(false);
    }

    fn fill_from(self: &Rc<Self>, kind: &Kind, o: &Opened) {
        let account = o.row.account;
        if let Some(i) = self.accounts.iter().position(|a| a.id == account) {
            self.account_dd.set_selected(i as u32);
        }
        let p = &o.parsed;
        let body = p.text.clone().or_else(|| p.html.as_ref().map(|h| maildot_core::html::to_text(h))).unwrap_or_default();
        let who = p.from.first().map(|a| a.full()).unwrap_or_default();
        let when = long_date(p.date.unwrap_or(o.row.received));
        match kind {
            Kind::Reply(_) | Kind::ReplyAll(_) => {
                // the address the mail came to answers (catch-all aliases included)
                let from = eng().reply_from(account, &p.to, &p.cc);
                self.from.set_text(&from.full());
                let to = if p.reply_to.is_empty() { p.from.clone() } else { p.reply_to.clone() };
                let mine: Vec<String> = eng().our_addresses(account).iter().map(|a| a.email.to_lowercase()).collect();
                let is_mine = |a: &Addr| mine.contains(&a.email.to_lowercase()) || a.email.eq_ignore_ascii_case(&from.email);
                // answering our own sent mail: to its recipients
                let to = if to.iter().all(|a| is_mine(a)) && !p.to.is_empty() { p.to.clone() } else { to };
                self.to.set_text(&Addr::join(&to));
                if matches!(kind, Kind::ReplyAll(_)) {
                    let cc: Vec<Addr> = p.to.iter().chain(p.cc.iter()).filter(|a| !is_mine(a) && !to.iter().any(|t| t.email.eq_ignore_ascii_case(&a.email))).cloned().collect();
                    self.cc.set_text(&Addr::join(&cc));
                }
                self.subject.set_text(&mime::reply_subject(&p.subject));
                self.set_body(&format!("\n\nOn {when}, {who} wrote:\n{}\n", mime::quote(body.trim_end())), true);
                *self.in_reply_to.borrow_mut() = p.message_id.clone();
                let mut refs: Vec<String> = p.references.clone();
                if let Some(m) = &p.message_id {
                    refs.push(m.clone());
                }
                *self.references.borrow_mut() = if refs.is_empty() { None } else { Some(refs.join(" ")) };
                self.answering.set(Some(o.row.id));
            }
            Kind::Forward(_) => {
                let from = eng().reply_from(account, &p.to, &p.cc);
                self.from.set_text(&from.full());
                self.subject.set_text(&mime::forward_subject(&p.subject));
                self.set_body(
                    &format!(
                        "\n\n---------- Forwarded message ----------\nFrom: {who}\nDate: {when}\nSubject: {}\nTo: {}\n{}\n{}\n",
                        p.subject,
                        Addr::join(&p.to),
                        if p.cc.is_empty() { String::new() } else { format!("Cc: {}\n", Addr::join(&p.cc)) },
                        body.trim_end()
                    ),
                    true,
                );
                for a in &p.attachments {
                    self.files.borrow_mut().push((a.name.clone(), a.mime.clone(), a.data.clone()));
                }
                self.show_files();
            }
            Kind::Draft(_) => {
                if let Some(f) = p.from.first() {
                    self.from.set_text(&f.full());
                }
                self.to.set_text(&Addr::join(&p.to));
                self.cc.set_text(&Addr::join(&p.cc));
                self.bcc.set_text(&Addr::join(&p.bcc));
                self.subject.set_text(&p.subject);
                self.set_body(&body, false);
                for a in &p.attachments {
                    self.files.borrow_mut().push((a.name.clone(), a.mime.clone(), a.data.clone()));
                }
                self.show_files();
                *self.in_reply_to.borrow_mut() = p.in_reply_to.first().cloned();
                *self.references.borrow_mut() = if p.references.is_empty() { None } else { Some(p.references.join(" ")) };
                if o.row.draft() {
                    self.draft.set(Some(o.row.id));
                }
            }
            _ => {}
        }
        self.update_crypto();
        self.changed.set(false);
    }

    fn outgoing(&self) -> Result<Outgoing, String> {
        let a = self.account();
        let from = Addr::parse(&self.from.text()).ok_or("the from address is not an address")?;
        let (to, bad1) = Addr::parse_list(&self.to.text());
        let (cc, bad2) = Addr::parse_list(&self.cc.text());
        let (bcc, bad3) = Addr::parse_list(&self.bcc.text());
        let bad: Vec<String> = bad1.into_iter().chain(bad2).chain(bad3).collect();
        if !bad.is_empty() {
            return Err(format!("not an address: {}", bad.join(", ")));
        }
        let buf = self.body.buffer();
        let (s, e) = buf.bounds();
        Ok(Outgoing {
            account: a.id,
            draft: self.draft.get(),
            from,
            to,
            cc,
            bcc,
            subject: self.subject.text().to_string(),
            body: buf.text(&s, &e, false).to_string(),
            files: self.files.borrow().clone(),
            encrypt: self.encrypt.is_active(),
            sign: self.sign.is_active(),
            in_reply_to: self.in_reply_to.borrow().clone(),
            references: self.references.borrow().clone(),
            answering: self.answering.get(),
        })
    }

    fn send(self: &Rc<Self>, draft: bool) {
        if self.sending.get() {
            return;
        }
        let o = match self.outgoing() {
            Ok(o) => o,
            Err(e) => {
                toast_err(&e);
                return;
            }
        };
        if !draft && o.to.is_empty() && o.cc.is_empty() && o.bcc.is_empty() {
            toast_err("no recipient");
            return;
        }
        if !draft && o.subject.trim().is_empty() {
            let me = self.clone();
            ask("no subject", "Send without a subject?", &["cancel", "send"], move |i| {
                if i == Some(1) {
                    me.subject.set_text(" ");
                    me.send(false);
                }
            });
            return;
        }
        self.sending.set(true);
        self.win.set_sensitive(false);
        let me = self.clone();
        let fut = if draft { Box::pin(eng().save_draft(o)) as std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<()>> + Send>> } else { Box::pin(eng().send(o)) };
        run(fut, move |r| {
            me.sending.set(false);
            me.win.set_sensitive(true);
            match r {
                Ok(()) => {
                    toast(if draft { "draft saved" } else { "sent" });
                    me.changed.set(false);
                    me.win.close();
                }
                Err(e) if maildot_core::pgp::needs_passphrase(&e) => {
                    let me2 = me.clone();
                    crate::ui::ask_passphrase(move || me2.send(draft));
                }
                Err(e) => toast_err(&format!("{}: {}", if draft { "not saved" } else { "not sent" }, err_text(&e))),
            }
        });
    }
}

fn pct(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn handle_mailto(uri: &str) {
    open(Kind::Mailto(uri.to_string()));
}

#[allow(dead_code)]
fn _unused(_: gio::File) {}
