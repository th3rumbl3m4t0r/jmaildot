//! Sieve filters of one account: the scripts on the server (one active), an editor with line
//! numbers and highlighting, validation on the server (errors come back with line and column),
//! and a rule builder that appends `if` blocks with the `require` line kept in step.

use crate::util::*;
use gtk::prelude::*;
use maildot_core::model::MsgRow;
use maildot_core::sieve::{self, Action, Field, Match, Rule};
use sourceview5::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

const LANG: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<language id="sieve" name="Sieve" version="2.0" _section="Other">
  <metadata>
    <property name="globs">*.sieve;*.siv</property>
    <property name="line-comment-start">#</property>
  </metadata>
  <styles>
    <style id="comment" name="Comment" map-to="def:comment"/>
    <style id="string" name="String" map-to="def:string"/>
    <style id="keyword" name="Keyword" map-to="def:keyword"/>
    <style id="tag" name="Tag" map-to="def:type"/>
    <style id="number" name="Number" map-to="def:number"/>
  </styles>
  <definitions>
    <context id="sieve">
      <include>
        <context id="line-comment" style-ref="comment" end-at-line-end="true"><start>#</start></context>
        <context id="block-comment" style-ref="comment"><start>/\*</start><end>\*/</end></context>
        <context id="multiline" style-ref="string"><start>text:</start><end>^\.$</end></context>
        <context id="string" style-ref="string"><start>"</start><end>"</end><include><context id="escape" style-ref="string"><match>\\.</match></context></include></context>
        <context id="tag" style-ref="tag"><match>:[a-zA-Z_][a-zA-Z0-9_-]*</match></context>
        <context id="number" style-ref="number"><match>\b[0-9]+[KMGkmg]?\b</match></context>
        <context id="keywords" style-ref="keyword">
          <keyword>require</keyword><keyword>if</keyword><keyword>elsif</keyword><keyword>else</keyword>
          <keyword>stop</keyword><keyword>keep</keyword><keyword>discard</keyword><keyword>fileinto</keyword>
          <keyword>redirect</keyword><keyword>reject</keyword><keyword>ereject</keyword><keyword>vacation</keyword>
          <keyword>addflag</keyword><keyword>setflag</keyword><keyword>removeflag</keyword><keyword>header</keyword>
          <keyword>address</keyword><keyword>envelope</keyword><keyword>exists</keyword><keyword>size</keyword>
          <keyword>allof</keyword><keyword>anyof</keyword><keyword>not</keyword><keyword>true</keyword>
          <keyword>false</keyword><keyword>body</keyword><keyword>set</keyword><keyword>notify</keyword>
          <keyword>include</keyword><keyword>return</keyword><keyword>date</keyword><keyword>currentdate</keyword>
        </context>
      </include>
    </context>
  </definitions>
</language>
"#;

fn scheme_xml(id: &str, p: &crate::style::Palette, accent: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<style-scheme id="{id}" name="{id}" version="1.0">
  <style name="text" foreground="{fg}" background="{bg}"/>
  <style name="selection" foreground="{bg}" background="{accent}"/>
  <style name="cursor" foreground="{accent}"/>
  <style name="current-line" background="{cur}"/>
  <style name="line-numbers" foreground="{dim}" background="{tb}"/>
  <style name="current-line-number" foreground="{accent}" background="{tb}"/>
  <style name="bracket-match" foreground="{bg}" background="{accent}"/>
  <style name="def:comment" foreground="{dim}" italic="true"/>
  <style name="def:string" foreground="{ok}"/>
  <style name="def:keyword" foreground="{accent}" bold="true"/>
  <style name="def:type" foreground="{warn}"/>
  <style name="def:number" foreground="{warn}"/>
  <style name="def:error" foreground="{bg}" background="{danger}"/>
</style-scheme>
"#,
        fg = p.fg,
        bg = p.panel2,
        cur = p.panel,
        dim = p.fg_dim,
        tb = p.titlebar,
        ok = p.ok,
        warn = p.warn,
        danger = p.danger,
    )
}

/// The language file and colour schemes, written next to the data and added to the search paths.
fn setup_sourceview() {
    let dir = maildot_core::paths::data_dir().join("sourceview");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(dir.join("sieve.lang"), LANG);
    let accent = crate::style::accent();
    let _ = std::fs::write(dir.join("maildot-night.xml"), scheme_xml("maildot-night", &crate::style::NIGHT, &accent));
    let _ = std::fs::write(dir.join("maildot-day.xml"), scheme_xml("maildot-day", &crate::style::DAY, &accent));
    let d = dir.to_string_lossy().to_string();
    let lm = sourceview5::LanguageManager::default();
    let mut lp: Vec<String> = lm.search_path().iter().map(|s| s.to_string()).collect();
    if !lp.contains(&d) {
        lp.insert(0, d.clone());
        let refs: Vec<&str> = lp.iter().map(|s| s.as_str()).collect();
        lm.set_search_path(&refs);
    }
    let sm = sourceview5::StyleSchemeManager::default();
    sm.append_search_path(&d);
    sm.force_rescan();
}

struct Ed {
    account: i64,
    win: gtk::Window,
    list: gtk::ListBox,
    scripts: RefCell<Vec<maildot_core::jmap::SieveScript>>,
    current: RefCell<Option<String>>,
    name: gtk::Entry,
    buf: sourceview5::Buffer,
    view: sourceview5::View,
    status: gtk::Label,
    field: gtk::DropDown,
    header: gtk::Entry,
    matcher: gtk::DropDown,
    value: gtk::Entry,
    action: gtk::DropDown,
    folder: gtk::DropDown,
    folders: RefCell<Vec<String>>,
    address: gtk::Entry,
    also_read: gtk::CheckButton,
    stop: gtk::CheckButton,
}

pub fn open(account: i64) {
    open_with_rule(account, None);
}

/// The editor, with the rule builder filled in from [msg] when given.
pub fn open_with_rule(account: i64, msg: Option<&MsgRow>) {
    let Some(acc) = eng().db.account(account) else { return };
    setup_sourceview();
    let win = gtk::Window::builder().title(format!("filters · {}", acc.name)).default_width(1040).default_height(720).build();
    win.set_widget_name("sieve");
    if let Some(p) = parent() {
        win.set_transient_for(Some(&p));
    }
    let root = gtk::Paned::new(gtk::Orientation::Horizontal);
    // ---- scripts ----
    let left = vbox(0);
    left.add_css_class("pane");
    let (tb, _) = titlebar("scripts");
    left.append(&tb);
    let list = gtk::ListBox::new();
    list.set_widget_name("sieve-scripts");
    left.append(&scrolled(&list));
    let lb = hbox(4);
    lb.add_css_class("bar");
    let new_b = button_c("new", &["small"]);
    let act_b = button_c("activate", &["small"]);
    let deact_b = button_c("deactivate", &["small"]);
    let del_b = button_c("delete", &["small", "danger"]);
    lb.append(&new_b);
    lb.append(&act_b);
    lb.append(&deact_b);
    lb.append(&del_b);
    left.append(&lb);
    left.set_size_request(250, -1);
    // ---- editor ----
    let right = vbox(0);
    let top = hbox(6);
    top.add_css_class("bar");
    top.append(&lbl("name"));
    let name = named(entry("rules"), "sieve-name");
    name.set_hexpand(true);
    top.append(&name);
    let validate_b = named(button_c("validate", &["small"]), "sieve-validate");
    let save_b = named(button_c("save", &["small"]), "sieve-save");
    let save_act_b = named(button_c("save & activate", &["small", "primary"]), "sieve-save-activate");
    top.append(&validate_b);
    top.append(&save_b);
    top.append(&save_act_b);
    right.append(&top);
    let buf = sourceview5::Buffer::new(None::<&gtk::TextTagTable>);
    if let Some(l) = sourceview5::LanguageManager::default().language("sieve") {
        buf.set_language(Some(&l));
    }
    let scheme = if crate::style::night() { "maildot-night" } else { "maildot-day" };
    if let Some(s) = sourceview5::StyleSchemeManager::default().scheme(scheme) {
        buf.set_style_scheme(Some(&s));
    }
    buf.set_highlight_matching_brackets(true);
    let view = sourceview5::View::with_buffer(&buf);
    view.set_widget_name("sieve-text");
    view.set_show_line_numbers(true);
    view.set_highlight_current_line(true);
    view.set_monospace(true);
    view.set_auto_indent(true);
    view.set_insert_spaces_instead_of_tabs(true);
    view.set_tab_width(4);
    view.add_css_class("sourceview");
    right.append(&scrolled(&view));
    let status = named(label_c("", &["small"]), "sieve-status");
    status.set_wrap(true);
    status.add_css_class("bar");
    right.append(&status);

    // ---- rule builder ----
    let rb = vbox(4);
    rb.add_css_class("cond");
    rb.append(&lbl("add a rule"));
    let r1 = hbox(4);
    let fnames: Vec<&str> = Field::ALL.iter().map(|f| f.label()).collect();
    let field = named(dropdown(&fnames, 0), "rule-field");
    let header = named(entry("header name, e.g. List-Id"), "rule-header");
    header.set_visible(false);
    let mnames: Vec<&str> = Match::ALL.iter().map(|m| m.label()).collect();
    let matcher = named(dropdown(&mnames, 0), "rule-match");
    let value = named(entry("value"), "rule-value");
    value.set_hexpand(true);
    r1.append(&lbl("if"));
    r1.append(&field);
    r1.append(&header);
    r1.append(&matcher);
    r1.append(&value);
    rb.append(&r1);
    let r2 = hbox(4);
    let anames: Vec<&str> = Action::ALL.iter().map(|a| a.label()).collect();
    let action = named(dropdown(&anames, 0), "rule-action");
    let folder = named(dropdown(&[""], 0), "rule-folder");
    let address = named(entry("address"), "rule-address");
    address.set_visible(false);
    let also_read = gtk::CheckButton::with_label("and mark read");
    let stop = gtk::CheckButton::with_label("then stop");
    stop.set_active(true);
    let add_b = named(button_c("add rule", &["small", "primary"]), "rule-add");
    r2.append(&lbl("then"));
    r2.append(&action);
    r2.append(&folder);
    r2.append(&address);
    r2.append(&also_read);
    r2.append(&stop);
    r2.append(&spacer());
    r2.append(&add_b);
    rb.append(&r2);
    right.append(&rb);

    root.set_start_child(Some(&left));
    root.set_end_child(Some(&right));
    root.set_position(260);
    win.set_child(Some(&root));

    let ed = Rc::new(Ed {
        account,
        win: win.clone(),
        list: list.clone(),
        scripts: RefCell::new(vec![]),
        current: RefCell::new(None),
        name,
        buf: buf.clone(),
        view,
        status,
        field: field.clone(),
        header: header.clone(),
        matcher,
        value: value.clone(),
        action: action.clone(),
        folder: folder.clone(),
        folders: RefCell::new(vec![]),
        address: address.clone(),
        also_read,
        stop,
    });
    ed.fill_folders();
    if let Some(m) = msg {
        value.set_text(&m.from.email);
        ed.status.set_text(&format!("rule for mail from {}: pick what happens and press \"add rule\", then save", m.from.email));
    }
    {
        let h = header.clone();
        field.connect_selected_notify(move |d| h.set_visible(Field::ALL[d.selected() as usize] == Field::Header));
        let (f, ad) = (folder.clone(), address.clone());
        action.connect_selected_notify(move |d| {
            let a = Action::ALL[d.selected() as usize];
            f.set_visible(matches!(a, Action::Move | Action::Copy));
            ad.set_visible(a == Action::Redirect);
        });
    }
    {
        let e = ed.clone();
        list.connect_row_activated(move |_, row| e.load(row.index() as usize));
        let e = ed.clone();
        new_b.connect_clicked(move |_| {
            *e.current.borrow_mut() = None;
            e.name.set_text("rules");
            e.buf.set_text(sieve::TEMPLATE);
            e.status.set_text("a new script: save to keep it, save & activate to run it");
            e.list.unselect_all();
        });
        let e = ed.clone();
        act_b.connect_clicked(move |_| e.activate(true));
        let e = ed.clone();
        deact_b.connect_clicked(move |_| e.activate(false));
        let e = ed.clone();
        del_b.connect_clicked(move |_| e.delete());
        let e = ed.clone();
        validate_b.connect_clicked(move |_| e.validate());
        let e = ed.clone();
        save_b.connect_clicked(move |_| e.save(false));
        let e = ed.clone();
        save_act_b.connect_clicked(move |_| e.save(true));
        let e = ed.clone();
        add_b.connect_clicked(move |_| e.add_rule());
    }
    ed.reload(None);
    win.present();
}

impl Ed {
    fn acct(&self) -> Option<std::sync::Arc<maildot_core::engine::Acct>> {
        let a = eng().acct(self.account);
        if a.is_none() {
            self.status.set_text("the account is not connected");
        }
        a
    }

    fn fill_folders(&self) {
        let list = eng().db.mailboxes(self.account);
        let names: Vec<String> = crate::mail::tree(&list).into_iter().map(|(m, _)| sieve::folder_path(&list, &m)).collect();
        let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
        self.folder.set_model(Some(&gtk::StringList::new(&refs)));
        *self.folders.borrow_mut() = names;
    }

    /// The scripts from the server; [select] by id after a save.
    fn reload(self: &Rc<Self>, select: Option<String>) {
        let Some(a) = self.acct() else { return };
        let me = self.clone();
        run(async move {
            if !a.jmap.has_sieve().await {
                return Err(anyhow::anyhow!("this server has no JMAP Sieve"));
            }
            a.jmap.sieve_scripts().await
        }, move |r| match r {
            Ok(list) => {
                while let Some(row) = me.list.row_at_index(0) {
                    me.list.remove(&row);
                }
                for s in &list {
                    let l = label(&format!("{}{}", s.name, if s.active { "   · active" } else { "" }));
                    if s.active {
                        l.add_css_class("accent");
                    }
                    me.list.append(&l);
                }
                let want = select.or_else(|| list.iter().find(|s| s.active).map(|s| s.id.clone())).or_else(|| list.first().map(|s| s.id.clone()));
                *me.scripts.borrow_mut() = list;
                let idx = want.and_then(|w| me.scripts.borrow().iter().position(|s| s.id == w));
                match idx {
                    Some(i) => me.load(i),
                    None => {
                        me.buf.set_text(sieve::TEMPLATE);
                        me.name.set_text("rules");
                        *me.current.borrow_mut() = None;
                    }
                }
            }
            Err(e) => me.status.set_text(&err_text(&e)),
        });
    }

    fn load(self: &Rc<Self>, i: usize) {
        let Some(s) = self.scripts.borrow().get(i).cloned() else { return };
        if let Some(row) = self.list.row_at_index(i as i32) {
            self.list.select_row(Some(&row));
        }
        let Some(a) = self.acct() else { return };
        let me = self.clone();
        let blob = s.blob.clone();
        run(async move { a.jmap.sieve_text(&blob).await }, move |r| match r {
            Ok(t) => {
                me.buf.set_text(&t);
                me.name.set_text(&s.name);
                *me.current.borrow_mut() = Some(s.id.clone());
                me.status.set_text(if s.active { "this script runs on arriving mail" } else { "not active" });
            }
            Err(e) => me.status.set_text(&err_text(&e)),
        });
    }

    fn text(&self) -> String {
        let (s, e) = self.buf.bounds();
        self.buf.text(&s, &e, false).to_string()
    }

    fn show_error(&self, msg: &str) {
        self.status.set_text(&format!("✗ {msg}"));
        self.status.add_css_class("bad");
        if let Some((line, col)) = sieve::error_line(msg) {
            if let Some(mut it) = self.buf.iter_at_line_offset((line as i32 - 1).max(0), (col as i32 - 1).max(0)) {
                self.buf.place_cursor(&it);
                self.view.scroll_to_iter(&mut it, 0.2, false, 0.0, 0.0);
                self.view.grab_focus();
            }
        }
    }

    fn ok(&self, msg: &str) {
        self.status.remove_css_class("bad");
        self.status.set_text(msg);
    }

    fn validate(self: &Rc<Self>) {
        let Some(a) = self.acct() else { return };
        let text = self.text();
        let me = self.clone();
        run(async move { a.jmap.sieve_validate(&text).await }, move |r| match r {
            Ok(None) => me.ok("✓ the server accepts it"),
            Ok(Some(e)) => me.show_error(&e),
            Err(e) => me.show_error(&err_text(&e)),
        });
    }

    fn save(self: &Rc<Self>, activate: bool) {
        let Some(a) = self.acct() else { return };
        let text = self.text();
        let name = self.name.text().trim().to_string();
        let name = if name.is_empty() { "rules".to_string() } else { name };
        let id = self.current.borrow().clone();
        let me = self.clone();
        run(async move {
            if let Some(e) = a.jmap.sieve_validate(&text).await? {
                return Ok(Err(e));
            }
            a.jmap.sieve_save(id.as_deref(), &name, &text, activate).await.map(Ok)
        }, move |r: anyhow::Result<Result<String, String>>| match r {
            Ok(Ok(id)) => {
                me.ok(if activate { "✓ saved; it runs on arriving mail" } else { "✓ saved" });
                me.reload(Some(id));
            }
            Ok(Err(e)) => me.show_error(&e),
            Err(e) => me.show_error(&err_text(&e)),
        });
    }

    fn selected(&self) -> Option<maildot_core::jmap::SieveScript> {
        let i = self.list.selected_row()?.index() as usize;
        self.scripts.borrow().get(i).cloned()
    }

    fn activate(self: &Rc<Self>, on: bool) {
        let Some(a) = self.acct() else { return };
        let id = if on {
            match self.selected() {
                Some(s) => Some(s.id),
                None => return,
            }
        } else {
            None
        };
        let me = self.clone();
        let id2 = id.clone();
        run(async move { a.jmap.sieve_activate(id2.as_deref()).await }, move |r| match r {
            Ok(()) => {
                me.ok(if on { "✓ active" } else { "✓ no script runs now" });
                me.reload(id);
            }
            Err(e) => me.show_error(&err_text(&e)),
        });
    }

    fn delete(self: &Rc<Self>) {
        let Some(s) = self.selected() else { return };
        let me = self.clone();
        ask("delete script", &format!("Delete the script \"{}\" from the server?", s.name), &["cancel", "delete"], move |i| {
            if i != Some(1) {
                return;
            }
            let Some(a) = me.acct() else { return };
            let me2 = me.clone();
            let id = s.id.clone();
            run(async move { a.jmap.sieve_delete(&id).await }, move |r| match r {
                Ok(()) => {
                    me2.ok("✓ deleted");
                    me2.reload(None);
                }
                Err(e) => me2.show_error(&err_text(&e)),
            });
        });
    }

    fn add_rule(self: &Rc<Self>) {
        let action = Action::ALL[self.action.selected() as usize];
        let target = match action {
            Action::Move | Action::Copy => self.folders.borrow().get(self.folder.selected() as usize).cloned().unwrap_or_default(),
            Action::Redirect => self.address.text().trim().to_string(),
            _ => String::new(),
        };
        if action.needs_target() && target.is_empty() {
            self.show_error("say where to");
            return;
        }
        let value = self.value.text().to_string();
        if value.is_empty() {
            self.show_error("say what to look for");
            return;
        }
        let field = Field::ALL[self.field.selected() as usize];
        if field == Field::Header && self.header.text().trim().is_empty() {
            self.show_error("which header?");
            return;
        }
        let r = Rule {
            field,
            header: self.header.text().to_string(),
            matcher: Match::ALL[self.matcher.selected() as usize],
            value,
            action,
            target,
            also_read: self.also_read.is_active(),
            stop: self.stop.is_active(),
        };
        let text = sieve::add_rule(&self.text(), &r);
        self.buf.set_text(&text);
        let mut end = self.buf.end_iter();
        self.view.scroll_to_iter(&mut end, 0.0, false, 0.0, 0.0);
        self.ok("rule added at the end: validate, then save (& activate)");
        let _ = &self.win;
    }
}
