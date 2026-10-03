//! shelf from here: save attachments or whole messages into a shelf folder, attach files
//! from shelf to a message. One browser for both: the place (an API key's folder), its
//! folders, and for picking, its files.

use crate::util::*;
use gtk::prelude::*;
use maildot_core::shelf::{self, Entry, Shelf};
use std::cell::RefCell;
use std::rc::Rc;

enum Mode {
    /// Files to put into the chosen folder (name, data), with this modification time.
    Save(Vec<(String, Vec<u8>)>, Option<i64>),
    /// Files to take: the callback gets them.
    Pick(Rc<dyn Fn(Vec<(String, Vec<u8>)>)>),
}

struct Browser {
    win: gtk::Window,
    places: Vec<(String, String, String)>,
    place: gtk::DropDown,
    path: RefCell<String>,
    path_label: gtk::Label,
    list: gtk::ListBox,
    entries: RefCell<Vec<Entry>>,
    status: gtk::Label,
    mode: Mode,
}

pub fn save_files(files: Vec<(String, Vec<u8>)>, mtime: i64) {
    browser(Mode::Save(files, Some(mtime)));
}

/// Messages as .eml files (`2026-10-03 subject.eml`).
pub fn save_messages(ids: Vec<i64>) {
    let mut files = Vec::new();
    for id in ids {
        let Some(m) = eng().db.msg(id) else { continue };
        let path = maildot_core::paths::eml_path(m.account, id);
        match std::fs::read(&path) {
            Ok(d) => {
                let subj: String = m.subject.chars().take(80).collect();
                let name = format!("{} {}.eml", local(m.received).format("%Y-%m-%d"), if subj.trim().is_empty() { "message".to_string() } else { subj });
                files.push((shelf::safe_name(&name), d));
            }
            Err(_) => toast_err(&format!("\"{}\" is not downloaded yet", m.subject)),
        }
    }
    if !files.is_empty() {
        browser(Mode::Save(files, None));
    }
}

pub fn pick_files(done: impl Fn(Vec<(String, Vec<u8>)>) + 'static) {
    browser(Mode::Pick(Rc::new(done)));
}

fn browser(mode: Mode) {
    let places = eng().shelves();
    if places.is_empty() {
        ask("shelf", "No shelf folder is set up yet. Add one with an API key from shelf (share → API keys) in settings → shelf.", &["later", "open settings"], |i| {
            if i == Some(1) {
                crate::settings::open(Some("shelf"));
            }
        });
        return;
    }
    let saving = matches!(mode, Mode::Save(..));
    let win = gtk::Window::builder().title(if saving { "save to shelf" } else { "attach from shelf" }).default_width(560).default_height(520).modal(true).build();
    win.set_widget_name("shelf");
    if let Some(p) = parent() {
        win.set_transient_for(Some(&p));
    }
    let root = vbox(4);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(10);
    root.set_margin_end(10);
    if let Mode::Save(files, _) = &mode {
        let l = label_c(&format!("{}: {}", if files.len() == 1 { "file" } else { "files" }, files.iter().map(|f| f.0.clone()).collect::<Vec<_>>().join(", ")), &["small"]);
        l.set_wrap(true);
        root.append(&l);
    }
    let labels: Vec<String> = places.iter().map(|p| p.1.clone()).collect();
    let refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let last = eng().db.setting("shelf.last").and_then(|l| places.iter().position(|p| p.0 == l)).unwrap_or(0);
    let place = dropdown(&refs, last as u32);
    root.append(&place);
    let nav = hbox(4);
    let up = button_c("up", &["small"]);
    let path_label = label_c("/", &["accent"]);
    path_label.set_hexpand(true);
    nav.append(&up);
    nav.append(&path_label);
    root.append(&nav);
    let list = gtk::ListBox::new();
    list.set_selection_mode(if saving { gtk::SelectionMode::None } else { gtk::SelectionMode::Multiple });
    list.set_widget_name("shelf-list");
    root.append(&scrolled(&list));
    let status = label_c("", &["dim", "small"]);
    status.set_wrap(true);
    root.append(&status);
    let bar = hbox(4);
    let newdir = entry("new folder");
    let mk = button_c("make", &["small"]);
    if saving {
        bar.append(&newdir);
        bar.append(&mk);
    }
    bar.append(&spacer());
    let cancel = button("cancel");
    let go = named(button_c(if saving { "save here" } else { "attach selected" }, &["primary"]), "shelf-go");
    bar.append(&cancel);
    bar.append(&go);
    root.append(&bar);
    win.set_child(Some(&root));

    let b = Rc::new(Browser {
        win: win.clone(),
        places,
        place: place.clone(),
        path: RefCell::new("/".into()),
        path_label,
        list: list.clone(),
        entries: RefCell::new(vec![]),
        status,
        mode,
    });
    {
        let b2 = b.clone();
        place.connect_selected_notify(move |_| {
            *b2.path.borrow_mut() = "/".into();
            b2.load();
        });
        let b2 = b.clone();
        up.connect_clicked(move |_| {
            let p = b2.path.borrow().clone();
            let parent = match p.trim_end_matches('/').rsplit_once('/') {
                Some((a, _)) if !a.is_empty() => a.to_string(),
                _ => "/".to_string(),
            };
            *b2.path.borrow_mut() = parent;
            b2.load();
        });
        let b2 = b.clone();
        list.connect_row_activated(move |_, row| {
            let e = b2.entries.borrow().get(row.index() as usize).cloned();
            if let Some(e) = e {
                if e.dir {
                    *b2.path.borrow_mut() = e.path.clone();
                    b2.load();
                }
            }
        });
        let w2 = win.clone();
        cancel.connect_clicked(move |_| w2.close());
        let b2 = b.clone();
        mk.connect_clicked(move |_| {
            let n = shelf::safe_name(newdir.text().trim());
            if newdir.text().trim().is_empty() {
                return;
            }
            let Some(s) = b2.shelf() else { return };
            let p = format!("{}/{}", b2.path.borrow().trim_end_matches('/'), n);
            let b3 = b2.clone();
            let p2 = p.clone();
            run(async move { s.mkdir(&p2).await }, move |r| match r {
                Ok(()) => {
                    *b3.path.borrow_mut() = p;
                    b3.load();
                }
                Err(e) => b3.status.set_text(&err_text(&e)),
            });
        });
        let b2 = b.clone();
        go.connect_clicked(move |_| b2.go());
    }
    b.load();
    win.present();
}

impl Browser {
    fn shelf(&self) -> Option<Shelf> {
        let p = self.places.get(self.place.selected() as usize)?;
        let _ = eng().db.set_setting("shelf.last", &p.0);
        match eng().shelf(&p.0) {
            Ok(s) => Some(s),
            Err(e) => {
                self.status.set_text(&err_text(&e));
                None
            }
        }
    }

    fn load(self: &Rc<Self>) {
        let Some(s) = self.shelf() else { return };
        let path = self.path.borrow().clone();
        self.path_label.set_text(&path);
        self.status.set_text("loading…");
        let me = self.clone();
        run(async move { s.list(&path).await }, move |r| {
            while let Some(row) = me.list.row_at_index(0) {
                me.list.remove(&row);
            }
            match r {
                Ok(list) => {
                    let saving = matches!(me.mode, Mode::Save(..));
                    let shown: Vec<Entry> = list.into_iter().filter(|e| e.dir || !saving).collect();
                    for e in &shown {
                        let row = hbox(8);
                        let l = label(&if e.dir { format!("{}/", e.name) } else { e.name.clone() });
                        l.set_hexpand(true);
                        if e.dir {
                            l.add_css_class("accent");
                        }
                        row.append(&l);
                        if !e.dir {
                            row.append(&label_c(&size(e.size as i64), &["dim", "small"]));
                        }
                        me.list.append(&row);
                    }
                    me.status.set_text(if saving { "double-click a folder to go in; \"save here\" saves into the folder shown" } else { "double-click a folder to go in; select files (ctrl / shift for several)" });
                    *me.entries.borrow_mut() = shown;
                }
                Err(e) => me.status.set_text(&err_text(&e)),
            }
        });
    }

    fn go(self: &Rc<Self>) {
        let Some(s) = self.shelf() else { return };
        let folder = self.path.borrow().clone();
        match &self.mode {
            Mode::Save(files, mtime) => {
                let files = files.clone();
                let mtime = *mtime;
                let me = self.clone();
                self.status.set_text("uploading…");
                run(async move {
                    let mut names = Vec::new();
                    for (n, d) in files {
                        let e = s.upload_free(&folder, &shelf::safe_name(&n), d, mtime).await?;
                        names.push(e.path);
                    }
                    Ok::<_, anyhow::Error>(names)
                }, move |r| match r {
                    Ok(names) => {
                        toast(&format!("saved to shelf: {}", names.join(", ")));
                        me.win.close();
                    }
                    Err(e) => me.status.set_text(&err_text(&e)),
                });
            }
            Mode::Pick(done) => {
                let picked: Vec<Entry> = self.list.selected_rows().iter().filter_map(|r| self.entries.borrow().get(r.index() as usize).cloned()).filter(|e| !e.dir).collect();
                if picked.is_empty() {
                    self.status.set_text("select one or more files");
                    return;
                }
                let done = done.clone();
                let me = self.clone();
                self.status.set_text("downloading…");
                run(async move {
                    let mut out = Vec::new();
                    for e in picked {
                        out.push((e.name.clone(), s.download(&e.path).await?));
                    }
                    Ok::<_, anyhow::Error>(out)
                }, move |r| match r {
                    Ok(files) => {
                        me.win.close();
                        done(files);
                    }
                    Err(e) => me.status.set_text(&err_text(&e)),
                });
            }
        }
    }
}
