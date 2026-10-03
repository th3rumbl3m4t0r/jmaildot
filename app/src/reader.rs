//! One message: headers, the PGP line, actions, attachments, the body (text, or HTML laid out
//! as styled text with cid: images; online images only when asked), search hits highlighted.

use crate::compose::{self, Kind};
use crate::mail::{act_keyword, act_trash, move_menu};
use crate::util::*;
use crate::{shelfui, sieveview};
use gtk::prelude::*;
use gtk::{gdk, glib};
use maildot_core::engine::Opened;
use maildot_core::html::{self, Run};
use maildot_core::search::{self, Cond};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct Reader {
    pub root: gtk::Box,
    stack: gtk::Stack,
    subject: gtk::Label,
    from: gtk::Label,
    to: gtk::Label,
    cc: gtk::Label,
    date: gtk::Label,
    place: gtk::Label,
    pgp: gtk::Label,
    notice: gtk::Label,
    notice_btn: gtk::Button,
    read_b: gtk::Button,
    flag_b: gtk::Button,
    move_b: gtk::Button,
    more_b: gtk::MenuButton,
    att_box: gtk::FlowBox,
    att_row: gtk::Box,
    html_bar: gtk::Box,
    mode_b: gtk::Button,
    online_b: gtk::Button,
    find: gtk::Entry,
    find_count: gtk::Label,
    view: gtk::TextView,
    buf: gtk::TextBuffer,
    many: gtk::Label,
    many_ids: RefCell<Vec<i64>>,
    cur: RefCell<Option<Rc<Opened>>>,
    html_mode: Cell<bool>,
    online: Cell<bool>,
    hl: RefCell<Vec<Cond>>,
    links: RefCell<Vec<String>>,
    link_tags: RefCell<Vec<gtk::TextTag>>,
    gen: Cell<u64>,
}

fn tag(buf: &gtk::TextBuffer, t: gtk::TextTag) {
    buf.tag_table().add(&t);
}

fn url_re() -> regex::Regex {
    regex::Regex::new(r#"(?i)\b(?:https?://|mailto:|www\.)[^\s<>"'\])]+[^\s<>"'\]).,;:!?]"#).unwrap()
}

impl Reader {
    pub fn new(name: &str) -> Rc<Reader> {
        let root = vbox(0);
        root.add_css_class("pane");
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);

        // ---- empty / many ----
        let empty = vbox(0);
        let el = label_c("no message selected", &["dim"]);
        el.set_halign(gtk::Align::Center);
        el.set_valign(gtk::Align::Center);
        el.set_vexpand(true);
        empty.append(&el);
        stack.add_named(&empty, Some("empty"));
        let manyb = vbox(8);
        manyb.set_valign(gtk::Align::Center);
        manyb.set_halign(gtk::Align::Center);
        let many = label_c("", &["big"]);
        manyb.append(&many);
        let mrow = hbox(6);
        manyb.append(&mrow);
        stack.add_named(&manyb, Some("many"));

        // ---- message ----
        let msg = vbox(0);
        let hdr = vbox(2);
        hdr.add_css_class("hdr");
        let subject = label_c("", &["subject"]);
        subject.set_wrap(true);
        subject.set_selectable(true);
        let from = label("");
        from.set_selectable(true);
        from.set_wrap(true);
        let to = label_c("", &["dim", "small"]);
        to.set_selectable(true);
        to.set_wrap(true);
        let cc = label_c("", &["dim", "small"]);
        cc.set_selectable(true);
        cc.set_wrap(true);
        let line = hbox(8);
        let date = label_c("", &["dim", "small"]);
        let place = label_c("", &["dim", "small"]);
        place.set_hexpand(true);
        line.append(&date);
        line.append(&place);
        hdr.append(&subject);
        hdr.append(&from);
        hdr.append(&to);
        hdr.append(&cc);
        hdr.append(&line);
        msg.append(&hdr);
        let pgp = label_c("", &["pgp"]);
        pgp.set_wrap(true);
        msg.append(&pgp);
        let nrow = hbox(6);
        let notice = label_c("", &["notice"]);
        notice.set_wrap(true);
        notice.set_hexpand(true);
        let notice_btn = button_c("unlock key", &["small"]);
        nrow.append(&notice);
        nrow.append(&notice_btn);
        msg.append(&nrow);

        let bar = hbox(4);
        bar.add_css_class("bar");
        let mk = |t: &str| named(button_c(t, &["small"]), &format!("{name}-{}", t.replace(' ', "-")));
        let reply_b = mk("reply");
        let all_b = mk("reply all");
        let fwd_b = mk("forward");
        let read_b = mk("unread");
        let flag_b = mk("flag");
        let move_b = mk("move");
        let trash_b = named(button_c("trash", &["small", "danger"]), &format!("{name}-trash"));
        let more_b = gtk::MenuButton::new();
        more_b.set_label("more");
        more_b.add_css_class("small");
        for b in [&reply_b, &all_b, &fwd_b, &read_b, &flag_b, &move_b] {
            bar.append(b);
        }
        bar.append(&more_b);
        bar.append(&spacer());
        bar.append(&trash_b);
        msg.append(&bar);

        let att_row = vbox(0);
        att_row.add_css_class("bar");
        let att_box = gtk::FlowBox::new();
        att_box.set_selection_mode(gtk::SelectionMode::None);
        att_box.set_max_children_per_line(6);
        att_box.set_column_spacing(4);
        att_box.set_row_spacing(2);
        att_row.append(&att_box);
        msg.append(&att_row);

        let html_bar = hbox(4);
        html_bar.add_css_class("bar");
        let mode_b = named(button_c("text", &["small"]), &format!("{name}-mode"));
        let online_b = named(button_c("load online content", &["small"]), &format!("{name}-online"));
        let browser_b = named(button_c("open in browser", &["small"]), &format!("{name}-browser"));
        let html_lbl = lbl("html");
        html_lbl.set_widget_name("html-label");
        html_bar.append(&html_lbl);
        html_bar.append(&mode_b);
        html_bar.append(&online_b);
        html_bar.append(&browser_b);
        html_bar.append(&spacer());
        let find = named(entry("find in message"), &format!("{name}-find"));
        find.set_width_chars(18);
        let find_count = label_c("", &["dim", "small"]);
        html_bar.append(&find_count);
        html_bar.append(&find);
        msg.append(&html_bar);

        let view = gtk::TextView::new();
        view.set_editable(false);
        view.set_cursor_visible(false);
        view.set_wrap_mode(gtk::WrapMode::WordChar);
        view.set_left_margin(12);
        view.set_right_margin(12);
        view.set_top_margin(10);
        view.set_bottom_margin(16);
        view.set_pixels_below_lines(1);
        view.set_widget_name(&format!("{name}-body"));
        let buf = view.buffer();
        let sw = scrolled(&view);
        msg.append(&sw);
        stack.add_named(&msg, Some("msg"));
        root.append(&stack);

        let p = crate::style::palette();
        let accent = crate::style::accent();
        tag(&buf, gtk::TextTag::builder().name("b").weight(700).build());
        tag(&buf, gtk::TextTag::builder().name("i").style(gtk::pango::Style::Italic).build());
        tag(&buf, gtk::TextTag::builder().name("u").underline(gtk::pango::Underline::Single).build());
        tag(&buf, gtk::TextTag::builder().name("s").strikethrough(true).build());
        tag(&buf, gtk::TextTag::builder().name("small").scale(0.85).build());
        tag(&buf, gtk::TextTag::builder().name("h1").scale(1.45).weight(700).pixels_above_lines(6).build());
        tag(&buf, gtk::TextTag::builder().name("h2").scale(1.25).weight(700).pixels_above_lines(4).build());
        tag(&buf, gtk::TextTag::builder().name("h3").scale(1.1).weight(700).build());
        tag(&buf, gtk::TextTag::builder().name("dim").foreground(p.fg_dim).build());
        tag(&buf, gtk::TextTag::builder().name("hit").background(&accent).foreground(p.bg).build());
        tag(&buf, gtk::TextTag::builder().name("find").background(p.warn).foreground(p.bg).build());
        for d in 1..=6 {
            tag(&buf, gtk::TextTag::builder().name(format!("indent{d}")).left_margin(12 + 22 * d).build());
            tag(&buf, gtk::TextTag::builder().name(format!("quote{d}")).left_margin(12 + 16 * d).foreground(p.fg_dim).build());
        }

        let r = Rc::new(Reader {
            root,
            stack: stack.clone(),
            subject,
            from,
            to,
            cc,
            date,
            place,
            pgp,
            notice,
            notice_btn: notice_btn.clone(),
            read_b: read_b.clone(),
            flag_b: flag_b.clone(),
            move_b: move_b.clone(),
            more_b: more_b.clone(),
            att_box,
            att_row,
            html_bar,
            mode_b: mode_b.clone(),
            online_b: online_b.clone(),
            find: find.clone(),
            find_count,
            view: view.clone(),
            buf,
            many,
            many_ids: RefCell::new(vec![]),
            cur: RefCell::new(None),
            html_mode: Cell::new(true),
            online: Cell::new(false),
            hl: RefCell::new(vec![]),
            links: RefCell::new(vec![]),
            link_tags: RefCell::new(vec![]),
            gen: Cell::new(0),
        });
        stack.set_visible_child_name("empty");

        // bulk actions for several selected messages
        {
            for (t, what) in [("mark read", "read"), ("mark unread", "unread"), ("flag", "flag"), ("move…", "move"), ("to shelf…", "shelf"), ("trash", "trash")] {
                let b = button_c(t, &["small"]);
                if what == "trash" {
                    b.add_css_class("danger");
                }
                let w = Rc::downgrade(&r);
                b.connect_clicked(move |b| {
                    let Some(r) = w.upgrade() else { return };
                    let ids = r.many_ids.borrow().clone();
                    match what {
                        "read" => act_keyword(ids, "$seen", true),
                        "unread" => act_keyword(ids, "$seen", false),
                        "flag" => act_keyword(ids, "$flagged", true),
                        "move" => move_menu(b.upcast_ref(), ids),
                        "shelf" => shelfui::save_messages(ids),
                        _ => act_trash(ids),
                    }
                });
                mrow.append(&b);
            }
        }
        let weak = Rc::downgrade(&r);
        let with = move |f: fn(&Rc<Reader>)| {
            let w = weak.clone();
            move |_: &gtk::Button| {
                if let Some(r) = w.upgrade() {
                    f(&r)
                }
            }
        };
        reply_b.connect_clicked(with(|r| r.act("reply")));
        all_b.connect_clicked(with(|r| r.act("reply-all")));
        fwd_b.connect_clicked(with(|r| r.act("forward")));
        read_b.connect_clicked(with(|r| r.act("read")));
        flag_b.connect_clicked(with(|r| r.act("flag")));
        trash_b.connect_clicked(with(|r| r.act("trash")));
        move_b.connect_clicked(with(|r| r.act("move")));
        mode_b.connect_clicked(with(|r| {
            r.html_mode.set(!r.html_mode.get());
            r.render();
        }));
        online_b.connect_clicked(with(|r| {
            r.online.set(true);
            r.render();
        }));
        browser_b.connect_clicked(with(|r| r.open_in_browser()));
        notice_btn.connect_clicked(with(|r| {
            let w = Rc::downgrade(r);
            crate::ui::ask_passphrase(move || {
                if let Some(r) = w.upgrade() {
                    if let Some(id) = r.current_id() {
                        let hl = r.hl.borrow().clone();
                        r.open(id, hl);
                    }
                }
            });
        }));
        {
            let w = Rc::downgrade(&r);
            find.connect_changed(move |_| {
                if let Some(r) = w.upgrade() {
                    r.apply_find();
                }
            });
        }
        // links: click opens, hover shows the hand
        {
            let g = gtk::GestureClick::new();
            g.set_button(1);
            let w = Rc::downgrade(&r);
            g.connect_released(move |g, _, x, y| {
                let Some(r) = w.upgrade() else { return };
                if r.buf.has_selection() {
                    return;
                }
                if let Some(url) = r.link_at(x, y) {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(rest) = url.strip_prefix("mailto:") {
                        compose::open(Kind::Mailto(format!("mailto:{rest}")));
                    } else if url.to_lowercase().starts_with("www.") {
                        open_uri(&format!("https://{url}"));
                    } else {
                        open_uri(&url);
                    }
                }
            });
            view.add_controller(g);
            let m = gtk::EventControllerMotion::new();
            let w = Rc::downgrade(&r);
            m.connect_motion(move |c, x, y| {
                let Some(r) = w.upgrade() else { return };
                let over = r.link_at(x, y);
                let wdg = c.widget().unwrap();
                wdg.set_cursor_from_name(if over.is_some() { Some("pointer") } else { Some("text") });
                wdg.set_tooltip_text(over.as_deref());
            });
            view.add_controller(m);
        }
        r
    }

    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (bx, by) = self.view.window_to_buffer_coords(gtk::TextWindowType::Widget, x as i32, y as i32);
        let it = self.view.iter_at_location(bx, by)?;
        for t in it.tags() {
            if let Some(n) = t.name() {
                if let Some(i) = n.strip_prefix("link") {
                    if let Ok(i) = i.parse::<usize>() {
                        return self.links.borrow().get(i).cloned();
                    }
                }
            }
        }
        None
    }

    pub fn current_id(&self) -> Option<i64> {
        self.cur.borrow().as_ref().map(|o| o.row.id)
    }

    pub fn clear(&self) {
        *self.cur.borrow_mut() = None;
        self.stack.set_visible_child_name("empty");
    }

    pub fn show_many(&self, ids: Vec<i64>) {
        self.many.set_text(&format!("{} messages selected", ids.len()));
        *self.many_ids.borrow_mut() = ids;
        self.stack.set_visible_child_name("many");
    }

    /// Opens a message; [hl] are search conditions to highlight.
    pub fn open(self: &Rc<Self>, id: i64, hl: Vec<Cond>) {
        let g = self.gen.get() + 1;
        self.gen.set(g);
        *self.hl.borrow_mut() = hl;
        if self.cur.borrow().as_ref().map(|o| o.row.id) != Some(id) {
            self.buf.set_text("");
            self.notice.set_text("opening…");
        }
        self.stack.set_visible_child_name("msg");
        let w = Rc::downgrade(self);
        run(eng().open(id), move |r| {
            let Some(me) = w.upgrade() else { return };
            if me.gen.get() != g {
                return;
            }
            match r {
                Ok(o) => {
                    let row_seen = o.row.seen();
                    let o = Rc::new(o);
                    // html by default where there is html, text in the junk folder
                    let junk = eng().db.mailbox_by_role(o.row.account, "junk").map(|j| o.row.mailboxes.contains(&j.id)).unwrap_or(false);
                    me.html_mode.set(o.parsed.html.is_some() && !junk && eng().db.setting_or("html.default", "html") == "html");
                    let allow: Vec<String> = eng().db.setting_or("html.allow", "").split_whitespace().map(String::from).collect();
                    *me.cur.borrow_mut() = Some(o.clone());
                    me.online.set(me.sender_domain().map(|d| allow.contains(&d)).unwrap_or(false));
                    me.show_headers();
                    me.show_attachments();
                    me.render();
                    if !row_seen && eng().db.flag("read.mark", true) {
                        act_keyword(vec![id], "$seen", true);
                    }
                }
                Err(e) => {
                    me.notice.set_text(&format!("cannot open: {}", err_text(&e)));
                    me.notice_btn.set_visible(false);
                }
            }
        });
    }

    fn sender_domain(&self) -> Option<String> {
        let c = self.cur.borrow();
        let o = c.as_ref()?;
        o.parsed.from.first().map(|a| a.domain()).filter(|d| !d.is_empty())
    }

    fn show_headers(&self) {
        let c = self.cur.borrow();
        let Some(o) = c.as_ref() else { return };
        let p = &o.parsed;
        self.subject.set_text(if p.subject.is_empty() { "(no subject)" } else { &p.subject });
        let from = maildot_core::model::Addr::join(&p.from);
        self.from.set_text(&if p.reply_to.is_empty() || p.reply_to == p.from { from } else { format!("{from}   (reply to {})", maildot_core::model::Addr::join(&p.reply_to)) });
        self.to.set_text(&format!("to {}", maildot_core::model::Addr::join(&p.to)));
        self.to.set_visible(!p.to.is_empty());
        self.cc.set_text(&format!("cc {}", maildot_core::model::Addr::join(&p.cc)));
        self.cc.set_visible(!p.cc.is_empty());
        self.date.set_text(&long_date(p.date.unwrap_or(o.row.received)));
        let acc = eng().db.account(o.row.account).map(|a| a.name).unwrap_or_default();
        let folders: Vec<String> = o.row.mailboxes.iter().map(|m| eng().mailbox_name(o.row.account, m).to_lowercase()).collect();
        self.place.set_text(&format!("{} · {} · {}", folders.join(", "), acc, size(o.row.size)));
        for c in ["ok", "warn", "bad"] {
            self.pgp.remove_css_class(c);
        }
        match &o.note {
            Some(n) => {
                self.pgp.set_text(&n.line);
                self.pgp.add_css_class(&n.level);
                self.pgp.set_visible(true);
            }
            None => self.pgp.set_visible(false),
        }
        let locked = o.row.fetched == 2;
        let note = if locked { o.err.clone().unwrap_or_else(|| "encrypted".into()) } else { String::new() };
        self.notice.set_text(&note);
        self.notice.set_visible(!note.is_empty());
        self.notice_btn.set_visible(locked && eng().locked());
        drop(c);
        self.refresh_flags();
    }

    /// Read / flag buttons follow the database (another device may have changed them).
    pub fn refresh_flags(&self) {
        let Some(id) = self.current_id() else { return };
        let Some(m) = eng().db.msg(id) else { return };
        self.read_b.set_label(if m.seen() { "unread" } else { "read" });
        self.flag_b.set_label(if m.flagged() { "unflag" } else { "flag" });
        // the more menu, built for this message
        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        let b = vbox(2);
        let item = |text: &str, f: Box<dyn Fn()>| {
            let bt = button_c(text, &["flat"]);
            if let Some(l) = bt.child().and_downcast::<gtk::Label>() {
                l.set_xalign(0.0);
            }
            let p = pop.clone();
            bt.connect_clicked(move |_| {
                p.popdown();
                f()
            });
            b.append(&bt);
        };
        let c = self.cur.borrow().clone();
        if let Some(o) = c {
            let o2 = o.clone();
            item("headers", Box::new(move || text_window("headers", &maildot_core::mime::raw_headers(&o2.raw))));
            let o2 = o.clone();
            item("source", Box::new(move || text_window("source", &String::from_utf8_lossy(&o2.raw[..o2.raw.len().min(4 << 20)]))));
            let o2 = o.clone();
            item("save as .eml", Box::new(move || save_data(&format!("{}.eml", if o2.parsed.subject.is_empty() { "message" } else { &o2.parsed.subject }), o2.raw.clone())));
            let id2 = o.row.id;
            item("message to shelf…", Box::new(move || shelfui::save_messages(vec![id2])));
            let row = o.row.clone();
            item("filter rule from this…", Box::new(move || sieveview::open_with_rule(row.account, Some(&row))));
            let id2 = o.row.id;
            item("as new message (edit again)", Box::new(move || compose::open(Kind::Draft(id2))));
            let mid = o.parsed.message_id.clone().unwrap_or_default();
            item("copy message id", Box::new(move || copy_text(&mid)));
            let path = maildot_core::paths::eml_path(o.row.account, o.row.id);
            item("show the stored file", Box::new(move || {
                if let Some(d) = path.parent() {
                    open_path(d)
                }
            }));
        }
        pop.set_child(Some(&b));
        self.more_b.set_popover(Some(&pop));
    }

    fn show_attachments(&self) {
        while let Some(c) = self.att_box.first_child() {
            self.att_box.remove(&c);
        }
        let c = self.cur.borrow().clone();
        let Some(o) = c else { return };
        let atts = &o.parsed.attachments;
        self.att_row.set_visible(!atts.is_empty());
        for (i, a) in atts.iter().enumerate() {
            let b = hbox(4);
            b.add_css_class("att");
            let l = label(&format!("{}  {}", a.name, size(a.data.len() as i64)));
            l.set_tooltip_text(Some(&a.mime));
            l.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
            l.set_max_width_chars(40);
            b.append(&l);
            let open_b = named(button_c("open", &["small", "flat"]), &format!("att-open-{i}"));
            let save_b = named(button_c("save", &["small", "flat"]), &format!("att-save-{i}"));
            let shelf_b = named(button_c("shelf", &["small", "flat"]), &format!("att-shelf-{i}"));
            b.append(&open_b);
            b.append(&save_b);
            b.append(&shelf_b);
            let (n, d) = (a.name.clone(), a.data.clone());
            open_b.connect_clicked(move |_| open_data(&n, &d));
            let (n, d) = (a.name.clone(), a.data.clone());
            save_b.connect_clicked(move |_| save_data(&n, d.clone()));
            let o2 = o.clone();
            shelf_b.connect_clicked(move |_| {
                let a = &o2.parsed.attachments[i];
                shelfui::save_files(vec![(a.name.clone(), a.data.clone())], o2.row.received);
            });
            self.att_box.append(&b);
        }
        if atts.len() > 1 {
            let all = button_c("all to shelf", &["small"]);
            let o2 = o.clone();
            all.connect_clicked(move |_| shelfui::save_files(o2.parsed.attachments.iter().map(|a| (a.name.clone(), a.data.clone())).collect(), o2.row.received));
            self.att_box.append(&all);
        }
    }

    pub fn rerender(self: &Rc<Self>) {
        let p = crate::style::palette();
        let table = self.buf.tag_table();
        if let Some(t) = table.lookup("hit") {
            t.set_background(Some(&crate::style::accent()));
            t.set_foreground(Some(p.bg));
        }
        if let Some(t) = table.lookup("dim") {
            t.set_foreground(Some(p.fg_dim));
        }
        for d in 1..=6 {
            if let Some(t) = table.lookup(&format!("quote{d}")) {
                t.set_foreground(Some(p.fg_dim));
            }
        }
        if self.cur.borrow().is_some() {
            self.render();
        }
    }

    fn new_link(&self, url: &str) -> gtk::TextTag {
        let i = self.links.borrow().len();
        self.links.borrow_mut().push(url.to_string());
        let t = gtk::TextTag::builder().name(format!("link{i}")).foreground(crate::style::accent()).underline(gtk::pango::Underline::Single).build();
        self.buf.tag_table().add(&t);
        self.link_tags.borrow_mut().push(t.clone());
        t
    }

    fn reset_buffer(&self) {
        self.buf.set_text("");
        let table = self.buf.tag_table();
        for t in self.link_tags.borrow_mut().drain(..) {
            table.remove(&t);
        }
        self.links.borrow_mut().clear();
    }

    /// The body, as text or as laid-out HTML.
    fn render(self: &Rc<Self>) {
        let c = self.cur.borrow().clone();
        let Some(o) = c else { return };
        self.reset_buffer();
        let has_html = o.parsed.html.is_some();
        self.html_bar.set_visible(true);
        // the html part of the bar only for html mail; find is always there
        for c in children(&self.html_bar) {
            let n = c.widget_name();
            if n == "html-label" || n.ends_with("-browser") {
                c.set_visible(has_html);
            }
        }
        self.mode_b.set_visible(has_html);
        self.mode_b.set_label(if self.html_mode.get() { "show text" } else { "show html" });
        let html_now = has_html && self.html_mode.get();
        if html_now {
            let html = o.parsed.html.clone().unwrap();
            let g = self.gen.get();
            let w = Rc::downgrade(self);
            blocking(move || html::render(&html), move |runs| {
                let Some(me) = w.upgrade() else { return };
                if me.gen.get() != g {
                    return;
                }
                let remote = html::has_remote(&runs);
                me.online_b.set_visible(remote && !me.online.get());
                me.insert_runs(&runs);
                me.after_render();
            });
        } else {
            self.online_b.set_visible(false);
            let text = o.parsed.text.clone().or_else(|| o.parsed.html.as_ref().map(|h| html::to_text(h))).unwrap_or_default();
            self.insert_text(&text);
            self.after_render();
        }
    }

    fn after_render(&self) {
        let hl = self.hl.borrow().clone();
        if !hl.is_empty() {
            let (s, e) = self.buf.bounds();
            let text = self.buf.text(&s, &e, true).to_string();
            let ranges = search::ranges_in(&text, &hl);
            self.apply_ranges(&text, &ranges, "hit", true);
        }
        self.apply_find();
        if hl.is_empty() {
            let mut it = self.buf.start_iter();
            self.view.scroll_to_iter(&mut it, 0.0, false, 0.0, 0.0);
        }
    }

    /// Byte ranges of [text] (the buffer's text) tagged; scrolls to the first when asked.
    fn apply_ranges(&self, text: &str, ranges: &[(usize, usize)], tag: &str, scroll: bool) {
        let (s, e) = self.buf.bounds();
        self.buf.remove_tag_by_name(tag, &s, &e);
        let mut first = None;
        // byte offsets → char offsets in one pass
        let mut char_at = Vec::with_capacity(ranges.len() * 2);
        let mut points: Vec<usize> = ranges.iter().flat_map(|(a, b)| [*a, *b]).collect();
        points.sort();
        points.dedup();
        let mut ci = 0usize;
        let mut pi = 0usize;
        for (bi, _) in text.char_indices().chain(std::iter::once((text.len(), ' '))) {
            while pi < points.len() && points[pi] <= bi {
                char_at.push((points[pi], ci));
                pi += 1;
            }
            ci += 1;
        }
        let conv = |b: usize| char_at.iter().find(|(x, _)| *x == b).map(|(_, c)| *c as i32);
        for &(a, b) in ranges {
            if let (Some(ca), Some(cb)) = (conv(a), conv(b)) {
                let ia = self.buf.iter_at_offset(ca);
                let ib = self.buf.iter_at_offset(cb);
                self.buf.apply_tag_by_name(tag, &ia, &ib);
                if first.is_none() {
                    first = Some(ca);
                }
            }
        }
        if scroll {
            if let Some(f) = first {
                let mut it = self.buf.iter_at_offset(f);
                let v = self.view.clone();
                glib::idle_add_local_once(move || {
                    v.scroll_to_iter(&mut it, 0.1, true, 0.0, 0.3);
                });
            }
        }
    }

    fn apply_find(&self) {
        let q = self.find.text().to_string();
        let (s, e) = self.buf.bounds();
        if q.is_empty() {
            self.buf.remove_tag_by_name("find", &s, &e);
            self.find_count.set_text("");
            return;
        }
        let text = self.buf.text(&s, &e, true).to_string();
        let ranges = search::ranges_in(&text, &[Cond::text(&q)]);
        self.find_count.set_text(&format!("{}", ranges.len()));
        self.apply_ranges(&text, &ranges, "find", true);
    }

    fn insert_text(&self, text: &str) {
        let re = url_re();
        let mut end = self.buf.end_iter();
        for line in text.replace("\r\n", "\n").split_inclusive('\n') {
            let depth = line.chars().take_while(|c| *c == '>' || *c == ' ').filter(|c| *c == '>').count().min(6);
            let mut tags: Vec<gtk::TextTag> = Vec::new();
            if depth > 0 {
                if let Some(t) = self.buf.tag_table().lookup(&format!("quote{depth}")) {
                    tags.push(t);
                }
            }
            let mut pos = 0;
            for m in re.find_iter(line) {
                let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                self.buf.insert_with_tags(&mut end, &line[pos..m.start()], &refs);
                let lt = self.new_link(m.as_str());
                let mut all = refs.clone();
                all.push(&lt);
                self.buf.insert_with_tags(&mut end, m.as_str(), &all);
                pos = m.end();
            }
            let refs: Vec<&gtk::TextTag> = tags.iter().collect();
            self.buf.insert_with_tags(&mut end, &line[pos..], &refs);
        }
    }

    fn insert_runs(self: &Rc<Self>, runs: &[Run]) {
        let table = self.buf.tag_table();
        let c = self.cur.borrow().clone();
        let Some(o) = c else { return };
        let mut end = self.buf.end_iter();
        for r in runs {
            match r {
                Run::Text(t, st) => {
                    let mut tags: Vec<gtk::TextTag> = Vec::new();
                    let mut add = |n: &str| {
                        if let Some(t) = table.lookup(n) {
                            tags.push(t)
                        }
                    };
                    if st.bold {
                        add("b");
                    }
                    if st.italic {
                        add("i");
                    }
                    if st.underline {
                        add("u");
                    }
                    if st.strike {
                        add("s");
                    }
                    if st.small {
                        add("small");
                    }
                    match st.heading {
                        1 => add("h1"),
                        2 => add("h2"),
                        3 => add("h3"),
                        _ => {}
                    }
                    if st.quote > 0 {
                        add(&format!("quote{}", st.quote.min(6)));
                    } else if st.indent > 0 {
                        add(&format!("indent{}", st.indent.min(6)));
                    }
                    if let Some(l) = &st.link {
                        tags.push(self.new_link(l));
                    }
                    let refs: Vec<&gtk::TextTag> = tags.iter().collect();
                    self.buf.insert_with_tags(&mut end, t, &refs);
                }
                Run::Rule => {
                    let dim = table.lookup("dim").unwrap();
                    self.buf.insert_with_tags(&mut end, "────────────────────────────────", &[&dim]);
                }
                Run::Image { src, alt, width, height, link } => {
                    let cid = src.strip_prefix("cid:").map(|c| c.trim_matches(|x| x == '<' || x == '>').to_string());
                    let data = cid.as_ref().and_then(|c| o.parsed.inline.iter().chain(o.parsed.attachments.iter()).find(|p| p.cid.as_deref() == Some(c.as_str())).map(|p| p.data.clone()));
                    let data = data.or_else(|| {
                        // data: URIs
                        let rest = src.strip_prefix("data:")?;
                        let (_, b64) = rest.split_once(";base64,")?;
                        use base64::Engine as _;
                        base64::engine::general_purpose::STANDARD.decode(b64.trim()).ok()
                    });
                    let remote = src.starts_with("http://") || src.starts_with("https://") || src.starts_with("//");
                    if data.is_some() || (remote && self.online.get()) {
                        let anchor = self.buf.create_child_anchor(&mut end);
                        let pic = gtk::Picture::new();
                        pic.set_can_shrink(true);
                        pic.set_content_fit(gtk::ContentFit::ScaleDown);
                        if let Some(l) = link {
                            pic.set_tooltip_text(Some(l));
                            let g = gtk::GestureClick::new();
                            let l2 = l.clone();
                            g.connect_released(move |_, _, _, _| open_uri(&l2));
                            pic.add_controller(g);
                            pic.set_cursor_from_name(Some("pointer"));
                        }
                        let (w0, h0) = (width.unwrap_or(0), height.unwrap_or(0));
                        let set = {
                            let pic = pic.clone();
                            move |bytes: Vec<u8>| {
                                if let Ok(t) = gdk::Texture::from_bytes(&glib::Bytes::from(&bytes)) {
                                    let (tw, th) = (t.width(), t.height());
                                    let (mut w, mut h) = if w0 > 0 && h0 > 0 { (w0, h0) } else { (tw, th) };
                                    if w > 640 {
                                        h = h * 640 / w.max(1);
                                        w = 640;
                                    }
                                    pic.set_size_request(w.max(1), h.max(1));
                                    pic.set_paintable(Some(&t));
                                }
                            }
                        };
                        match data {
                            Some(d) => set(d),
                            None => {
                                pic.set_size_request(w0.clamp(16, 640), h0.clamp(16, 480));
                                let url = src.clone();
                                run(async move { eng().fetch_bytes(&url, 10 << 20).await }, move |r| {
                                    if let Ok(d) = r {
                                        set(d)
                                    }
                                });
                            }
                        }
                        self.view.add_child_at_anchor(&pic, &anchor);
                        end = self.buf.end_iter();
                    } else {
                        let dim = table.lookup("dim").unwrap();
                        let t = if alt.is_empty() { "[image]".to_string() } else { format!("[{alt}]") };
                        match link {
                            Some(l) => {
                                let lt = self.new_link(l);
                                self.buf.insert_with_tags(&mut end, &t, &[&dim, &lt]);
                            }
                            None => self.buf.insert_with_tags(&mut end, &t, &[&dim]),
                        }
                    }
                }
            }
        }
    }

    fn open_in_browser(&self) {
        let c = self.cur.borrow().clone();
        let Some(o) = c else { return };
        let Some(h) = &o.parsed.html else {
            toast("this message has no html");
            return;
        };
        let parts = o.parsed.inline.clone();
        let atts = o.parsed.attachments.clone();
        let page = html::sandboxed(
            h,
            &|cid: &str| parts.iter().chain(atts.iter()).find(|p| p.cid.as_deref() == Some(cid)).map(|p| (p.mime.clone(), p.data.clone())),
            self.online.get(),
        );
        open_data("message.html", page.as_bytes());
    }

    /// reply / reply-all / forward / read / flag / move / trash on the open message.
    pub fn act(self: &Rc<Self>, what: &str) {
        let Some(id) = self.current_id() else { return };
        match what {
            "reply" => compose::open(Kind::Reply(id)),
            "reply-all" => compose::open(Kind::ReplyAll(id)),
            "forward" => compose::open(Kind::Forward(id)),
            "read" => {
                let seen = eng().db.msg(id).map(|m| m.seen()).unwrap_or(false);
                act_keyword(vec![id], "$seen", !seen);
                let w = Rc::downgrade(self);
                glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                    if let Some(r) = w.upgrade() {
                        r.refresh_flags()
                    }
                });
            }
            "flag" => {
                let f = eng().db.msg(id).map(|m| m.flagged()).unwrap_or(false);
                act_keyword(vec![id], "$flagged", !f);
                let w = Rc::downgrade(self);
                glib::timeout_add_local_once(std::time::Duration::from_millis(150), move || {
                    if let Some(r) = w.upgrade() {
                        r.refresh_flags()
                    }
                });
            }
            "move" => move_menu(self.move_b.upcast_ref(), vec![id]),
            "trash" => act_trash(vec![id]),
            _ => {}
        }
    }
}

/// A window with a lot of text to read or copy (headers, source).
pub fn text_window(title: &str, text: &str) {
    let w = gtk::Window::builder().title(title).default_width(900).default_height(640).build();
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let v = gtk::TextView::new();
    v.set_editable(false);
    v.set_monospace(true);
    v.set_wrap_mode(gtk::WrapMode::Char);
    v.set_left_margin(8);
    v.set_top_margin(6);
    v.buffer().set_text(text);
    w.set_child(Some(&scrolled(&v)));
    w.present();
}
