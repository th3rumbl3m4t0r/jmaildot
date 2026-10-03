//! Small things every view uses: the engine, running work off the main thread, building
//! widgets in the x11 look, dates, dialogs, toasts, opening and saving files.

use chrono::{DateTime, Datelike, Local, TimeZone};
use gtk::prelude::*;
use gtk::{gio, glib};
use maildot_core::engine::Engine;
use std::future::Future;
use std::sync::{Arc, OnceLock};

static ENGINE: OnceLock<Arc<Engine>> = OnceLock::new();

pub fn set_engine(e: Arc<Engine>) {
    let _ = ENGINE.set(e);
}

pub fn eng() -> &'static Arc<Engine> {
    ENGINE.get().expect("engine")
}

/// Runs [fut] on the network runtime; [done] gets the result on the main thread.
pub fn run<T: Send + 'static>(fut: impl Future<Output = T> + Send + 'static, done: impl FnOnce(T) + 'static) {
    let (tx, rx) = async_channel::bounded(1);
    eng().rt.spawn(async move {
        let _ = tx.send(fut.await).await;
    });
    glib::spawn_future_local(async move {
        if let Ok(v) = rx.recv().await {
            done(v)
        }
    });
}

/// Runs blocking work (database, search, parsing) on a worker thread.
pub fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static, done: impl FnOnce(T) + 'static) {
    let (tx, rx) = async_channel::bounded(1);
    eng().rt.spawn_blocking(move || {
        let _ = tx.send_blocking(f());
    });
    glib::spawn_future_local(async move {
        if let Ok(v) = rx.recv().await {
            done(v)
        }
    });
}

pub fn err_text(e: &anyhow::Error) -> String {
    maildot_core::engine::describe(e)
}

// ---- widgets ----

pub fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.set_xalign(0.0);
    l
}

pub fn label_c(text: &str, classes: &[&str]) -> gtk::Label {
    let l = label(text);
    for c in classes {
        l.add_css_class(c);
    }
    l
}

/// An UPPERCASE dim label, as the x11 `.lbl`.
pub fn lbl(text: &str) -> gtk::Label {
    label_c(&text.to_uppercase(), &["lbl"])
}

pub fn button(text: &str) -> gtk::Button {
    let b = gtk::Button::with_label(text);
    b.set_focus_on_click(false);
    b
}

pub fn button_c(text: &str, classes: &[&str]) -> gtk::Button {
    let b = button(text);
    for c in classes {
        b.add_css_class(c);
    }
    b
}

pub fn named<W: IsA<gtk::Widget>>(w: W, name: &str) -> W {
    w.set_widget_name(name);
    w
}

pub fn hbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Horizontal, spacing)
}

pub fn vbox(spacing: i32) -> gtk::Box {
    gtk::Box::new(gtk::Orientation::Vertical, spacing)
}

pub fn spacer() -> gtk::Box {
    let b = hbox(0);
    b.set_hexpand(true);
    b
}

/// A titlebar strip: `:: CAPTION`, with room on the right.
pub fn titlebar(caption: &str) -> (gtk::Box, gtk::Label) {
    let b = hbox(6);
    b.add_css_class("tbar");
    let grip = label("::");
    grip.add_css_class("dim");
    let l = label(&caption.to_uppercase());
    b.append(&grip);
    b.append(&l);
    b.append(&spacer());
    (b, l)
}

pub fn scrolled<W: IsA<gtk::Widget>>(child: &W) -> gtk::ScrolledWindow {
    let s = gtk::ScrolledWindow::new();
    s.set_child(Some(child));
    s.set_vexpand(true);
    s.set_hexpand(true);
    s.set_hscrollbar_policy(gtk::PolicyType::Never);
    s
}

pub fn entry(placeholder: &str) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_placeholder_text(Some(placeholder));
    e
}

pub fn dropdown(items: &[&str], selected: u32) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(items);
    d.set_selected(selected);
    d
}

pub fn markup_escape(s: &str) -> String {
    glib::markup_escape_text(s).to_string()
}

/// [text] as markup with the byte ranges highlighted in the accent.
pub fn highlight(text: &str, ranges: &[(usize, usize)]) -> String {
    let accent = crate::style::accent();
    let mut out = String::new();
    let mut pos = 0;
    for &(a, b) in ranges {
        if a < pos || b > text.len() || !text.is_char_boundary(a) || !text.is_char_boundary(b) {
            continue;
        }
        out.push_str(&markup_escape(&text[pos..a]));
        out.push_str(&format!("<span background=\"{accent}\" foreground=\"{}\">", crate::style::palette().bg));
        out.push_str(&markup_escape(&text[a..b]));
        out.push_str("</span>");
        pos = b;
    }
    out.push_str(&markup_escape(&text[pos..]));
    out
}

pub fn clear_box(b: &gtk::Box) {
    while let Some(c) = b.first_child() {
        b.remove(&c);
    }
}

pub fn children(w: &impl IsA<gtk::Widget>) -> Vec<gtk::Widget> {
    let mut out = Vec::new();
    let mut c = w.first_child();
    while let Some(x) = c {
        c = x.next_sibling();
        out.push(x);
    }
    out
}

// ---- time ----

pub fn local(secs: i64) -> DateTime<Local> {
    Local.timestamp_opt(secs, 0).single().unwrap_or_else(Local::now)
}

/// List dates: `14:32` today, `03 Oct` this year, else `2025-10-03`.
pub fn short_date(secs: i64) -> String {
    let d = local(secs);
    let now = Local::now();
    if d.date_naive() == now.date_naive() {
        d.format("%H:%M").to_string()
    } else if d.year() == now.year() {
        d.format("%d %b").to_string()
    } else {
        d.format("%Y-%m-%d").to_string()
    }
}

pub fn long_date(secs: i64) -> String {
    local(secs).format("%a %Y-%m-%d %H:%M").to_string()
}

pub fn size(n: i64) -> String {
    let n = n as f64;
    if n < 1024.0 {
        format!("{n} B")
    } else if n < 1024.0 * 1024.0 {
        format!("{:.0} KB", n / 1024.0)
    } else if n < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", n / 1024.0 / 1024.0)
    } else {
        format!("{:.2} GB", n / 1024.0 / 1024.0 / 1024.0)
    }
}

// ---- dialogs ----

pub fn parent() -> Option<gtk::Window> {
    gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()).find(|w| w.is_active()).or_else(|| crate::ui::try_ui().map(|u| u.win.clone().upcast()))
}

/// A small modal window with a message and buttons; [done] gets the index pressed (None: closed).
pub fn ask(title: &str, text: &str, buttons: &[&str], done: impl Fn(Option<usize>) + 'static) {
    let w = gtk::Window::builder().title(title).modal(true).resizable(false).default_width(380).build();
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let b = vbox(8);
    b.set_margin_top(10);
    b.set_margin_bottom(10);
    b.set_margin_start(12);
    b.set_margin_end(12);
    let l = label(text);
    l.set_wrap(true);
    l.set_max_width_chars(60);
    b.append(&l);
    let row = hbox(6);
    row.append(&spacer());
    let done = std::rc::Rc::new(done);
    let answered = std::rc::Rc::new(std::cell::Cell::new(false));
    for (i, t) in buttons.iter().enumerate() {
        let bt = button(t);
        if i + 1 == buttons.len() {
            bt.add_css_class("primary");
        }
        if t.contains("delete") || t.contains("remove") || t.contains("discard") {
            bt.remove_css_class("primary");
            bt.add_css_class("danger");
        }
        let w2 = w.clone();
        let d = done.clone();
        let a = answered.clone();
        bt.connect_clicked(move |_| {
            a.set(true);
            w2.close();
            d(Some(i));
        });
        row.append(&bt);
    }
    b.append(&row);
    w.set_child(Some(&b));
    let d = done.clone();
    w.connect_close_request(move |_| {
        if !answered.get() {
            d(None);
        }
        glib::Propagation::Proceed
    });
    w.present();
}

/// Asks for a line of text (a passphrase when [secret]).
pub fn prompt(title: &str, text: &str, initial: &str, secret: bool, done: impl Fn(Option<String>) + 'static) {
    let w = gtk::Window::builder().title(title).modal(true).resizable(false).default_width(420).build();
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let b = vbox(8);
    b.set_margin_top(10);
    b.set_margin_bottom(10);
    b.set_margin_start(12);
    b.set_margin_end(12);
    let l = label(text);
    l.set_wrap(true);
    b.append(&l);
    let e = gtk::Entry::new();
    e.set_widget_name("prompt-entry");
    e.set_text(initial);
    e.set_visibility(!secret);
    e.set_activates_default(true);
    b.append(&e);
    let row = hbox(6);
    row.append(&spacer());
    let cancel = button("cancel");
    let ok = named(button_c("ok", &["primary"]), "prompt-ok");
    row.append(&cancel);
    row.append(&ok);
    b.append(&row);
    w.set_child(Some(&b));
    let done = std::rc::Rc::new(done);
    {
        let w = w.clone();
        let d = done.clone();
        cancel.connect_clicked(move |_| {
            w.close();
            d(None);
        });
    }
    {
        let w = w.clone();
        let d = done.clone();
        let e2 = e.clone();
        ok.connect_clicked(move |_| {
            let t = e2.text().to_string();
            w.close();
            d(Some(t));
        });
    }
    {
        let okb = ok.clone();
        e.connect_activate(move |_| okb.emit_clicked());
    }
    w.present();
    e.grab_focus();
}

/// A short message at the bottom of the main window that goes away by itself.
pub fn toast(text: &str) {
    if let Some(u) = crate::ui::try_ui() {
        u.toast(text, false);
    }
}

pub fn toast_err(text: &str) {
    if let Some(u) = crate::ui::try_ui() {
        u.toast(text, true);
    }
}

// ---- files ----

/// Writes [data] to a private temporary file and opens it with the desktop's program.
pub fn open_data(name: &str, data: &[u8]) {
    let dir = maildot_core::paths::tmp_dir().join(format!("{:x}", rand_u64()));
    let safe = maildot_core::shelf::safe_name(name);
    let path = dir.join(&safe);
    let r = (|| -> std::io::Result<()> {
        maildot_core::paths::ensure(&dir)?;
        std::fs::write(&path, data)?;
        Ok(())
    })();
    match r {
        Err(e) => toast_err(&format!("cannot write {}: {e}", path.display())),
        Ok(()) => open_path(&path),
    }
}

pub fn open_path(path: &std::path::Path) {
    let uri = gio::File::for_path(path).uri();
    if let Err(e) = gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>) {
        toast_err(&format!("no program to open it: {e}"));
    }
}

pub fn open_uri(uri: &str) {
    if let Err(e) = gio::AppInfo::launch_default_for_uri(uri, None::<&gio::AppLaunchContext>) {
        toast_err(&format!("cannot open {uri}: {e}"));
    }
}

pub fn rand_u64() -> u64 {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    h.finish()
}

/// Asks where to save and writes [data] there.
pub fn save_data(name: &str, data: Vec<u8>) {
    let d = gtk::FileDialog::builder().title("save").initial_name(maildot_core::shelf::safe_name(name)).modal(true).build();
    let p = parent();
    d.save(p.as_ref(), None::<&gio::Cancellable>, move |r| {
        if let Ok(f) = r {
            if let Some(path) = f.path() {
                match std::fs::write(&path, &data) {
                    Ok(()) => toast(&format!("saved {}", path.display())),
                    Err(e) => toast_err(&format!("cannot save: {e}")),
                }
            }
        }
    });
}

/// Asks for files to open; [done] gets their paths.
pub fn pick_files(title: &str, done: impl Fn(Vec<std::path::PathBuf>) + 'static) {
    let d = gtk::FileDialog::builder().title(title).modal(true).build();
    let p = parent();
    d.open_multiple(p.as_ref(), None::<&gio::Cancellable>, move |r| {
        if let Ok(list) = r {
            let mut out = Vec::new();
            for i in 0..list.n_items() {
                if let Some(f) = list.item(i).and_downcast::<gio::File>() {
                    if let Some(p) = f.path() {
                        out.push(p);
                    }
                }
            }
            done(out);
        }
    });
}

pub fn guess_mime(name: &str) -> String {
    let (ty, _) = gio::content_type_guess(Some(name), None::<&[u8]>);
    gio::content_type_get_mime_type(&ty).map(|m| m.to_string()).unwrap_or_else(|| "application/octet-stream".into())
}

pub fn copy_text(text: &str) {
    if let Some(d) = gtk::gdk::Display::default() {
        d.clipboard().set_text(text);
    }
}
