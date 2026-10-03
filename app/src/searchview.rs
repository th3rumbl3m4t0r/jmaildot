//! The search page: conditions (pattern; plain text or regex; case; whole word; NOT; which
//! fields), the scope (account, folder, dates, unread / flagged / attachments), results with
//! the matching text marked, and the reader with every match highlighted.

use crate::mail::row_factory;
use crate::reader::Reader;
use crate::util::*;
use gtk::prelude::*;
use gtk::{gio, glib};
use maildot_core::model::MsgRow;
use maildot_core::search::{self, Cond, Field, Query};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct CondRow {
    root: gtk::Box,
    pattern: gtk::Entry,
    regex: gtk::ToggleButton,
    case: gtk::ToggleButton,
    word: gtk::ToggleButton,
    negate: gtk::ToggleButton,
    fields: Vec<(Field, gtk::ToggleButton)>,
}

impl CondRow {
    fn cond(&self) -> Cond {
        Cond {
            pattern: self.pattern.text().to_string(),
            regex: self.regex.is_active(),
            case: self.case.is_active(),
            word: self.word.is_active(),
            negate: self.negate.is_active(),
            fields: self.fields.iter().filter(|(_, b)| b.is_active()).map(|(f, _)| *f).collect(),
        }
    }
}

pub struct SearchView {
    pub root: gtk::Paned,
    conds_box: gtk::Box,
    conds: RefCell<Vec<Rc<CondRow>>>,
    account_dd: gtk::DropDown,
    folder_dd: gtk::DropDown,
    folder_keys: RefCell<Vec<Option<(i64, String)>>>,
    since: gtk::Entry,
    before: gtk::Entry,
    unread: gtk::CheckButton,
    flagged: gtk::CheckButton,
    atts: gtk::CheckButton,
    stats: gtk::Label,
    error: gtk::Label,
    store: gio::ListStore,
    sel: gtk::SingleSelection,
    pub reader: Rc<Reader>,
    cancel: RefCell<Option<Arc<AtomicBool>>>,
    gen: Cell<u64>,
    last_conds: RefCell<Vec<Cond>>,
}

fn toggle(text: &str, tip: &str, on: bool) -> gtk::ToggleButton {
    let t = gtk::ToggleButton::with_label(text);
    t.add_css_class("small");
    t.set_tooltip_text(Some(tip));
    t.set_active(on);
    t.set_focus_on_click(false);
    t
}

fn parse_day(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let d = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%d.%m.%Y"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(&format!("{s}-01"), "%Y-%m-%d"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(&format!("{s}-01-01"), "%Y-%m-%d"))
        .ok()?;
    Some(maildot_core::ical::midnight(d, maildot_core::ical::local_tz()))
}

impl SearchView {
    pub fn new() -> Rc<SearchView> {
        let left = vbox(0);
        left.add_css_class("pane");
        let (tb, _) = titlebar("search everything stored here");
        left.append(&tb);
        let form = vbox(6);
        form.set_margin_start(8);
        form.set_margin_end(8);
        form.set_margin_top(8);
        let conds_box = vbox(6);
        form.append(&conds_box);
        let add_b = named(button_c("+ condition (and)", &["small"]), "s-add");
        add_b.set_halign(gtk::Align::Start);
        form.append(&add_b);

        form.append(&lbl("where"));
        let accounts = eng().accounts();
        let mut names = vec!["all accounts".to_string()];
        names.extend(accounts.iter().map(|a| a.name.clone()));
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        let account_dd = named(dropdown(&refs, 0), "s-account");
        let folder_dd = named(dropdown(&["all folders"], 0), "s-folder");
        let row = hbox(6);
        row.append(&account_dd);
        row.append(&folder_dd);
        form.append(&row);
        let drow = hbox(6);
        let since = named(entry("since 2025-01-01"), "s-since");
        let before = named(entry("before 2026-12-31"), "s-before");
        since.set_width_chars(14);
        before.set_width_chars(14);
        drow.append(&since);
        drow.append(&before);
        form.append(&drow);
        let frow = hbox(10);
        let unread = gtk::CheckButton::with_label("unread");
        let flagged = gtk::CheckButton::with_label("flagged");
        let atts = gtk::CheckButton::with_label("has attachments");
        frow.append(&unread);
        frow.append(&flagged);
        frow.append(&atts);
        form.append(&frow);
        let brow = hbox(6);
        let go = named(button_c("search", &["primary"]), "s-go");
        let clear = button_c("clear", &["small"]);
        brow.append(&go);
        brow.append(&clear);
        form.append(&brow);
        let error = label_c("", &["bad", "small"]);
        error.set_wrap(true);
        form.append(&error);
        let stats = named(label_c("", &["dim", "small"]), "s-stats");
        stats.set_wrap(true);
        form.append(&stats);
        let help = label_c(
            "plain text matches as typed (any case unless Aa); regex is Rust syntax: \\d, \\b, (?i), a|b, [^x]…  conditions all have to hold; not = must not match.",
            &["dim", "small"],
        );
        help.set_wrap(true);
        help.set_margin_top(10);
        form.append(&help);
        left.append(&scrolled(&form));
        left.set_size_request(380, -1);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sel = gtk::SingleSelection::new(Some(store.clone()));
        sel.set_autoselect(false);
        let list = gtk::ListView::new(Some(sel.clone()), Some(row_factory(Rc::new(Cell::new(true)), true)));
        list.set_widget_name("s-results");
        let mid = vbox(0);
        let (rtb, _) = titlebar("results");
        mid.append(&rtb);
        mid.append(&scrolled(&list));
        mid.set_size_request(320, -1);

        let reader = Reader::new("search");
        let p2 = gtk::Paned::new(gtk::Orientation::Horizontal);
        p2.set_start_child(Some(&mid));
        p2.set_end_child(Some(&reader.root));
        p2.set_position(460);
        p2.set_shrink_start_child(false);
        let root = gtk::Paned::new(gtk::Orientation::Horizontal);
        root.set_start_child(Some(&left));
        root.set_end_child(Some(&p2));
        root.set_position(420);
        root.set_shrink_start_child(false);

        let v = Rc::new(SearchView {
            root,
            conds_box,
            conds: RefCell::new(vec![]),
            account_dd: account_dd.clone(),
            folder_dd: folder_dd.clone(),
            folder_keys: RefCell::new(vec![None]),
            since: since.clone(),
            before: before.clone(),
            unread,
            flagged,
            atts,
            stats,
            error,
            store,
            sel: sel.clone(),
            reader,
            cancel: RefCell::new(None),
            gen: Cell::new(0),
            last_conds: RefCell::new(vec![]),
        });
        v.add_cond();
        {
            let w = Rc::downgrade(&v);
            add_b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.add_cond();
                }
            });
        }
        {
            let w = Rc::downgrade(&v);
            go.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.run();
                }
            });
            for e in [&since, &before] {
                let g = go.clone();
                e.connect_activate(move |_| g.emit_clicked());
            }
        }
        {
            let w = Rc::downgrade(&v);
            clear.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    for c in v.conds.borrow().iter() {
                        v.conds_box.remove(&c.root);
                    }
                    v.conds.borrow_mut().clear();
                    v.add_cond();
                    v.store.remove_all();
                    v.stats.set_text("");
                    v.error.set_text("");
                }
            });
        }
        {
            let w = Rc::downgrade(&v);
            account_dd.connect_selected_notify(move |_| {
                if let Some(v) = w.upgrade() {
                    v.fill_folders();
                }
            });
        }
        {
            let w = Rc::downgrade(&v);
            sel.connect_selection_changed(move |s, _, _| {
                let Some(v) = w.upgrade() else { return };
                if let Some(o) = s.selected_item().and_downcast::<glib::BoxedAnyObject>() {
                    let id = o.borrow::<(MsgRow, String)>().0.id;
                    let hl = v.last_conds.borrow().clone();
                    v.reader.open(id, hl);
                }
            });
        }
        v
    }

    pub fn focus(&self) {
        if let Some(c) = self.conds.borrow().first() {
            c.pattern.grab_focus();
        }
    }

    fn fill_folders(&self) {
        let accounts = eng().accounts();
        let i = self.account_dd.selected() as usize;
        let mut keys: Vec<Option<(i64, String)>> = vec![None];
        let mut names = vec!["all folders".to_string()];
        if i > 0 {
            if let Some(a) = accounts.get(i - 1) {
                for (m, depth) in crate::mail::tree(&eng().db.mailboxes(a.id)) {
                    names.push(format!("{}{}", "  ".repeat(depth), m.name.to_lowercase()));
                    keys.push(Some((a.id, m.id)));
                }
            }
        }
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        self.folder_dd.set_model(Some(&gtk::StringList::new(&refs)));
        self.folder_dd.set_selected(0);
        *self.folder_keys.borrow_mut() = keys;
    }

    /// Refreshes the account list (after accounts were added or removed).
    pub fn accounts_changed(&self) {
        let mut names = vec!["all accounts".to_string()];
        names.extend(eng().accounts().iter().map(|a| a.name.clone()));
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        self.account_dd.set_model(Some(&gtk::StringList::new(&refs)));
        self.account_dd.set_selected(0);
        self.fill_folders();
    }

    fn add_cond(self: &Rc<Self>) {
        let n = self.conds.borrow().len();
        let root = vbox(4);
        root.add_css_class("cond");
        let top = hbox(4);
        let pattern = named(entry(if n == 0 { "text or regex…" } else { "and…" }), &format!("s-pattern{n}"));
        pattern.set_hexpand(true);
        top.append(&pattern);
        let rm = button_c("×", &["small", "flat"]);
        if n > 0 {
            top.append(&rm);
        }
        root.append(&top);
        let opts = hbox(3);
        let regex = named(toggle("regex", "a regular expression instead of plain text", false), &format!("s-regex{n}"));
        let case = named(toggle("Aa", "case sensitive", false), &format!("s-case{n}"));
        let word = named(toggle("word", "whole words only", false), &format!("s-word{n}"));
        let negate = named(toggle("not", "the message must NOT match", false), &format!("s-not{n}"));
        opts.append(&regex);
        opts.append(&case);
        opts.append(&word);
        opts.append(&negate);
        root.append(&opts);
        let fl = hbox(3);
        let mut fields = Vec::new();
        for f in Field::ALL {
            let on = !matches!(f, Field::Headers);
            let t = named(toggle(f.label(), &format!("look in {}", f.label()), on), &format!("s-f{n}-{}", f.label().replace('/', "")));
            fl.append(&t);
            fields.push((f, t));
        }
        root.append(&fl);
        self.conds_box.append(&root);
        let row = Rc::new(CondRow { root, pattern: pattern.clone(), regex, case, word, negate, fields });
        self.conds.borrow_mut().push(row.clone());
        {
            let w = Rc::downgrade(self);
            pattern.connect_activate(move |_| {
                if let Some(v) = w.upgrade() {
                    v.run();
                }
            });
        }
        {
            let w = Rc::downgrade(self);
            let r2 = Rc::downgrade(&row);
            rm.connect_clicked(move |_| {
                if let (Some(v), Some(r)) = (w.upgrade(), r2.upgrade()) {
                    v.conds_box.remove(&r.root);
                    v.conds.borrow_mut().retain(|x| !Rc::ptr_eq(x, &r));
                }
            });
        }
        pattern.grab_focus();
    }

    fn query(&self) -> Result<Query, String> {
        let conds: Vec<Cond> = self.conds.borrow().iter().map(|c| c.cond()).filter(|c| !c.pattern.is_empty()).collect();
        for c in &conds {
            if c.fields.is_empty() {
                return Err(format!("\"{}\": pick at least one field", c.pattern));
            }
            if let Err(e) = search::compile(c) {
                return Err(format!("\"{}\": {e}", c.pattern));
            }
        }
        let accounts = eng().accounts();
        let ai = self.account_dd.selected() as usize;
        let mut q = Query { conds, limit: 5000, ..Default::default() };
        if ai > 0 {
            if let Some(a) = accounts.get(ai - 1) {
                q.accounts = vec![a.id];
            }
        }
        q.mailbox = self.folder_keys.borrow().get(self.folder_dd.selected() as usize).cloned().flatten();
        let s = self.since.text().to_string();
        let b = self.before.text().to_string();
        q.since = parse_day(&s);
        q.before = parse_day(&b);
        if !s.trim().is_empty() && q.since.is_none() {
            return Err("since: write a date as 2026-10-03".into());
        }
        if !b.trim().is_empty() && q.before.is_none() {
            return Err("before: write a date as 2026-10-03".into());
        }
        q.unread = self.unread.is_active();
        q.flagged = self.flagged.is_active();
        q.attachments = self.atts.is_active();
        if q.conds.is_empty() && !q.unread && !q.flagged && !q.attachments && q.since.is_none() && q.before.is_none() && q.mailbox.is_none() {
            return Err("type something to look for".into());
        }
        Ok(q)
    }

    /// Runs the search on all cores; hits show up as they come, sorted at the end.
    pub fn run(self: &Rc<Self>) {
        self.error.set_text("");
        let q = match self.query() {
            Ok(q) => q,
            Err(e) => {
                self.error.set_text(&e);
                return;
            }
        };
        if let Some(c) = self.cancel.borrow_mut().take() {
            c.store(true, Ordering::Relaxed);
        }
        let cancel = Arc::new(AtomicBool::new(false));
        *self.cancel.borrow_mut() = Some(cancel.clone());
        *self.last_conds.borrow_mut() = q.conds.clone();
        let g = self.gen.get() + 1;
        self.gen.set(g);
        self.store.remove_all();
        self.stats.set_text("searching…");
        let (tx, rx) = async_channel::unbounded::<Vec<search::Hit>>();
        let path = eng().db.path().to_path_buf();
        let q2 = q.clone();
        let c2 = cancel.clone();
        let (done_tx, done_rx) = async_channel::bounded(1);
        eng().rt.spawn_blocking(move || {
            let r = search::search(&path, &q2, &c2, &|h| {
                let _ = tx.send_blocking(h);
            });
            drop(tx);
            let _ = done_tx.send_blocking(r.map_err(|e| e.to_string()));
        });
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut all: Vec<search::Hit> = Vec::new();
            while let Ok(batch) = rx.recv().await {
                let Some(v) = w.upgrade() else { return };
                if v.gen.get() != g {
                    return;
                }
                // show what is there while the rest is searched
                v.append_hits(&batch);
                all.extend(batch);
                v.stats.set_text(&format!("{} found so far…", all.len()));
            }
            let Some(v) = w.upgrade() else { return };
            if v.gen.get() != g {
                return;
            }
            match done_rx.recv().await {
                Ok(Ok(st)) => {
                    all.sort_by(|a, b| b.received.cmp(&a.received));
                    v.store.remove_all();
                    v.append_hits(&all);
                    let more = if st.matched > all.len() { format!(" (first {} shown)", all.len()) } else { String::new() };
                    v.stats.set_text(&format!(
                        "{} messages match{more} · {} searched, {} in {} ms on {} threads",
                        st.matched,
                        st.scanned,
                        size(st.bytes as i64),
                        st.millis,
                        st.threads
                    ));
                }
                Ok(Err(e)) => v.error.set_text(&e),
                Err(_) => {}
            }
        });
    }

    fn append_hits(&self, hits: &[search::Hit]) {
        let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
        let rows = eng().db.msgs(&ids);
        let by: std::collections::HashMap<i64, MsgRow> = rows.into_iter().map(|r| (r.id, r)).collect();
        let objs: Vec<glib::BoxedAnyObject> = hits
            .iter()
            .filter_map(|h| {
                let r = by.get(&h.id)?.clone();
                let field = h.field.map(|f| format!("{}: ", f.label())).unwrap_or_default();
                let count = if h.count > 1 { format!(" ({}×)", h.count) } else { String::new() };
                let markup = format!("<span alpha=\"70%\">{}</span>{}{}", markup_escape(&field), highlight(&h.snippet, &h.ranges), markup_escape(&count));
                Some(glib::BoxedAnyObject::new((r, markup)))
            })
            .collect();
        self.store.extend_from_slice(&objs);
    }
}
