//! The mail page: folders of every account (plus all inboxes and flagged), the message list
//! of the chosen one with a quick filter (the local search, scoped to the folder), and the reader.

use crate::reader::Reader;
use crate::util::*;
use crate::{compose, shelfui, sieveview};
use gtk::prelude::*;
use gtk::{gdk, gio, glib};
use maildot_core::model::{Mailbox, MsgRow};
use maildot_core::search::{self, Cond, Field, Query};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::atomic::AtomicBool;

#[derive(Clone, Debug, PartialEq)]
pub enum Sel {
    Inboxes,
    Flagged,
    Folder(i64, String),
}

pub struct MailView {
    pub root: gtk::Paned,
    folders: gtk::ListBox,
    folder_keys: RefCell<Vec<Option<Sel>>>,
    store: gio::ListStore,
    sel: gtk::MultiSelection,
    list: gtk::ListView,
    filter: gtk::Entry,
    title: gtk::Label,
    count: gtk::Label,
    pub reader: Rc<Reader>,
    current: RefCell<Sel>,
    refresh_pending: Cell<bool>,
    gen: Cell<u64>,
    ids: RefCell<HashMap<i64, u32>>,
    want_open: Cell<Option<i64>>,
}

fn role_rank(r: Option<&str>) -> i32 {
    match r {
        Some("inbox") => 0,
        Some("drafts") => 1,
        Some("sent") => 2,
        Some("archive") => 3,
        Some("junk") => 5,
        Some("trash") => 6,
        _ => 4,
    }
}

/// Folders in tree order with their depth.
pub fn tree(list: &[Mailbox]) -> Vec<(Mailbox, usize)> {
    fn walk(list: &[Mailbox], parent: Option<&str>, depth: usize, out: &mut Vec<(Mailbox, usize)>) {
        let mut kids: Vec<&Mailbox> = list.iter().filter(|m| m.parent.as_deref() == parent).collect();
        kids.sort_by(|a, b| role_rank(a.role.as_deref()).cmp(&role_rank(b.role.as_deref())).then(a.sort.cmp(&b.sort)).then(a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        for k in kids {
            out.push((k.clone(), depth));
            walk(list, Some(&k.id), depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(list, None, 0, &mut out);
    // orphans (parent unknown) at the end
    for m in list {
        if !out.iter().any(|(x, _)| x.id == m.id) {
            out.push((m.clone(), 0));
        }
    }
    out
}

pub fn row_obj(m: MsgRow) -> glib::BoxedAnyObject {
    glib::BoxedAnyObject::new(m)
}

/// What a message drag carries: the ids after a prefix, so text dropped from elsewhere is not taken for it.
const DRAG_PREFIX: &str = "maildot-ids:";

fn drag_text(ids: &[i64]) -> String {
    format!("{DRAG_PREFIX}{}", ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(","))
}

fn drag_ids(text: &str) -> Vec<i64> {
    text.strip_prefix(DRAG_PREFIX).map(|s| s.split(',').filter_map(|x| x.parse().ok()).collect()).unwrap_or_default()
}

fn row_id(o: &glib::BoxedAnyObject) -> Option<i64> {
    if let Ok(h) = o.try_borrow::<(MsgRow, String)>() {
        return Some(h.0.id);
    }
    o.try_borrow::<MsgRow>().ok().map(|m| m.id)
}

/// The messages a drag from this row takes: the selection when the row is in it, else the row alone.
fn dragged(item: &gtk::ListItem, root: &gtk::Widget) -> Vec<i64> {
    let Some(this) = item.item().and_downcast::<glib::BoxedAnyObject>().as_ref().and_then(row_id) else { return vec![] };
    if !item.is_selected() {
        return vec![this];
    }
    let Some(model) = root.ancestor(gtk::ListView::static_type()).and_downcast::<gtk::ListView>().and_then(|l| l.model()) else { return vec![this] };
    let bs = model.selection();
    (0..bs.size()).filter_map(|k| model.item(bs.nth(k as u32)).and_downcast::<glib::BoxedAnyObject>().as_ref().and_then(row_id)).collect()
}

/// The list row: dot, sender, marks, date / subject / preview. With [drag], rows can be dragged onto a folder.
pub fn row_factory(show_account: Rc<Cell<bool>>, snippet: bool, drag: bool) -> gtk::SignalListItemFactory {
    let f = gtk::SignalListItemFactory::new();
    f.connect_setup(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let root = vbox(0);
        root.add_css_class("msg");
        let l1 = hbox(6);
        let dot = label_c("●", &["unread-dot"]);
        let from = label_c("", &["from"]);
        from.set_hexpand(true);
        from.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let marks = label_c("", &["marks"]);
        let date = label_c("", &["dim", "small"]);
        l1.append(&dot);
        l1.append(&from);
        l1.append(&marks);
        l1.append(&date);
        let subject = label_c("", &["subject"]);
        subject.set_ellipsize(gtk::pango::EllipsizeMode::End);
        let preview = label_c("", &["preview"]);
        preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
        root.append(&l1);
        root.append(&subject);
        root.append(&preview);
        if snippet {
            preview.add_css_class("snippet");
        }
        if drag {
            let src = gtk::DragSource::new();
            src.set_actions(gdk::DragAction::MOVE);
            let n = Rc::new(Cell::new(0usize));
            let (it, r2, n2) = (item.downgrade(), root.downgrade(), n.clone());
            src.connect_prepare(move |_, _, _| {
                let ids = dragged(&it.upgrade()?, r2.upgrade()?.upcast_ref());
                if ids.is_empty() {
                    return None;
                }
                n2.set(ids.len());
                Some(gdk::ContentProvider::for_value(&drag_text(&ids).to_value()))
            });
            src.connect_drag_begin(move |_, d| {
                let n = n.get();
                let icon = gtk::DragIcon::for_drag(d);
                icon.set_child(Some(&label_c(&if n == 1 { "1 message".to_string() } else { format!("{n} messages") }, &["drag-icon"])));
            });
            root.add_controller(src);
        }
        item.set_child(Some(&root));
    });
    f.connect_bind(move |_, item| {
        let item = item.downcast_ref::<gtk::ListItem>().unwrap();
        let Some(obj) = item.item().and_downcast::<glib::BoxedAnyObject>() else { return };
        let Some(root) = item.child().and_downcast::<gtk::Box>() else { return };
        let kids = children(&root);
        let l1 = kids[0].clone().downcast::<gtk::Box>().unwrap();
        let k1 = children(&l1);
        let (dot, from, marks, date) = (
            k1[0].clone().downcast::<gtk::Label>().unwrap(),
            k1[1].clone().downcast::<gtk::Label>().unwrap(),
            k1[2].clone().downcast::<gtk::Label>().unwrap(),
            k1[3].clone().downcast::<gtk::Label>().unwrap(),
        );
        let subject = kids[1].clone().downcast::<gtk::Label>().unwrap();
        let preview = kids[2].clone().downcast::<gtk::Label>().unwrap();
        // a plain row, or a search hit (row + snippet markup)
        let (m, snip): (MsgRow, Option<String>) = if let Ok(h) = obj.try_borrow::<(MsgRow, String)>() {
            (h.0.clone(), Some(h.1.clone()))
        } else if let Ok(m) = obj.try_borrow::<MsgRow>() {
            (m.clone(), None)
        } else {
            return;
        };
        let outgoing = eng().db.mailboxes(m.account).iter().any(|b| m.mailboxes.contains(&b.id) && matches!(b.role.as_deref(), Some("sent") | Some("drafts")));
        let who = if outgoing && !m.to.is_empty() { format!("→ {}", m.to.iter().map(|a| a.short()).collect::<Vec<_>>().join(", ")) } else { m.from.short() };
        from.set_text(&who);
        from.set_tooltip_text(Some(&if outgoing { maildot_core::model::Addr::join(&m.to) } else { m.from.full() }));
        subject.set_text(if m.subject.is_empty() { "(no subject)" } else { &m.subject });
        date.set_text(&short_date(m.received));
        dot.set_opacity(if m.seen() { 0.0 } else { 1.0 });
        if m.seen() {
            root.remove_css_class("unread");
        } else {
            root.add_css_class("unread");
        }
        let mut mk = Vec::new();
        if show_account.get() {
            if let Some(a) = eng().db.account(m.account) {
                mk.push(crate::ui::short_name(&a.name));
            }
        }
        if m.encrypted {
            mk.push("pgp".into());
        }
        if m.has_att {
            mk.push("att".into());
        }
        if m.answered() {
            mk.push("re".into());
        }
        if m.fetched == 0 {
            mk.push("…".into());
        }
        if m.fetched == 2 {
            mk.push("locked".into());
        }
        if m.flagged() {
            mk.push("flag".into());
            marks.add_css_class("flagged");
        } else {
            marks.remove_css_class("flagged");
        }
        marks.set_text(&mk.join(" "));
        match snip {
            Some(markup) => {
                preview.set_markup(&markup);
                preview.set_visible(true);
            }
            None => {
                preview.set_text(&m.preview);
                preview.set_visible(!m.preview.is_empty() && eng().db.flag("list.preview", true));
            }
        }
    });
    f
}

impl MailView {
    pub fn new() -> Rc<MailView> {
        let folders = gtk::ListBox::new();
        folders.add_css_class("folders");
        folders.set_selection_mode(gtk::SelectionMode::Single);
        folders.set_widget_name("folders");
        let fwin = vbox(0);
        let (ftb, _) = titlebar("folders");
        fwin.append(&ftb);
        fwin.append(&scrolled(&folders));
        fwin.add_css_class("pane");
        fwin.set_size_request(190, -1);

        let store = gio::ListStore::new::<glib::BoxedAnyObject>();
        let sel = gtk::MultiSelection::new(Some(store.clone()));
        let show_account = Rc::new(Cell::new(false));
        let list = gtk::ListView::new(Some(sel.clone()), Some(row_factory(show_account.clone(), false, true)));
        list.set_widget_name("messages");
        let lwin = vbox(0);
        let (ltb, title) = titlebar("inbox");
        let count = label_c("", &["dim"]);
        ltb.append(&count);
        lwin.append(&ltb);
        let fb = hbox(4);
        fb.add_css_class("bar");
        let filter = named(entry("filter this folder…  (from, to, subject, text)"), "filter");
        filter.set_hexpand(true);
        fb.append(&filter);
        lwin.append(&fb);
        lwin.append(&scrolled(&list));
        lwin.set_size_request(320, -1);

        let reader = Reader::new("mail");
        let inner = gtk::Paned::new(gtk::Orientation::Horizontal);
        inner.set_start_child(Some(&lwin));
        inner.set_end_child(Some(&reader.root));
        inner.set_position(470);
        inner.set_shrink_start_child(false);
        inner.set_shrink_end_child(false);
        let root = gtk::Paned::new(gtk::Orientation::Horizontal);
        root.set_start_child(Some(&fwin));
        root.set_end_child(Some(&inner));
        root.set_position(220);
        root.set_shrink_start_child(false);
        root.set_resize_start_child(false);

        let v = Rc::new(MailView {
            root,
            folders: folders.clone(),
            folder_keys: RefCell::new(Vec::new()),
            store,
            sel: sel.clone(),
            list: list.clone(),
            filter: filter.clone(),
            title,
            count,
            reader,
            current: RefCell::new(Sel::Inboxes),
            refresh_pending: Cell::new(false),
            gen: Cell::new(0),
            ids: RefCell::new(HashMap::new()),
            want_open: Cell::new(None),
        });
        let _ = show_account;

        {
            let v2 = Rc::downgrade(&v);
            folders.connect_row_selected(move |_, row| {
                let (Some(v), Some(row)) = (v2.upgrade(), row) else { return };
                let key = v.folder_keys.borrow().get(row.index() as usize).cloned().flatten();
                if let Some(k) = key {
                    if *v.current.borrow() != k {
                        *v.current.borrow_mut() = k;
                        v.sel.unselect_all();
                        v.reload_list();
                    }
                }
            });
        }
        {
            let v2 = Rc::downgrade(&v);
            sel.connect_selection_changed(move |_, _, _| {
                if let Some(v) = v2.upgrade() {
                    v.selection_changed();
                }
            });
        }
        {
            let v2 = Rc::downgrade(&v);
            let pending = Rc::new(Cell::new(0u32));
            filter.connect_changed(move |_| {
                // a short pause after typing, then filter
                let g = pending.get() + 1;
                pending.set(g);
                let v3 = v2.clone();
                let p2 = pending.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(250), move || {
                    if p2.get() == g {
                        if let Some(v) = v3.upgrade() {
                            v.reload_list();
                        }
                    }
                });
            });
        }
        // keys on the list: Delete trash, m read, s flag, r reply, a reply all, f forward
        {
            let k = gtk::EventControllerKey::new();
            let v2 = Rc::downgrade(&v);
            k.connect_key_pressed(move |_, key, _, state| {
                let Some(v) = v2.upgrade() else { return glib::Propagation::Proceed };
                if state.intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK) {
                    return glib::Propagation::Proceed;
                }
                let act = match key {
                    gdk::Key::Delete | gdk::Key::KP_Delete => "trash",
                    gdk::Key::m => "read",
                    gdk::Key::s => "flag",
                    gdk::Key::r => "reply",
                    gdk::Key::a => "reply-all",
                    gdk::Key::f => "forward",
                    gdk::Key::j => "next",
                    gdk::Key::k => "prev",
                    _ => return glib::Propagation::Proceed,
                };
                v.act_selected(act);
                glib::Propagation::Stop
            });
            list.add_controller(k);
        }
        // right click: the message menu
        {
            let g = gtk::GestureClick::new();
            g.set_button(3);
            let v2 = Rc::downgrade(&v);
            g.connect_pressed(move |g, _, x, y| {
                let Some(v) = v2.upgrade() else { return };
                if v.selected().is_empty() {
                    return;
                }
                let w = g.widget().unwrap();
                v.context_menu(&w, x, y);
            });
            list.add_controller(g);
        }
        // messages dragged from the list onto a folder of their account are moved there
        {
            let drop = gtk::DropTarget::new(String::static_type(), gdk::DragAction::MOVE);
            drop.set_preload(true);
            let v2 = Rc::downgrade(&v);
            drop.connect_motion(move |t, _, y| {
                let Some(v) = v2.upgrade() else { return gdk::DragAction::empty() };
                // until the ids are loaded any folder row will do; the drop checks again
                let ok = |r: &gtk::ListBoxRow| match t.value().and_then(|x| x.get::<String>().ok()) {
                    Some(s) => v.drop_plan(r, &drag_ids(&s)).is_some(),
                    None => matches!(v.folder_keys.borrow().get(r.index() as usize), Some(Some(Sel::Folder(..)))),
                };
                match v.folders.row_at_y(y as i32).filter(|r| ok(r)) {
                    Some(r) => {
                        v.folders.drag_highlight_row(&r);
                        gdk::DragAction::MOVE
                    }
                    None => {
                        v.folders.drag_unhighlight_row();
                        gdk::DragAction::empty()
                    }
                }
            });
            let v2 = Rc::downgrade(&v);
            drop.connect_leave(move |_| {
                if let Some(v) = v2.upgrade() {
                    v.folders.drag_unhighlight_row();
                }
            });
            let v2 = Rc::downgrade(&v);
            drop.connect_drop(move |_, val, _, y| {
                let Some(v) = v2.upgrade() else { return false };
                v.folders.drag_unhighlight_row();
                let (Ok(s), Some(r)) = (val.get::<String>(), v.folders.row_at_y(y as i32)) else { return false };
                v.drop_on(&r, &drag_ids(&s))
            });
            folders.add_controller(drop);
        }
        v
    }

    /// Where messages dropped on a folder row go: the folder, and those of its account not there already.
    fn drop_plan(&self, row: &gtk::ListBoxRow, ids: &[i64]) -> Option<(i64, String, Vec<i64>)> {
        let Some(Some(Sel::Folder(account, mb))) = self.folder_keys.borrow().get(row.index() as usize).cloned() else { return None };
        let ids: Vec<i64> = eng().db.msgs(ids).into_iter().filter(|m| m.account == account && m.mailboxes != [mb.clone()]).map(|m| m.id).collect();
        (!ids.is_empty()).then_some((account, mb, ids))
    }

    /// Moves dropped messages to the folder of [row]; false when there is nothing to move there.
    pub fn drop_on(&self, row: &gtk::ListBoxRow, ids: &[i64]) -> bool {
        let Some((account, mb, moved)) = self.drop_plan(row, ids) else { return false };
        let name = eng().mailbox_name(account, &mb).to_lowercase();
        let others = eng().db.msgs(ids).iter().filter(|m| m.account != account).count();
        let n = moved.len();
        run(async move { eng().move_to(moved, account, &mb).await }, move |r| match r {
            Ok(()) if others > 0 => toast(&format!("{n} moved to {name} ({others} of other accounts stay)")),
            Ok(()) => toast(&format!("{n} moved to {name}")),
            Err(e) => toast_err(&err_text(&e)),
        });
        true
    }

    pub fn focus_filter(&self) {
        self.filter.grab_focus();
    }

    /// Rebuilds the folder list (names, counts), keeping the chosen one.
    pub fn reload_folders(&self) {
        let e = eng();
        let accounts = e.accounts();
        let cur = self.current.borrow().clone();
        while let Some(r) = self.folders.row_at_index(0) {
            self.folders.remove(&r);
        }
        let mut keys: Vec<Option<Sel>> = Vec::new();
        let all = e.db.all_mailboxes();
        let unread_inbox: i64 = all.iter().filter(|m| m.role.as_deref() == Some("inbox")).map(|m| m.unread).sum();
        let add = |text: &str, count: i64, depth: usize, key: Option<Sel>, header: bool, keys: &mut Vec<Option<Sel>>| {
            let row = gtk::ListBoxRow::new();
            let b = hbox(6);
            b.set_margin_start((depth * 12) as i32);
            let l = label(text);
            l.set_hexpand(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::End);
            b.append(&l);
            if count > 0 {
                b.append(&label_c(&count.to_string(), &["count", "unread"]));
            }
            row.set_child(Some(&b));
            if header {
                row.add_css_class("account");
                row.set_selectable(false);
                row.set_activatable(false);
            }
            if let Some(k) = &key {
                row.set_widget_name(&match k {
                    Sel::Inboxes => "f-inboxes".to_string(),
                    Sel::Flagged => "f-flagged".to_string(),
                    Sel::Folder(a, id) => format!("f-{a}-{id}"),
                });
            }
            self.folders.append(&row);
            keys.push(key);
        };
        add("all inboxes", unread_inbox, 0, Some(Sel::Inboxes), false, &mut keys);
        add("flagged", 0, 0, Some(Sel::Flagged), false, &mut keys);
        for a in &accounts {
            add(&a.name.to_uppercase(), 0, 0, None, true, &mut keys);
            let mbs: Vec<Mailbox> = all.iter().filter(|m| m.account == a.id).cloned().collect();
            for (m, depth) in tree(&mbs) {
                let count = if matches!(m.role.as_deref(), Some("sent") | Some("drafts") | Some("trash")) { 0 } else { m.unread };
                add(&m.name.to_lowercase(), count, depth + 1, Some(Sel::Folder(a.id, m.id.clone())), false, &mut keys);
            }
        }
        *self.folder_keys.borrow_mut() = keys;
        let idx = self.folder_keys.borrow().iter().position(|k| k.as_ref() == Some(&cur));
        if let Some(i) = idx {
            if let Some(r) = self.folders.row_at_index(i as i32) {
                self.folders.select_row(Some(&r));
            }
        }
    }

    pub fn select(&self, s: Sel) {
        let idx = self.folder_keys.borrow().iter().position(|k| k.as_ref() == Some(&s));
        *self.current.borrow_mut() = s;
        if let Some(i) = idx {
            if let Some(r) = self.folders.row_at_index(i as i32) {
                self.folders.select_row(Some(&r));
            }
        }
        self.reload_list();
    }

    /// Lots of changes come in during a sync: one refresh per quarter second at most.
    pub fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_pending.get() {
            return;
        }
        self.refresh_pending.set(true);
        let v = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
            if let Some(v) = v.upgrade() {
                v.refresh_pending.set(false);
                v.reload_folders();
                v.reload_list();
                v.reader.refresh_flags();
            }
        });
    }

    fn title_of(&self, s: &Sel) -> String {
        match s {
            Sel::Inboxes => "all inboxes".into(),
            Sel::Flagged => "flagged".into(),
            Sel::Folder(a, id) => {
                let acc = eng().db.account(*a).map(|x| crate::ui::short_name(&x.name)).unwrap_or_default();
                format!("{} · {}", eng().mailbox_name(*a, id).to_lowercase(), acc)
            }
        }
    }

    /// Loads the chosen folder (filtered when there is filter text) off the main thread.
    pub fn reload_list(&self) {
        let s = self.current.borrow().clone();
        self.title.set_text(&self.title_of(&s).to_uppercase());
        let text = self.filter.text().trim().to_string();
        let g = self.gen.get() + 1;
        self.gen.set(g);
        let db_path = eng().db.path().to_path_buf();
        let s2 = s.clone();
        blocking(
            move || {
                let e = eng();
                let rows = match &s2 {
                    Sel::Inboxes => e.db.list_role("inbox", 200_000),
                    Sel::Flagged => e.db.list_flagged(200_000),
                    Sel::Folder(a, id) => e.db.list(*a, id, 1_000_000),
                };
                if text.is_empty() {
                    return (rows, None);
                }
                let mut q = Query {
                    conds: text
                        .split_whitespace()
                        .map(|w| Cond { fields: vec![Field::Subject, Field::Sender, Field::Rcpts, Field::Body, Field::Attach], ..Cond::text(w) })
                        .collect(),
                    ..Default::default()
                };
                if let Sel::Folder(a, id) = &s2 {
                    q.mailbox = Some((*a, id.clone()));
                }
                let hits = std::sync::Mutex::new(HashSet::new());
                let _ = search::search(&db_path, &q, &AtomicBool::new(false), &|h| hits.lock().unwrap().extend(h.into_iter().map(|x| x.id)));
                let set = hits.into_inner().unwrap();
                let n = rows.len();
                (rows.into_iter().filter(|r| set.contains(&r.id)).collect(), Some(n))
            },
            move |(rows, of): (Vec<MsgRow>, Option<usize>)| {
                let u = crate::ui::ui();
                let v = &u.mail;
                if v.gen.get() != g {
                    return;
                }
                v.fill(rows, of);
            },
        );
    }

    fn fill(&self, rows: Vec<MsgRow>, of: Option<usize>) {
        let unread = rows.iter().filter(|r| !r.seen()).count();
        self.count.set_text(&match of {
            Some(n) => format!("{} of {n}", rows.len()),
            None if unread > 0 => format!("{} · {unread} unread", rows.len()),
            None => format!("{}", rows.len()),
        });
        let selected: Vec<i64> = self.selected().iter().map(|m| m.id).collect();
        // same messages in the same order: update in place (keeps scroll and selection)
        let same = rows.len() as u32 == self.store.n_items()
            && rows.iter().enumerate().all(|(i, r)| self.ids.borrow().get(&r.id) == Some(&(i as u32)));
        if same {
            for (i, r) in rows.into_iter().enumerate() {
                if let Some(o) = self.store.item(i as u32).and_downcast::<glib::BoxedAnyObject>() {
                    let changed = { o.borrow::<MsgRow>().keywords != r.keywords || o.borrow::<MsgRow>().fetched != r.fetched || o.borrow::<MsgRow>().preview != r.preview };
                    if changed {
                        o.replace(r);
                        self.store.items_changed(i as u32, 1, 1);
                    }
                }
            }
        } else {
            let mut ids = HashMap::new();
            let objs: Vec<glib::BoxedAnyObject> = rows
                .into_iter()
                .enumerate()
                .map(|(i, r)| {
                    ids.insert(r.id, i as u32);
                    row_obj(r)
                })
                .collect();
            *self.ids.borrow_mut() = ids;
            self.store.splice(0, self.store.n_items(), &objs);
            for id in &selected {
                if let Some(&i) = self.ids.borrow().get(id) {
                    self.sel.select_item(i, false);
                }
            }
        }
        if let Some(id) = self.want_open.take() {
            if let Some(&i) = self.ids.borrow().get(&id) {
                self.sel.select_item(i, true);
                self.list.scroll_to(i, gtk::ListScrollFlags::NONE, None);
            } else {
                self.reader.open(id, vec![]);
            }
        }
    }

    pub fn selected(&self) -> Vec<MsgRow> {
        let bs = self.sel.selection();
        let mut out = Vec::new();
        let n = bs.size();
        for k in 0..n {
            let i = bs.nth(k as u32);
            if let Some(o) = self.store.item(i).and_downcast::<glib::BoxedAnyObject>() {
                out.push(o.borrow::<MsgRow>().clone());
            }
        }
        out
    }

    fn selection_changed(&self) {
        let s = self.selected();
        match s.len() {
            0 => {}
            1 => {
                if self.reader.current_id() != Some(s[0].id) {
                    self.reader.open(s[0].id, vec![]);
                }
            }
            _ => self.reader.show_many(s.iter().map(|m| m.id).collect()),
        }
    }

    /// Opens a message (from a notification): its inbox is shown and it is selected.
    pub fn open_message(&self, id: i64) {
        let Some(m) = eng().db.msg(id) else { return };
        let target = m.mailboxes.first().map(|b| Sel::Folder(m.account, b.clone())).unwrap_or(Sel::Inboxes);
        self.want_open.set(Some(id));
        self.filter.set_text("");
        if *self.current.borrow() != target {
            self.select(target);
        } else {
            self.reload_list();
        }
    }

    /// An action on the selected messages (or the one in the reader).
    pub fn act_selected(&self, what: &str) {
        let sel = self.selected();
        let ids: Vec<i64> = if sel.is_empty() { self.reader.current_id().into_iter().collect() } else { sel.iter().map(|m| m.id).collect() };
        match what {
            "next" | "prev" => {
                let bs = self.sel.selection();
                let cur = if bs.size() > 0 { bs.nth(0) as i64 } else { -1 };
                let n = self.store.n_items() as i64;
                let to = if what == "next" { (cur + 1).min(n - 1) } else { (cur - 1).max(0) };
                if to >= 0 && n > 0 {
                    self.sel.select_item(to as u32, true);
                    self.list.scroll_to(to as u32, gtk::ListScrollFlags::NONE, None);
                }
            }
            _ if ids.is_empty() => {}
            "read" => {
                let rows = eng().db.msgs(&ids);
                let on = rows.iter().any(|m| !m.seen());
                act_keyword(ids, "$seen", on);
            }
            "flag" => {
                let rows = eng().db.msgs(&ids);
                let on = rows.iter().any(|m| !m.flagged());
                act_keyword(ids, "$flagged", on);
            }
            "trash" => act_trash(ids),
            "reply" | "reply-all" | "forward" if ids.len() == 1 => self.reader.act(what),
            _ => {}
        }
    }

    fn context_menu(&self, w: &gtk::Widget, x: f64, y: f64) {
        let sel = self.selected();
        let ids: Vec<i64> = sel.iter().map(|m| m.id).collect();
        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        pop.set_parent(w);
        pop.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        let b = vbox(2);
        let item = |text: &str, f: Box<dyn Fn()>| {
            let bt = button_c(text, &["flat"]);
            bt.set_halign(gtk::Align::Fill);
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
        if ids.len() == 1 {
            let id = ids[0];
            item("reply", Box::new(move || compose::open(compose::Kind::Reply(id))));
            item("reply all", Box::new(move || compose::open(compose::Kind::ReplyAll(id))));
            item("forward", Box::new(move || compose::open(compose::Kind::Forward(id))));
        }
        let unread = sel.iter().any(|m| !m.seen());
        let i2 = ids.clone();
        item(if unread { "mark read" } else { "mark unread" }, Box::new(move || act_keyword(i2.clone(), "$seen", unread)));
        let flag = sel.iter().any(|m| !m.flagged());
        let i2 = ids.clone();
        item(if flag { "flag" } else { "unflag" }, Box::new(move || act_keyword(i2.clone(), "$flagged", flag)));
        let i2 = ids.clone();
        let w2 = w.clone();
        item("move to…", Box::new(move || move_menu(&w2, i2.clone())));
        let i2 = ids.clone();
        item("to shelf…", Box::new(move || shelfui::save_messages(i2.clone())));
        if ids.len() == 1 {
            let m = sel[0].clone();
            item("filter rule from this…", Box::new(move || sieveview::open_with_rule(m.account, Some(&m))));
        }
        let i2 = ids.clone();
        item("trash", Box::new(move || act_trash(i2.clone())));
        pop.set_child(Some(&b));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }
}

pub fn act_keyword(ids: Vec<i64>, kw: &'static str, on: bool) {
    run(eng().set_keyword(ids, kw, on), |r| {
        if let Err(e) = r {
            toast_err(&err_text(&e));
        }
    });
}

pub fn act_trash(ids: Vec<i64>) {
    let n = ids.len();
    run(eng().trash(ids), move |r| match r {
        Ok(()) => toast(&format!("{n} to the trash (or deleted from it)")),
        Err(e) => toast_err(&err_text(&e)),
    });
}

/// A popover with the account's folders; picking one moves the messages there.
pub fn move_menu(anchor: &gtk::Widget, ids: Vec<i64>) {
    let rows = eng().db.msgs(&ids);
    let Some(account) = rows.first().map(|m| m.account) else { return };
    let ids: Vec<i64> = rows.iter().filter(|m| m.account == account).map(|m| m.id).collect();
    let pop = gtk::Popover::new();
    pop.set_has_arrow(false);
    pop.set_parent(anchor);
    let lb = gtk::ListBox::new();
    lb.add_css_class("plain");
    let mbs = eng().db.mailboxes(account);
    let t = tree(&mbs);
    for (m, depth) in &t {
        let l = label(&m.name.to_lowercase());
        l.set_margin_start((*depth * 12) as i32);
        lb.append(&l);
    }
    let p2 = pop.clone();
    lb.connect_row_activated(move |_, row| {
        let i = row.index() as usize;
        if let Some((m, _)) = t.get(i) {
            let name = m.name.clone();
            let (ids2, mid) = (ids.clone(), m.id.clone());
            run(async move { eng().move_to(ids2, account, &mid).await }, move |r| match r {
                Ok(()) => toast(&format!("moved to {}", name.to_lowercase())),
                Err(e) => toast_err(&err_text(&e)),
            });
        }
        p2.popdown();
    });
    let s = gtk::ScrolledWindow::new();
    s.set_child(Some(&lb));
    s.set_propagate_natural_height(true);
    s.set_max_content_height(420);
    s.set_min_content_width(200);
    pop.set_child(Some(&s));
    pop.connect_closed(|p| {
        let p = p.clone();
        glib::idle_add_local_once(move || p.unparent());
    });
    pop.popup();
}
