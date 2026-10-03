//! The main window: top bar (brand, tags, controls), the three pages, the status bar, toasts;
//! app actions with their shortcuts; the engine's events.

use crate::util::*;
use crate::{calview, compose, mail, searchview, settings, style};
use gtk::prelude::*;
use gtk::{gio, glib};
use maildot_core::model::Event;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub const APP_ID: &str = "io.github.th3rumbl3m4t0r.jmaildot";

pub struct Ui {
    pub app: gtk::Application,
    pub win: gtk::ApplicationWindow,
    pub stack: gtk::Stack,
    pub tabs: Vec<gtk::ToggleButton>,
    pub status: gtk::Label,
    pub status_right: gtk::Label,
    pub unlock: gtk::Button,
    pub mail: Rc<mail::MailView>,
    pub search: Rc<searchview::SearchView>,
    pub cal: Rc<calview::CalView>,
    toast_box: gtk::Box,
    toast_label: gtk::Label,
    toast_gen: Cell<u32>,
    hold: RefCell<Option<gio::ApplicationHoldGuard>>,
    mode_btn: gtk::Button,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
}

pub fn ui() -> Rc<Ui> {
    try_ui().expect("ui")
}

pub fn try_ui() -> Option<Rc<Ui>> {
    UI.with(|u| u.borrow().clone())
}

fn host() -> String {
    std::fs::read_to_string("/etc/hostname").map(|s| s.trim().to_string()).ok().filter(|s| !s.is_empty()).unwrap_or_else(|| glib::host_name().to_string())
}

pub fn build(app: &gtk::Application) -> Rc<Ui> {
    if let Some(u) = try_ui() {
        return u;
    }
    style::apply();
    let win = gtk::ApplicationWindow::builder().application(app).title("mail.").default_width(1400).default_height(860).build();
    win.set_icon_name(Some(APP_ID));
    // the main window keeps a quiet outline; pop-up windows get the accent (style.rs)
    win.add_css_class("main");

    // ---- top bar ----
    let hb = gtk::HeaderBar::new();
    hb.set_show_title_buttons(true);
    let brand = label_c("MAIL.", &["brand"]);
    let hostl = label_c(&format!("// {}", host()), &["brand-host"]);
    hb.pack_start(&brand);
    hb.pack_start(&hostl);
    let mut tabs = Vec::new();
    for (name, title) in [("mail", "mail"), ("calendar", "calendar"), ("search", "search")] {
        let t = gtk::ToggleButton::with_label(title);
        t.add_css_class("tag");
        t.set_widget_name(&format!("tab-{name}"));
        t.set_focus_on_click(false);
        if let Some(first) = tabs.first() {
            t.set_group(Some(first));
        }
        hb.pack_start(&t);
        tabs.push(t);
    }
    let status_top = label_c("", &["dim", "small"]);
    hb.set_title_widget(Some(&status_top));
    let settings_b = named(button_c("settings", &["flat"]), "btn-settings");
    let mode_b = named(button_c(if style::night() { "night" } else { "day" }, &["flat"]), "btn-mode");
    let sync_b = named(button_c("sync", &["flat"]), "btn-sync");
    let write_b = named(button_c("write", &["primary"]), "btn-write");
    hb.pack_end(&settings_b);
    hb.pack_end(&mode_b);
    hb.pack_end(&sync_b);
    hb.pack_end(&write_b);
    win.set_titlebar(Some(&hb));

    // ---- pages ----
    let stack = gtk::Stack::new();
    stack.set_transition_type(gtk::StackTransitionType::None);
    let mailv = mail::MailView::new();
    let searchv = searchview::SearchView::new();
    let calv = calview::CalView::new();
    stack.add_named(&mailv.root, Some("mail"));
    stack.add_named(&calv.root, Some("calendar"));
    stack.add_named(&searchv.root, Some("search"));

    // ---- status bar ----
    let sb = hbox(8);
    sb.add_css_class("statusbar");
    let status = label("");
    status.set_ellipsize(gtk::pango::EllipsizeMode::End);
    status.set_hexpand(true);
    let unlock = named(button_c("unlock key", &["link"]), "btn-unlock");
    unlock.set_visible(false);
    let status_right = label("");
    sb.append(&status);
    sb.append(&unlock);
    sb.append(&status_right);

    let main = vbox(0);
    main.append(&stack);
    main.append(&sb);
    stack.set_vexpand(true);

    // ---- toast ----
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&main));
    let toast_box = hbox(0);
    toast_box.add_css_class("toast");
    toast_box.set_halign(gtk::Align::Center);
    toast_box.set_valign(gtk::Align::End);
    toast_box.set_margin_bottom(30);
    let toast_label = label("");
    toast_label.set_wrap(true);
    toast_label.set_max_width_chars(90);
    toast_box.append(&toast_label);
    toast_box.set_visible(false);
    toast_box.set_can_target(false);
    overlay.add_overlay(&toast_box);
    win.set_child(Some(&overlay));

    let u = Rc::new(Ui {
        app: app.clone(),
        win: win.clone(),
        stack: stack.clone(),
        tabs: tabs.clone(),
        status: status.clone(),
        status_right,
        unlock: unlock.clone(),
        mail: mailv,
        search: searchv,
        cal: calv,
        toast_box,
        toast_label,
        toast_gen: Cell::new(0),
        hold: RefCell::new(None),
        mode_btn: mode_b.clone(),
    });
    UI.with(|x| *x.borrow_mut() = Some(u.clone()));

    for (i, t) in tabs.iter().enumerate() {
        let name = ["mail", "calendar", "search"][i];
        t.connect_toggled(move |t| {
            if t.is_active() {
                ui().show(name);
            }
        });
    }
    tabs[0].set_active(true);
    write_b.connect_clicked(|_| compose::open(compose::Kind::New));
    sync_b.connect_clicked(|_| {
        eng().sync_now(None);
        toast("syncing");
    });
    settings_b.connect_clicked(|_| settings::open(None));
    mode_b.connect_clicked(|b| {
        let night = !style::night();
        let _ = eng().db.set_setting("look.theme", if night { "night" } else { "day" });
        style::apply();
        b.set_label(if night { "night" } else { "day" });
        ui().restyled();
    });
    unlock.connect_clicked(|_| ask_passphrase(|| {}));

    // closing: to the background when asked to, else quit
    win.connect_close_request(|w| {
        if eng().db.flag("app.background", false) {
            w.set_visible(false);
            let u = ui();
            if u.hold.borrow().is_none() {
                *u.hold.borrow_mut() = Some(u.app.hold());
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });

    u.mail.reload_folders();
    u.mail.select(mail::Sel::Inboxes);
    u.cal.refresh();
    u.refresh_status();
    glib::timeout_add_seconds_local(5, || {
        if let Some(u) = try_ui() {
            u.refresh_status();
        }
        glib::ControlFlow::Continue
    });
    if eng().accounts().is_empty() {
        glib::idle_add_local_once(|| settings::open(Some("accounts")));
    }
    u
}

impl Ui {
    pub fn show(&self, page: &str) {
        self.stack.set_visible_child_name(page);
        let i = match page {
            "calendar" => 1,
            "search" => 2,
            _ => 0,
        };
        if !self.tabs[i].is_active() {
            self.tabs[i].set_active(true);
        }
        if page == "search" {
            self.search.focus();
        }
        if page == "calendar" {
            self.cal.refresh();
        }
    }

    /// Started with --background: no window, but the program stays (sync, notifications).
    pub fn keep_running(&self) {
        if self.hold.borrow().is_none() {
            *self.hold.borrow_mut() = Some(self.app.hold());
        }
    }

    pub fn present(&self) {
        *self.hold.borrow_mut() = None;
        self.win.set_visible(true);
        self.win.present();
    }

    pub fn toast(&self, text: &str, bad: bool) {
        self.toast_label.set_text(text);
        if bad {
            self.toast_box.add_css_class("bad");
        } else {
            self.toast_box.remove_css_class("bad");
        }
        self.toast_box.set_visible(true);
        let g = self.toast_gen.get() + 1;
        self.toast_gen.set(g);
        let secs = if bad { 8 } else { 3 };
        glib::timeout_add_seconds_local_once(secs, move || {
            if let Some(u) = try_ui() {
                if u.toast_gen.get() == g {
                    u.toast_box.set_visible(false);
                }
            }
        });
    }

    /// The status bar: each account's sync line, what is stored, the key.
    pub fn refresh_status(&self) {
        let e = eng();
        let st = e.statuses();
        let accounts = e.accounts();
        let mut parts = Vec::new();
        let mut bad = false;
        for a in &accounts {
            if let Some((s, b)) = st.get(&a.id) {
                parts.push(format!("{}: {}", short_name(&a.name), s));
                bad |= *b;
            }
        }
        self.status.set_text(&parts.join("   ·   "));
        if bad {
            self.status.add_css_class("bad");
        } else {
            self.status.remove_css_class("bad");
        }
        let n = e.db.total_messages();
        let pending: i64 = accounts.iter().map(|a| {
            let (t, s) = e.db.counts(a.id);
            t - s
        }).sum();
        let mut right = format!("{n} messages stored");
        if pending > 0 {
            right.push_str(&format!(" · {pending} to download"));
        }
        if e.syncing() {
            right.push_str(" · syncing");
        }
        self.status_right.set_text(&right);
        self.unlock.set_visible(e.locked());
    }

    /// The mode or accent changed: what draws colours itself redraws.
    pub fn restyled(&self) {
        self.mode_btn.set_label(if style::night() { "night" } else { "day" });
        self.cal.refresh();
        self.mail.reader.rerender();
        self.search.reader.rerender();
    }

    pub fn on_event(&self, ev: Event) {
        match ev {
            Event::Changed(_) => {
                self.mail.schedule_refresh();
            }
            Event::Progress(..) | Event::Status(..) => self.refresh_status(),
            Event::NewMail(account, ids) => notify_mail(account, &ids),
            Event::CalChanged(_) => self.cal.schedule_refresh(),
            Event::Reminder(title, when, place) => {
                let n = gio::Notification::new(&title);
                n.set_body(Some(&match place {
                    Some(p) => format!("{when} · {p}"),
                    None => when,
                }));
                n.set_default_action("app.show-calendar");
                self.app.send_notification(None, &n);
            }
            Event::NeedPassphrase => {
                self.refresh_status();
            }
        }
    }
}

pub fn short_name(s: &str) -> String {
    s.split('@').next().unwrap_or(s).to_string()
}

fn notify_mail(account: i64, ids: &[i64]) {
    if !eng().db.flag("notify.mail", true) {
        return;
    }
    let u = ui();
    if u.win.is_active() && u.win.is_visible() && eng().db.flag("notify.only_hidden", false) {
        return;
    }
    let rows = eng().db.msgs(ids);
    let many = rows.len() > 3;
    for r in rows.iter().take(3) {
        let n = gio::Notification::new(&r.from.short());
        let mbox = r.mailboxes.first().map(|m| eng().mailbox_name(account, m)).unwrap_or_default();
        n.set_body(Some(&format!(
            "{}{}{}",
            if r.subject.is_empty() { "(no subject)" } else { &r.subject },
            if r.encrypted { " · encrypted" } else { "" },
            if mbox.eq_ignore_ascii_case("inbox") || mbox.is_empty() { String::new() } else { format!(" · {}", mbox.to_lowercase()) }
        )));
        n.set_default_action_and_target_value("app.open-message", Some(&r.id.to_variant()));
        u.app.send_notification(Some(&format!("mail-{}", r.id)), &n);
    }
    if many {
        let n = gio::Notification::new(&format!("{} new messages", rows.len()));
        n.set_default_action("app.show-mail");
        u.app.send_notification(Some("mail-many"), &n);
    }
}

/// Asks for the key's passphrase; [then] runs once it opened the key.
pub fn ask_passphrase(then: impl Fn() + 'static) {
    let then = Rc::new(then);
    prompt("unlock key", "The passphrase of your OpenPGP key (it stays in memory; \"remember\" is in settings → keys):", "", true, move |p| {
        let Some(p) = p else { return };
        let remember = eng().db.flag("pgp.remember", false);
        if eng().unlock(&p, remember) {
            toast("key unlocked; encrypted mail is being decrypted");
            if let Some(u) = try_ui() {
                u.refresh_status();
            }
            then();
        } else {
            toast_err("wrong passphrase");
        }
    });
}

/// The app actions and their shortcuts.
pub fn actions(app: &gtk::Application) {
    let add = |name: &str, f: Box<dyn Fn()>| {
        let a = gio::SimpleAction::new(name, None);
        a.connect_activate(move |_, _| f());
        app.add_action(&a);
    };
    add("compose", Box::new(|| compose::open(compose::Kind::New)));
    add("reply", Box::new(|| ui().mail.reader.act("reply")));
    add("reply-all", Box::new(|| ui().mail.reader.act("reply-all")));
    add("forward", Box::new(|| ui().mail.reader.act("forward")));
    add("toggle-read", Box::new(|| ui().mail.act_selected("read")));
    add("toggle-flag", Box::new(|| ui().mail.act_selected("flag")));
    add("sync", Box::new(|| eng().sync_now(None)));
    add("settings", Box::new(|| settings::open(None)));
    add("show-mail", Box::new(|| {
        let u = ui();
        u.present();
        u.show("mail");
    }));
    add("show-calendar", Box::new(|| {
        let u = ui();
        u.present();
        u.show("calendar");
    }));
    add("show-search", Box::new(|| {
        let u = ui();
        u.present();
        u.show("search");
    }));
    add("find", Box::new(|| {
        let u = ui();
        match u.stack.visible_child_name().as_deref() {
            Some("search") => u.search.focus(),
            _ => u.mail.focus_filter(),
        }
    }));
    add("new-event", Box::new(|| {
        let u = ui();
        u.show("calendar");
        u.cal.new_event(None);
    }));
    add("quit", Box::new(|| {
        eng().stop_all();
        if let Some(u) = try_ui() {
            u.app.quit();
        }
    }));
    let open = gio::SimpleAction::new("open-message", Some(glib::VariantTy::INT64));
    open.connect_activate(|_, p| {
        if let Some(id) = p.and_then(|v| v.get::<i64>()) {
            let u = ui();
            u.present();
            u.show("mail");
            u.mail.open_message(id);
        }
    });
    app.add_action(&open);
    for (a, keys) in [
        ("app.compose", &["<Control>n"][..]),
        ("app.reply", &["<Control>r"]),
        ("app.reply-all", &["<Control><Shift>r"]),
        ("app.forward", &["<Control>l"]),
        ("app.toggle-read", &["<Control><Shift>u"]),
        ("app.toggle-flag", &["<Control><Shift>s"]),
        ("app.sync", &["F5"]),
        ("app.settings", &["<Control>comma"]),
        ("app.show-mail", &["<Control>1"]),
        ("app.show-calendar", &["<Control>2"]),
        ("app.show-search", &["<Control>3", "<Control><Shift>f"]),
        ("app.find", &["<Control>f"]),
        ("app.new-event", &["<Control>e"]),
        ("app.quit", &["<Control>q"]),
    ] {
        app.set_accels_for_action(a, keys);
    }
}
