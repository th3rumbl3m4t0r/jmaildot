//! A test hook, on only when MAILDOT_DEBUG_SOCK names a socket: one command per line
//! (tree, shot, click, set, enter, select, act, text, windows), one answer per command,
//! ending with a line "." — enough to drive the window from a script and check what it shows.

use crate::util::*;
use gtk::prelude::*;
use gtk::{gio, glib, graphene};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

pub fn start(path: &str) {
    let _ = std::fs::remove_file(path);
    let listener = match UnixListener::bind(path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("debug socket {path}: {e}");
            return;
        }
    };
    let (tx, rx) = async_channel::unbounded::<(String, async_channel::Sender<String>)>();
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            let tx = tx.clone();
            std::thread::spawn(move || {
                let mut w = conn.try_clone().unwrap();
                for line in BufReader::new(conn).lines().map_while(Result::ok) {
                    let (rtx, rrx) = async_channel::bounded(1);
                    if tx.send_blocking((line, rtx)).is_err() {
                        return;
                    }
                    let answer = rrx.recv_blocking().unwrap_or_default();
                    let _ = w.write_all(format!("{answer}\n.\n").as_bytes());
                }
            });
        }
    });
    glib::spawn_future_local(async move {
        while let Ok((cmd, reply)) = rx.recv().await {
            let a = handle(&cmd);
            let _ = reply.send(a).await;
        }
    });
}

fn windows() -> Vec<gtk::Window> {
    gtk::Window::list_toplevels().into_iter().filter_map(|w| w.downcast::<gtk::Window>().ok()).filter(|w| w.is_visible()).collect()
}

fn find(name: &str) -> Option<gtk::Widget> {
    fn walk(w: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
        if w.widget_name() == name && w.is_mapped() {
            return Some(w.clone());
        }
        let mut c = w.first_child();
        while let Some(x) = c {
            if let Some(f) = walk(&x, name) {
                return Some(f);
            }
            c = x.next_sibling();
        }
        None
    }
    for win in windows().into_iter().rev() {
        if let Some(f) = walk(win.upcast_ref(), name) {
            return Some(f);
        }
    }
    None
}

fn text_of(w: &gtk::Widget) -> Option<String> {
    if let Some(l) = w.downcast_ref::<gtk::Label>() {
        return Some(l.text().to_string());
    }
    if let Some(b) = w.downcast_ref::<gtk::Button>() {
        return b.label().map(|s| s.to_string());
    }
    if let Some(e) = w.downcast_ref::<gtk::Entry>() {
        return Some(e.text().to_string());
    }
    if let Some(e) = w.downcast_ref::<gtk::CheckButton>() {
        return Some(format!("{}{}", if e.is_active() { "[x] " } else { "[ ] " }, e.label().unwrap_or_default()));
    }
    if let Some(t) = w.downcast_ref::<gtk::TextView>() {
        let b = t.buffer();
        let (s, e) = b.bounds();
        let s = b.text(&s, &e, false).to_string();
        return Some(s.chars().take(400).collect());
    }
    None
}

fn dump(w: &gtk::Widget, depth: usize, out: &mut String, all: bool) {
    if !all && !w.is_mapped() {
        return;
    }
    let ty = w.type_().name().to_string();
    let name = w.widget_name().to_string();
    let mut line = format!("{}{}", "  ".repeat(depth), ty);
    if !name.is_empty() && name != ty {
        line.push_str(&format!(" #{name}"));
    }
    let classes: Vec<String> = w.css_classes().iter().map(|c| c.to_string()).collect();
    if !classes.is_empty() {
        line.push_str(&format!(" .{}", classes.join(".")));
    }
    line.push_str(&format!(" {}x{}", w.width(), w.height()));
    if !w.is_sensitive() {
        line.push_str(" (insensitive)");
    }
    if let Some(t) = text_of(w) {
        let t: String = t.replace('\n', "⏎").chars().take(160).collect();
        line.push_str(&format!(" \"{t}\""));
    }
    if let Some(t) = w.downcast_ref::<gtk::ToggleButton>() {
        if t.is_active() {
            line.push_str(" [on]");
        }
    }
    if let Some(s) = w.downcast_ref::<gtk::Switch>() {
        line.push_str(if s.is_active() { " [on]" } else { " [off]" });
    }
    if let Some(l) = w.downcast_ref::<gtk::ListView>() {
        if let Some(m) = l.model() {
            line.push_str(&format!(" items={}", m.n_items()));
        }
    }
    out.push_str(&line);
    out.push('\n');
    let mut c = w.first_child();
    while let Some(x) = c {
        dump(&x, depth + 1, out, all);
        c = x.next_sibling();
    }
}

fn shot(path: &str, win: &gtk::Window, pad: f32) -> String {
    let (w, h) = (win.width(), win.height());
    let p = gtk::WidgetPaintable::new(Some(win));
    let snap = gtk::Snapshot::new();
    p.snapshot(&snap, w as f64, h as f64);
    let Some(node) = snap.to_node() else { return "nothing drawn".into() };
    let Some(r) = win.native().and_then(|n| n.renderer()) else { return "no renderer".into() };
    // with [pad], the strip around the window (where the outline is drawn) is in the picture too
    let tex = r.render_texture(&node, Some(&graphene::Rect::new(-pad, -pad, w as f32 + 2.0 * pad, h as f32 + 2.0 * pad)));
    match tex.save_to_png(path) {
        Ok(()) => format!("saved {path} {w}x{h}"),
        Err(e) => format!("error {e}"),
    }
}

fn handle(cmd: &str) -> String {
    let mut parts = cmd.splitn(3, ' ');
    let verb = parts.next().unwrap_or("");
    let a1 = parts.next().unwrap_or("");
    let a2 = parts.next().unwrap_or("");
    match verb {
        "windows" => windows().iter().map(|w| format!("{} #{} {}x{}", w.title().unwrap_or_default(), w.widget_name(), w.width(), w.height())).collect::<Vec<_>>().join("\n"),
        "tree" | "treeall" => {
            let mut out = String::new();
            for w in windows() {
                if a1.is_empty() || w.widget_name() == a1 || w.title().as_deref() == Some(a1) {
                    out.push_str(&format!("== window \"{}\"\n", w.title().unwrap_or_default()));
                    dump(w.upcast_ref(), 0, &mut out, verb == "treeall");
                }
            }
            out
        }
        "shot" | "shotpad" => {
            let ws = windows();
            let w = if a2.is_empty() { crate::ui::try_ui().map(|u| u.win.clone().upcast::<gtk::Window>()).or_else(|| ws.first().cloned()) } else { ws.into_iter().find(|w| w.widget_name() == a2 || w.title().as_deref() == Some(a2)) };
            match w {
                Some(w) => shot(a1, &w, if verb == "shotpad" { 4.0 } else { 0.0 }),
                None => "no such window".into(),
            }
        }
        "click" => match find(a1) {
            None => format!("no widget {a1}"),
            Some(w) => {
                if let Some(m) = w.downcast_ref::<gtk::MenuButton>() {
                    m.popup();
                } else if let Some(t) = w.downcast_ref::<gtk::ToggleButton>() {
                    t.set_active(!t.is_active());
                } else if let Some(b) = w.downcast_ref::<gtk::Button>() {
                    b.emit_clicked();
                } else if let Some(c) = w.downcast_ref::<gtk::CheckButton>() {
                    c.set_active(!c.is_active());
                } else if let Some(s) = w.downcast_ref::<gtk::Switch>() {
                    s.set_active(!s.is_active());
                } else if let Some(r) = w.downcast_ref::<gtk::ListBoxRow>() {
                    r.activate();
                    if let Some(lb) = r.parent().and_downcast::<gtk::ListBox>() {
                        lb.select_row(Some(r));
                    }
                } else {
                    return format!("cannot click a {}", w.type_().name());
                }
                "ok".into()
            }
        },
        "set" => match find(a1) {
            None => format!("no widget {a1}"),
            Some(w) => {
                let t = a2.replace("\\n", "\n");
                if let Some(e) = w.downcast_ref::<gtk::Entry>() {
                    e.set_text(&t);
                } else if let Some(v) = w.downcast_ref::<gtk::TextView>() {
                    v.buffer().set_text(&t);
                } else {
                    return format!("cannot set a {}", w.type_().name());
                }
                "ok".into()
            }
        },
        "enter" => match find(a1).and_then(|w| w.downcast::<gtk::Entry>().ok()) {
            None => format!("no entry {a1}"),
            Some(e) => {
                e.emit_activate();
                "ok".into()
            }
        },
        "select" => {
            let i: u32 = a2.trim().parse().unwrap_or(0);
            match find(a1) {
                None => format!("no widget {a1}"),
                Some(w) => {
                    if let Some(d) = w.downcast_ref::<gtk::DropDown>() {
                        d.set_selected(i);
                    } else if let Some(lb) = w.downcast_ref::<gtk::ListBox>() {
                        match lb.row_at_index(i as i32) {
                            Some(r) => {
                                lb.select_row(Some(&r));
                                r.activate();
                            }
                            None => return "no such row".into(),
                        }
                    } else if let Some(lv) = w.downcast_ref::<gtk::ListView>() {
                        match lv.model() {
                            Some(m) => {
                                m.select_item(i, true);
                            }
                            None => return "no model".into(),
                        }
                    } else {
                        return format!("cannot select in a {}", w.type_().name());
                    }
                    "ok".into()
                }
            }
        }
        "text" => match find(a1) {
            None => format!("no widget {a1}"),
            Some(w) => text_of(&w).unwrap_or_default(),
        },
        "act" => {
            let Some(u) = crate::ui::try_ui() else { return "no ui".into() };
            let app: &gio::Application = u.app.upcast_ref();
            let param = a2.trim().parse::<i64>().ok().map(|v| v.to_variant());
            app.activate_action(a1, param.as_ref());
            "ok".into()
        }
        "native" => {
            // where the window's content sits in its surface: the frame (outline / shadow) width
            windows()
                .iter()
                .map(|w| {
                    let (x, y) = w.surface_transform();
                    let (sw, sh) = w.surface().map(|s| (s.width(), s.height())).unwrap_or((0, 0));
                    format!("{} #{} content at {x},{y} in a {sw}x{sh} surface, window {}x{}", w.title().unwrap_or_default(), w.widget_name(), w.width(), w.height())
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
        "settings" => {
            crate::settings::open(Some(a1));
            "ok".into()
        }
        "status" => {
            let e = eng();
            format!("{:?} syncing={} messages={}", e.statuses(), e.syncing(), e.db.total_messages())
        }
        "close" => {
            for w in windows() {
                if w.widget_name() == a1 || w.title().as_deref() == Some(a1) {
                    w.close();
                }
            }
            "ok".into()
        }
        _ => "commands: windows | tree [win] | treeall [win] | shot <png> [win] | click <name> | set <name> <text> | enter <name> | select <name> <i> | text <name> | act <action> [i64] | status | close <win>".into(),
    }
}
