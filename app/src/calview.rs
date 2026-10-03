//! The calendar page. Our own calendars (every account) are drawn together in one pane; each
//! calendar someone shared with us is a pane of its own, switched on and off next to its name,
//! so shared calendars never overlap ours or each other. Month, week and agenda views.

use crate::util::*;
use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime};
use gtk::prelude::*;
use gtk::{gdk, glib};
use maildot_core::cal::Shown;
use maildot_core::caldav::{Ace, Cal};
use maildot_core::ical::{self, Draft, Repeat};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum View {
    Month,
    Week,
    Agenda,
}

pub struct CalView {
    pub root: gtk::Paned,
    side: gtk::Box,
    mini: gtk::Calendar,
    title: gtk::Label,
    panes: gtk::Box,
    view: Cell<View>,
    anchor: Cell<NaiveDate>,
    selected: Cell<NaiveDate>,
    refresh_pending: Cell<bool>,
    view_btns: Vec<gtk::ToggleButton>,
}

const HOUR: i32 = 42;

fn today() -> NaiveDate {
    Local::now().date_naive()
}

fn monday_first() -> bool {
    eng().db.setting_or("cal.week", "monday") == "monday"
}

fn week_start(d: NaiveDate) -> NaiveDate {
    d - Duration::days(ical::weekday_index(d, monday_first()) as i64)
}

fn color_of(c: &Cal) -> String {
    c.color.clone().filter(|x| crate::style::valid_hex(x)).unwrap_or_else(crate::style::accent)
}

fn cal_label(c: &Cal) -> String {
    match &c.owner {
        Some(o) => format!("{o} · {}", c.name),
        None => c.name.clone(),
    }
}

/// A pane: our calendars, or one shared calendar.
#[derive(Clone, PartialEq, Debug)]
enum PaneKey {
    Mine,
    Shared(i64, String),
}

impl PaneKey {
    fn holds(&self, c: &Cal) -> bool {
        match self {
            PaneKey::Mine => c.owner.is_none(),
            PaneKey::Shared(a, h) => c.account == *a && &c.href == h,
        }
    }
}

fn time_text(s: &Shown) -> String {
    let tz = ical::local_tz();
    if s.occ.all_day {
        String::new()
    } else {
        format!("{} ", ical::hm(s.occ.start, tz))
    }
}

/// The days an instance covers (an all-day event's end is exclusive, a timed one's too).
fn days_of(s: &Shown) -> (NaiveDate, NaiveDate) {
    let tz = ical::local_tz();
    let a = ical::local_date(s.occ.start, tz);
    let mut b = ical::local_date((s.occ.end - 1).max(s.occ.start), tz);
    if b < a {
        b = a;
    }
    (a, b)
}

impl CalView {
    pub fn new() -> Rc<CalView> {
        let side = vbox(6);
        side.set_margin_start(6);
        side.set_margin_end(6);
        side.set_margin_top(6);
        let mini = gtk::Calendar::new();
        let sidebox = vbox(0);
        sidebox.add_css_class("pane");
        let (stb, _) = titlebar("calendars");
        sidebox.append(&stb);
        let sv = vbox(6);
        sv.append(&mini);
        sv.append(&side);
        sidebox.append(&scrolled(&sv));
        sidebox.set_size_request(250, -1);

        let main = vbox(0);
        let bar = hbox(4);
        bar.add_css_class("bar");
        let today_b = named(button_c("today", &["small"]), "cal-today");
        let prev = named(button_c("<", &["small"]), "cal-prev");
        let next = named(button_c(">", &["small"]), "cal-next");
        let title = label_c("", &["big"]);
        title.set_margin_start(8);
        bar.append(&today_b);
        bar.append(&prev);
        bar.append(&next);
        bar.append(&title);
        bar.append(&spacer());
        let mut view_btns = Vec::new();
        for (n, t) in [("month", "month"), ("week", "week"), ("agenda", "agenda")] {
            let b = gtk::ToggleButton::with_label(t);
            b.add_css_class("tag");
            b.set_widget_name(&format!("cal-{n}"));
            if let Some(f) = view_btns.first() {
                b.set_group(Some(f));
            }
            bar.append(&b);
            view_btns.push(b);
        }
        let new_b = named(button_c("new event", &["primary", "small"]), "cal-new");
        bar.append(&new_b);
        main.append(&bar);
        let panes = hbox(0);
        panes.set_homogeneous(true);
        panes.set_vexpand(true);
        main.append(&panes);

        let root = gtk::Paned::new(gtk::Orientation::Horizontal);
        root.set_start_child(Some(&sidebox));
        root.set_end_child(Some(&main));
        root.set_position(270);
        root.set_shrink_start_child(false);
        root.set_resize_start_child(false);

        let view = match eng().db.setting_or("cal.view", "month").as_str() {
            "week" => View::Week,
            "agenda" => View::Agenda,
            _ => View::Month,
        };
        let v = Rc::new(CalView {
            root,
            side,
            mini: mini.clone(),
            title,
            panes,
            view: Cell::new(view),
            anchor: Cell::new(today()),
            selected: Cell::new(today()),
            refresh_pending: Cell::new(false),
            view_btns: view_btns.clone(),
        });
        view_btns[match view {
            View::Month => 0,
            View::Week => 1,
            View::Agenda => 2,
        }]
        .set_active(true);
        for (i, b) in view_btns.iter().enumerate() {
            let w = Rc::downgrade(&v);
            b.connect_toggled(move |b| {
                if !b.is_active() {
                    return;
                }
                if let Some(v) = w.upgrade() {
                    let nv = [View::Month, View::Week, View::Agenda][i];
                    v.view.set(nv);
                    let _ = eng().db.set_setting("cal.view", ["month", "week", "agenda"][i]);
                    v.refresh();
                }
            });
        }
        {
            let w = Rc::downgrade(&v);
            today_b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.anchor.set(today());
                    v.selected.set(today());
                    v.refresh();
                }
            });
            let w = Rc::downgrade(&v);
            prev.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.step(-1)
                }
            });
            let w = Rc::downgrade(&v);
            next.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.step(1)
                }
            });
            let w = Rc::downgrade(&v);
            new_b.connect_clicked(move |_| {
                if let Some(v) = w.upgrade() {
                    v.new_event(Some(v.selected.get()))
                }
            });
            let w = Rc::downgrade(&v);
            mini.connect_day_selected(move |c| {
                if let Some(v) = w.upgrade() {
                    let d = c.date();
                    if let Some(nd) = NaiveDate::from_ymd_opt(d.year(), d.month() as u32, d.day_of_month() as u32) {
                        if nd != v.selected.get() {
                            v.selected.set(nd);
                            v.anchor.set(nd);
                            v.refresh();
                        }
                    }
                }
            });
        }
        v
    }

    fn step(self: &Rc<Self>, dir: i64) {
        let a = self.anchor.get();
        let n = match self.view.get() {
            View::Month => {
                let (y, m) = (a.year(), a.month() as i32 - 1 + dir as i32);
                let y2 = y + m.div_euclid(12);
                let m2 = m.rem_euclid(12) as u32 + 1;
                NaiveDate::from_ymd_opt(y2, m2, 1).unwrap_or(a)
            }
            View::Week => a + Duration::days(7 * dir),
            View::Agenda => a + Duration::days(30 * dir),
        };
        self.anchor.set(n);
        self.refresh();
    }

    pub fn schedule_refresh(self: &Rc<Self>) {
        if self.refresh_pending.get() {
            return;
        }
        self.refresh_pending.set(true);
        let w = Rc::downgrade(self);
        glib::timeout_add_local_once(std::time::Duration::from_millis(400), move || {
            if let Some(v) = w.upgrade() {
                v.refresh_pending.set(false);
                v.refresh();
            }
        });
    }

    /// The range on screen: [first day, day after the last).
    fn range(&self) -> (NaiveDate, NaiveDate) {
        let a = self.anchor.get();
        match self.view.get() {
            View::Month => {
                let first = NaiveDate::from_ymd_opt(a.year(), a.month(), 1).unwrap();
                let s = week_start(first);
                (s, s + Duration::days(42))
            }
            View::Week => {
                let s = week_start(a);
                (s, s + Duration::days(7))
            }
            View::Agenda => (a, a + Duration::days(60)),
        }
    }

    pub fn refresh(self: &Rc<Self>) {
        self.build_side();
        let (a, b) = self.range();
        let a0 = self.anchor.get();
        self.title.set_text(&match self.view.get() {
            View::Month => a0.format("%B %Y").to_string().to_lowercase(),
            View::Week => format!("{} – {}", a.format("%d %b"), (b - Duration::days(1)).format("%d %b %Y")).to_lowercase(),
            View::Agenda => format!("from {}", a.format("%a %d %b %Y")).to_lowercase(),
        });
        let sel = self.selected.get();
        self.mini.select_day(&glib::DateTime::from_local(sel.year(), sel.month() as i32, sel.day() as i32, 0, 0, 0.0).unwrap());
        let tz = ical::local_tz();
        let lo = ical::midnight(a, tz);
        let hi = ical::midnight(b, tz);
        let w = Rc::downgrade(self);
        let view = self.view.get();
        blocking(move || eng().occurrences(lo, hi), move |shown| {
            let Some(v) = w.upgrade() else { return };
            if v.view.get() != view {
                return;
            }
            v.draw(shown, a, b);
        });
    }

    fn draw(self: &Rc<Self>, shown: Vec<Shown>, a: NaiveDate, b: NaiveDate) {
        clear_box(&self.panes);
        let cals = eng().calendars();
        let mut keys = vec![PaneKey::Mine];
        for c in cals.iter().filter(|c| c.owner.is_some() && c.enabled) {
            keys.push(PaneKey::Shared(c.account, c.href.clone()));
        }
        let many = keys.len() > 1;
        for k in keys {
            let pane = vbox(0);
            pane.add_css_class("win");
            let caption = match &k {
                PaneKey::Mine => if many { "mine".to_string() } else { String::new() },
                PaneKey::Shared(acc, h) => cals.iter().find(|c| c.account == *acc && &c.href == h).map(|c| {
                    format!("shared by {} · {}{}", c.owner.clone().unwrap_or_default(), c.name, if c.read_only { " · read only" } else { "" })
                }).unwrap_or_default(),
            };
            if !caption.is_empty() {
                let (tb, _) = titlebar(&caption);
                if matches!(k, PaneKey::Shared(..)) {
                    tb.add_css_class("ext");
                }
                pane.append(&tb);
            }
            let mine: Vec<Shown> = shown.iter().filter(|s| k.holds(&s.cal)).cloned().collect();
            let body = match self.view.get() {
                View::Month => self.month(&mine, a, &k),
                View::Week => self.week(&mine, a, &k),
                View::Agenda => self.agenda(&mine, a, b),
            };
            body.set_vexpand(true);
            pane.append(&body);
            self.panes.append(&pane);
        }
    }

    // ---- sidebar: our calendars (checkbox), shared ones (switch = its own pane) ----

    fn build_side(self: &Rc<Self>) {
        clear_box(&self.side);
        let cals = eng().calendars();
        let accounts = eng().accounts();
        if cals.is_empty() {
            let l = label_c("no calendars yet (they come with the first sync)", &["dim", "small"]);
            l.set_wrap(true);
            self.side.append(&l);
        }
        for a in &accounts {
            let own: Vec<&Cal> = cals.iter().filter(|c| c.account == a.id && c.owner.is_none()).collect();
            if own.is_empty() {
                continue;
            }
            self.side.append(&lbl(&format!("mine · {}", crate::ui::short_name(&a.name))));
            for c in own {
                let row = hbox(4);
                let cb = gtk::CheckButton::new();
                cb.set_active(c.enabled);
                let l = gtk::Label::new(None);
                l.set_markup(&format!("<span foreground=\"{}\">▌</span>{}", color_of(c), markup_escape(&c.name)));
                l.set_xalign(0.0);
                l.set_hexpand(true);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                row.append(&cb);
                row.append(&l);
                if c.read_only {
                    row.append(&label_c("ro", &["dim", "small"]));
                } else {
                    let share = button_c("share", &["small", "flat"]);
                    let c2 = c.clone();
                    share.connect_clicked(move |_| share_dialog(c2.clone()));
                    row.append(&share);
                }
                let c2 = c.clone();
                let w = Rc::downgrade(self);
                cb.connect_toggled(move |cb| {
                    let _ = eng().set_calendar_enabled(c2.account, &c2.href, cb.is_active());
                    if let Some(v) = w.upgrade() {
                        v.schedule_refresh();
                    }
                });
                self.side.append(&row);
            }
        }
        let shared: Vec<&Cal> = cals.iter().filter(|c| c.owner.is_some()).collect();
        if !shared.is_empty() {
            let h = lbl("shared with me");
            h.set_margin_top(8);
            self.side.append(&h);
            let hint = label_c("each one is its own pane", &["dim", "small"]);
            self.side.append(&hint);
            for c in shared {
                let row = hbox(4);
                let sw = gtk::Switch::new();
                sw.set_active(c.enabled);
                sw.set_valign(gtk::Align::Center);
                sw.set_widget_name(&format!("share-{}", c.href));
                let l = gtk::Label::new(None);
                l.set_markup(&format!(
                    "<span foreground=\"{}\" alpha=\"55%\">▌</span>{}{}",
                    color_of(c),
                    markup_escape(&cal_label(c)),
                    if c.read_only { " <span alpha=\"60%\">ro</span>" } else { "" }
                ));
                l.set_xalign(0.0);
                l.set_hexpand(true);
                l.set_ellipsize(gtk::pango::EllipsizeMode::End);
                l.set_tooltip_text(Some(&format!("{} (account {})", c.href, accounts.iter().find(|a| a.id == c.account).map(|a| a.name.clone()).unwrap_or_default())));
                row.append(&l);
                row.append(&sw);
                let c2 = c.clone();
                let w = Rc::downgrade(self);
                sw.connect_state_set(move |_, on| {
                    let _ = eng().set_calendar_enabled(c2.account, &c2.href, on);
                    if let Some(v) = w.upgrade() {
                        v.schedule_refresh();
                    }
                    glib::Propagation::Proceed
                });
                self.side.append(&row);
            }
        }
    }

    // ---- month ----

    fn month(self: &Rc<Self>, list: &[Shown], start: NaiveDate, key: &PaneKey) -> gtk::Widget {
        let grid = gtk::Grid::new();
        grid.set_row_homogeneous(false);
        grid.set_column_homogeneous(true);
        let mon = monday_first();
        let names = if mon { ["mon", "tue", "wed", "thu", "fri", "sat", "sun"] } else { ["sun", "mon", "tue", "wed", "thu", "fri", "sat"] };
        for (i, n) in names.iter().enumerate() {
            let l = label_c(&n.to_uppercase(), &["weekday"]);
            grid.attach(&l, i as i32, 0, 1, 1);
        }
        let month = self.anchor.get().month();
        let td = today();
        for i in 0..42 {
            let d = start + Duration::days(i);
            let cell = vbox(1);
            cell.add_css_class("daycell");
            cell.set_vexpand(true);
            cell.set_hexpand(true);
            if d.month() != month {
                cell.add_css_class("other");
            }
            if d.weekday().num_days_from_monday() >= 5 {
                cell.add_css_class("weekend");
            }
            if d == td {
                cell.add_css_class("today");
            }
            if d == self.selected.get() {
                cell.add_css_class("selected");
            }
            let num = label_c(&d.day().to_string(), &["daynum"]);
            num.set_halign(gtk::Align::Start);
            cell.append(&num);
            let day: Vec<&Shown> = list.iter().filter(|s| {
                let (a, b) = days_of(s);
                a <= d && d <= b
            }).collect();
            let max = 4usize;
            for s in day.iter().take(if day.len() > max { max - 1 } else { max }) {
                cell.append(&self.ev_button(s, &format!("{}{}", time_text(s), s.occ.summary), true));
            }
            if day.len() > max {
                let more = button_c(&format!("+{} more", day.len() - (max - 1)), &["ev", "more"]);
                let all: Vec<Shown> = day.iter().map(|s| (*s).clone()).collect();
                let w = Rc::downgrade(self);
                more.connect_clicked(move |b| {
                    if let Some(v) = w.upgrade() {
                        v.day_popover(b.upcast_ref(), d, &all);
                    }
                });
                cell.append(&more);
            }
            // click selects the day, a double click starts a new event there
            let g = gtk::GestureClick::new();
            let w = Rc::downgrade(self);
            let k = key.clone();
            g.connect_released(move |_, n, _, _| {
                let Some(v) = w.upgrade() else { return };
                v.selected.set(d);
                if n >= 2 {
                    v.new_event_in(Some(d), &k);
                } else {
                    v.refresh();
                }
            });
            cell.add_controller(g);
            grid.attach(&cell, (i % 7) as i32, (i / 7 + 1) as i32, 1, 1);
        }
        grid.upcast()
    }

    fn ev_button(self: &Rc<Self>, s: &Shown, text: &str, compact: bool) -> gtk::Button {
        let b = gtk::Button::new();
        b.add_css_class("ev");
        if s.cal.owner.is_some() {
            b.add_css_class("shared");
        }
        let l = gtk::Label::new(None);
        let alpha = if s.cal.owner.is_some() { "55%" } else { "100%" };
        l.set_markup(&format!("<span foreground=\"{}\" alpha=\"{alpha}\">▌</span>{}", color_of(&s.cal), markup_escape(text)));
        l.set_xalign(0.0);
        l.set_ellipsize(gtk::pango::EllipsizeMode::End);
        if !compact {
            l.set_wrap(true);
            l.set_ellipsize(gtk::pango::EllipsizeMode::None);
        }
        b.set_child(Some(&l));
        b.set_tooltip_text(Some(&format!("{}{}", s.occ.summary, s.occ.location.as_ref().map(|x| format!(" · {x}")).unwrap_or_default())));
        let s2 = s.clone();
        let w = Rc::downgrade(self);
        b.connect_clicked(move |b| {
            if let Some(v) = w.upgrade() {
                v.event_popover(b.upcast_ref(), &s2);
            }
        });
        b
    }

    fn day_popover(self: &Rc<Self>, anchor: &gtk::Widget, d: NaiveDate, list: &[Shown]) {
        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        pop.set_parent(anchor);
        let b = vbox(2);
        b.append(&lbl(&d.format("%a %d %b").to_string()));
        for s in list {
            b.append(&self.ev_button(s, &format!("{}{}", time_text(s), s.occ.summary), false));
        }
        pop.set_child(Some(&b));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    // ---- week: a time grid per day, overlapping events side by side ----

    fn week(self: &Rc<Self>, list: &[Shown], start: NaiveDate, key: &PaneKey) -> gtk::Widget {
        let root = vbox(0);
        let head = gtk::Grid::new();
        head.set_column_homogeneous(false);
        let gutter = label("");
        gutter.set_size_request(44, -1);
        head.attach(&gutter, 0, 0, 1, 1);
        let td = today();
        let tz = ical::local_tz();
        for i in 0..7 {
            let d = start + Duration::days(i);
            let col = vbox(1);
            col.set_hexpand(true);
            let h = label_c(&d.format("%a %d").to_string().to_lowercase(), &["dayhead"]);
            if d == td {
                h.add_css_class("today");
            }
            col.append(&h);
            for s in list.iter().filter(|s| s.occ.all_day && {
                let (a, b) = days_of(s);
                a <= d && d <= b
            }) {
                col.append(&self.ev_button(s, &s.occ.summary, true));
            }
            head.attach(&col, (i + 1) as i32, 0, 1, 1);
        }
        root.append(&head);

        let grid = hbox(0);
        let hours = vbox(0);
        hours.set_size_request(44, -1);
        for h in 0..24 {
            let l = label_c(&format!("{h:02}:00"), &["dim", "small"]);
            l.set_size_request(-1, HOUR);
            l.set_valign(gtk::Align::Start);
            l.set_yalign(0.0);
            hours.append(&l);
        }
        grid.append(&hours);
        let p = crate::style::palette();
        let border = gdk::RGBA::parse(p.border).unwrap_or(gdk::RGBA::BLACK);
        let accent = gdk::RGBA::parse(crate::style::accent()).unwrap_or(gdk::RGBA::BLACK);
        let now = Local::now();
        for i in 0..7 {
            let d = start + Duration::days(i);
            let ov = gtk::Overlay::new();
            ov.set_hexpand(true);
            let area = gtk::DrawingArea::new();
            area.set_content_height(24 * HOUR);
            area.set_hexpand(true);
            let is_today = d == td;
            let now_min = (now.hour() * 60 + now.minute()) as f64;
            area.set_draw_func(move |_, cr, w, _h| {
                cr.set_source_rgba(border.red() as f64, border.green() as f64, border.blue() as f64, 0.6);
                cr.set_line_width(1.0);
                for h in 0..24 {
                    let y = (h * HOUR) as f64 + 0.5;
                    cr.move_to(0.0, y);
                    cr.line_to(w as f64, y);
                }
                cr.move_to(0.5, 0.0);
                cr.line_to(0.5, (24 * HOUR) as f64);
                let _ = cr.stroke();
                if is_today {
                    cr.set_source_rgba(accent.red() as f64, accent.green() as f64, accent.blue() as f64, 1.0);
                    let y = now_min / 60.0 * HOUR as f64;
                    cr.rectangle(0.0, y, w as f64, 2.0);
                    let _ = cr.fill();
                }
            });
            ov.set_child(Some(&area));
            // the day's timed instances, clipped to the day, in lanes where they overlap
            let lo = ical::midnight(d, tz);
            let hi = ical::midnight(d + Duration::days(1), tz);
            let mut evs: Vec<(i64, i64, &Shown)> = list.iter().filter(|s| !s.occ.all_day && s.occ.start < hi && s.occ.end.max(s.occ.start + 1) > lo).map(|s| (s.occ.start.max(lo), s.occ.end.min(hi).max(s.occ.start.max(lo) + 900), s)).collect();
            evs.sort_by_key(|e| (e.0, -(e.1)));
            let mut lanes: Vec<i64> = Vec::new();
            let mut placed: Vec<(i64, i64, usize, &Shown)> = Vec::new();
            for (s, e, sh) in evs {
                let lane = match lanes.iter().position(|end| *end <= s) {
                    Some(l) => {
                        lanes[l] = e;
                        l
                    }
                    None => {
                        lanes.push(e);
                        lanes.len() - 1
                    }
                };
                placed.push((s, e, lane, sh));
            }
            let nlanes = lanes.len().max(1);
            let pos: Rc<RefCell<Vec<(gtk::Widget, f64, f64, usize)>>> = Rc::new(RefCell::new(vec![]));
            for (s, e, lane, sh) in placed {
                let text = format!("{} {}", ical::hm(sh.occ.start, tz), sh.occ.summary);
                let b = self.ev_button(sh, &text, false);
                b.set_valign(gtk::Align::Start);
                b.set_halign(gtk::Align::Start);
                ov.add_overlay(&b);
                let top = (s - lo) as f64 / 3600.0 * HOUR as f64;
                let height = ((e - s) as f64 / 3600.0 * HOUR as f64).max(16.0);
                pos.borrow_mut().push((b.upcast(), top, height, lane));
            }
            let pos2 = pos.clone();
            ov.connect_get_child_position(move |ov, child| {
                let w = ov.width() as f64;
                let p = pos2.borrow();
                let (_, top, h, lane) = p.iter().find(|(c, ..)| c == child.upcast_ref::<gtk::Widget>())?;
                let lw = (w - 4.0) / nlanes as f64;
                Some(gdk::Rectangle::new((2.0 + *lane as f64 * lw) as i32, *top as i32, (lw - 1.0).max(10.0) as i32, *h as i32))
            });
            let g = gtk::GestureClick::new();
            let w = Rc::downgrade(self);
            let k = key.clone();
            g.connect_released(move |_, n, _, y| {
                if n < 2 {
                    return;
                }
                let Some(v) = w.upgrade() else { return };
                let minutes = ((y / HOUR as f64) * 60.0) as u32 / 30 * 30;
                let t = NaiveTime::from_hms_opt(minutes / 60 % 24, minutes % 60, 0).unwrap();
                v.selected.set(d);
                v.new_event_at(d.and_time(t), &k);
            });
            area.add_controller(g);
            grid.append(&ov);
        }
        let sw = scrolled(&grid);
        sw.set_vexpand(true);
        // start the view at 07:00
        let adj = sw.vadjustment();
        glib::idle_add_local_once(move || adj.set_value((7 * HOUR) as f64));
        root.append(&sw);
        root.upcast()
    }

    // ---- agenda ----

    fn agenda(self: &Rc<Self>, list: &[Shown], a: NaiveDate, b: NaiveDate) -> gtk::Widget {
        let col = vbox(0);
        let tz = ical::local_tz();
        let mut d = a;
        let mut any = false;
        while d < b {
            let day: Vec<&Shown> = list.iter().filter(|s| {
                let (x, y) = days_of(s);
                x <= d && d <= y
            }).collect();
            if !day.is_empty() {
                any = true;
                let h = label_c(&d.format("%a %d %b %Y").to_string().to_lowercase(), &["dayhead"]);
                if d == today() {
                    h.add_css_class("today");
                }
                col.append(&h);
                for s in day {
                    let when = if s.occ.all_day { "all day".to_string() } else { format!("{}–{}", ical::hm(s.occ.start, tz), ical::hm(s.occ.end, tz)) };
                    let text = format!("{when:<12} {}{}", s.occ.summary, s.occ.location.as_ref().map(|l| format!(" · {l}")).unwrap_or_default());
                    col.append(&self.ev_button(s, &text, true));
                }
            }
            d += Duration::days(1);
        }
        if !any {
            let l = label_c("nothing in these 60 days", &["dim"]);
            l.set_margin_top(20);
            l.set_halign(gtk::Align::Center);
            col.append(&l);
        }
        scrolled(&col).upcast()
    }

    // ---- one event ----

    fn event_popover(self: &Rc<Self>, anchor: &gtk::Widget, s: &Shown) {
        let tz = ical::local_tz();
        let pop = gtk::Popover::new();
        pop.set_has_arrow(false);
        pop.set_parent(anchor);
        let b = vbox(4);
        b.set_size_request(320, -1);
        let t = label_c(if s.occ.summary.is_empty() { "(no title)" } else { &s.occ.summary }, &["big"]);
        t.set_wrap(true);
        t.set_selectable(true);
        b.append(&t);
        let when = if s.occ.all_day {
            let (x, y) = days_of(s);
            if x == y { format!("{} · all day", x.format("%a %d %b %Y")) } else { format!("{} – {} · all day", x.format("%a %d %b"), y.format("%a %d %b %Y")) }
        } else {
            let (x, y) = days_of(s);
            if x == y {
                format!("{} {}–{}", x.format("%a %d %b %Y"), ical::hm(s.occ.start, tz), ical::hm(s.occ.end, tz))
            } else {
                format!("{} {} – {} {}", x.format("%a %d %b"), ical::hm(s.occ.start, tz), y.format("%a %d %b"), ical::hm(s.occ.end, tz))
            }
        };
        b.append(&label(&when.to_lowercase()));
        if let Some(l) = &s.occ.location {
            let x = label(&format!("at {l}"));
            x.set_wrap(true);
            x.set_selectable(true);
            b.append(&x);
        }
        let mut calname = cal_label(&s.cal);
        if s.cal.read_only {
            calname.push_str(" · read only");
        }
        b.append(&label_c(&calname, &["dim", "small"]));
        if s.ev.recurring {
            b.append(&label_c(if s.occ.exception { "repeats (this one was moved)" } else { "repeats" }, &["dim", "small"]));
        }
        if let Some(m) = s.ev.alarm_min {
            b.append(&label_c(&format!("reminder {}", if m == 0 { "at the time".to_string() } else { format!("{m} min before") }), &["dim", "small"]));
        }
        if let Some(n) = &s.occ.description {
            let x = label(n.trim());
            x.set_wrap(true);
            x.set_selectable(true);
            x.set_max_width_chars(60);
            b.append(&x);
        }
        if !s.cal.read_only {
            let row = hbox(4);
            row.set_margin_top(6);
            let edit = button_c(if s.ev.recurring { "edit series" } else { "edit" }, &["small"]);
            let s2 = s.clone();
            let p2 = pop.clone();
            edit.connect_clicked(move |_| {
                p2.popdown();
                edit_event(Some(&s2), None);
            });
            row.append(&edit);
            if s.ev.recurring && s.occ.instance.is_some() {
                let s2 = s.clone();
                let p2 = pop.clone();
                row.append(&sure_button("skip this one", move || {
                    p2.popdown();
                    if let Err(e) = eng().skip_instance(s2.ev.id, s2.occ.instance.unwrap(), s2.occ.all_day) {
                        toast_err(&err_text(&e));
                    }
                }));
            }
            let s2 = s.clone();
            let p2 = pop.clone();
            row.append(&sure_button(if s.ev.recurring { "delete all" } else { "delete" }, move || {
                p2.popdown();
                if let Err(e) = eng().delete_event(s2.ev.id) {
                    toast_err(&err_text(&e));
                }
            }));
            b.append(&row);
        }
        pop.set_child(Some(&b));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    }

    pub fn new_event(self: &Rc<Self>, day: Option<NaiveDate>) {
        self.new_event_in(day, &PaneKey::Mine)
    }

    fn new_event_in(self: &Rc<Self>, day: Option<NaiveDate>, key: &PaneKey) {
        let d = day.unwrap_or_else(today);
        let now = Local::now();
        let h = if d == today() { (now.hour() + 1).min(23) } else { 9 };
        self.new_event_at(d.and_hms_opt(h, 0, 0).unwrap(), key)
    }

    fn new_event_at(self: &Rc<Self>, start: NaiveDateTime, key: &PaneKey) {
        let draft = Draft {
            title: String::new(),
            location: String::new(),
            notes: String::new(),
            all_day: false,
            start,
            end: start + Duration::hours(1),
            repeat: Repeat::None,
            reminder_min: eng().db.setting("cal.reminder").and_then(|v| v.parse().ok()),
            rule_text: None,
        };
        let cal = match key {
            PaneKey::Shared(a, h) => Some((*a, h.clone())),
            PaneKey::Mine => None,
        };
        edit_event(None, Some((draft, cal)));
    }
}

/// A button that has to be pressed twice: the first press makes it say "sure?" (for 4 s).
fn sure_button(text: &str, action: impl Fn() + 'static) -> gtk::Button {
    let b = button_c(text, &["small", "danger"]);
    let armed = Rc::new(Cell::new(false));
    let t = text.to_string();
    b.connect_clicked(move |b| {
        if armed.get() {
            armed.set(false);
            b.set_label(&t);
            action();
            return;
        }
        armed.set(true);
        b.set_label("sure?");
        let (b2, a2, t2) = (b.clone(), armed.clone(), t.clone());
        glib::timeout_add_seconds_local_once(4, move || {
            if a2.get() {
                a2.set(false);
                b2.set_label(&t2);
            }
        });
    });
    b
}

fn parse_hm(s: &str) -> Option<NaiveTime> {
    let t: String = s.trim().chars().filter(|c| !c.is_whitespace()).collect();
    if t.is_empty() {
        return None;
    }
    let (h, m) = if let Some((a, b)) = t.split_once([':', '.']) {
        (a.parse::<u32>().ok()?, if b.is_empty() { 0 } else { b.parse::<u32>().ok()? })
    } else if t.len() <= 2 {
        (t.parse().ok()?, 0)
    } else {
        let (a, b) = t.split_at(t.len() - 2);
        (a.parse().ok()?, b.parse().ok()?)
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

fn parse_date(s: &str) -> Option<NaiveDate> {
    let t = s.trim();
    NaiveDate::parse_from_str(t, "%Y-%m-%d").or_else(|_| NaiveDate::parse_from_str(t, "%d.%m.%Y")).ok()
}

/// The event editor (a new event when [ev] is None; [new] gives its draft and calendar).
pub fn edit_event(ev: Option<&Shown>, new: Option<(Draft, Option<(i64, String)>)>) {
    let tz = ical::local_tz();
    let writable: Vec<Cal> = eng().calendars().into_iter().filter(|c| !c.read_only).collect();
    if writable.is_empty() {
        toast_err("no calendar to write to (yet)");
        return;
    }
    let (draft, cal_key, id) = match (ev, new) {
        (Some(s), _) => match ical::draft_of(&s.ev.ics, tz) {
            Some(d) => (d, Some((s.ev.account, s.ev.cal.clone())), Some(s.ev.id)),
            None => {
                toast_err("this event cannot be edited here");
                return;
            }
        },
        (None, Some((d, c))) => (d, c, None),
        (None, None) => return,
    };
    let default_key = eng().db.setting("cal.last").and_then(|v| {
        let (a, h) = v.split_once('|')?;
        Some((a.parse::<i64>().ok()?, h.to_string()))
    });
    let cal_idx = cal_key
        .as_ref()
        .and_then(|(a, h)| writable.iter().position(|c| c.account == *a && &c.href == h))
        .or_else(|| default_key.as_ref().and_then(|(a, h)| writable.iter().position(|c| c.account == *a && &c.href == h)))
        .unwrap_or(0);

    let w = gtk::Window::builder().title(if id.is_some() { "edit event" } else { "new event" }).default_width(560).modal(false).build();
    w.set_widget_name("event-editor");
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let g = gtk::Grid::new();
    g.set_row_spacing(5);
    g.set_column_spacing(8);
    g.set_margin_top(10);
    g.set_margin_bottom(10);
    g.set_margin_start(12);
    g.set_margin_end(12);
    let row = |r: i32, name: &str, wd: &gtk::Widget| {
        let l = lbl(name);
        l.set_xalign(1.0);
        g.attach(&l, 0, r, 1, 1);
        wd.set_hexpand(true);
        g.attach(wd, 1, r, 1, 1);
    };
    let title = named(entry("title"), "ev-title");
    title.set_text(&draft.title);
    row(0, "title", title.upcast_ref());
    let all_day = gtk::CheckButton::with_label("all day");
    all_day.set_active(draft.all_day);
    row(1, "", all_day.upcast_ref());
    let from_d = named(entry("2026-10-03"), "ev-from-date");
    from_d.set_text(&draft.start.date().to_string());
    let from_t = named(entry("09:00"), "ev-from-time");
    from_t.set_width_chars(6);
    from_t.set_text(&draft.start.format("%H:%M").to_string());
    let fr = hbox(4);
    fr.append(&from_d);
    fr.append(&from_t);
    row(2, "from", fr.upcast_ref());
    let to_d = named(entry("2026-10-03"), "ev-to-date");
    to_d.set_text(&draft.end.date().to_string());
    let to_t = named(entry("10:00"), "ev-to-time");
    to_t.set_width_chars(6);
    to_t.set_text(&draft.end.format("%H:%M").to_string());
    let tr = hbox(4);
    tr.append(&to_d);
    tr.append(&to_t);
    row(3, "to", tr.upcast_ref());
    let names: Vec<String> = writable.iter().map(|c| {
        let acc = eng().db.account(c.account).map(|a| crate::ui::short_name(&a.name)).unwrap_or_default();
        format!("{} ({acc})", cal_label(c))
    }).collect();
    let refs: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    let cal_dd = dropdown(&refs, cal_idx as u32);
    row(4, "calendar", cal_dd.upcast_ref());
    let rep_names: Vec<&str> = Repeat::ALL.iter().map(|r| r.label()).collect();
    let rep_dd = dropdown(&rep_names, Repeat::ALL.iter().position(|r| *r == draft.repeat).unwrap_or(0) as u32);
    row(5, "repeat", rep_dd.upcast_ref());
    if let Some(rt) = &draft.rule_text {
        if rt.contains(';') {
            let l = label_c(&format!("rule: {rt} (kept while the frequency stays)"), &["dim", "small"]);
            l.set_wrap(true);
            g.attach(&l, 1, 6, 1, 1);
        }
    }
    let rem_vals: [Option<i32>; 7] = [None, Some(0), Some(10), Some(30), Some(60), Some(1440), Some(2880)];
    let rem_names = ["none", "at the time", "10 min before", "30 min before", "1 hour before", "1 day before", "2 days before"];
    let rem_idx = rem_vals.iter().position(|v| *v == draft.reminder_min).unwrap_or(0);
    let rem_dd = dropdown(&rem_names, rem_idx as u32);
    row(7, "reminder", rem_dd.upcast_ref());
    let place = named(entry("where"), "ev-location");
    place.set_text(&draft.location);
    row(8, "location", place.upcast_ref());
    let notes = gtk::TextView::new();
    notes.add_css_class("edit");
    notes.set_wrap_mode(gtk::WrapMode::WordChar);
    notes.buffer().set_text(&draft.notes);
    notes.set_size_request(-1, 110);
    row(9, "notes", notes.upcast_ref());
    let err = label_c("", &["bad", "small"]);
    g.attach(&err, 1, 10, 1, 1);
    let btns = hbox(6);
    btns.append(&spacer());
    let cancel = button("cancel");
    let save = named(button_c("save", &["primary"]), "ev-save");
    btns.append(&cancel);
    btns.append(&save);
    g.attach(&btns, 1, 11, 1, 1);
    w.set_child(Some(&g));
    {
        let (ft, tt) = (from_t.clone(), to_t.clone());
        all_day.connect_toggled(move |c| {
            ft.set_visible(!c.is_active());
            tt.set_visible(!c.is_active());
        });
        from_t.set_visible(!draft.all_day);
        to_t.set_visible(!draft.all_day);
    }
    {
        let w2 = w.clone();
        cancel.connect_clicked(move |_| w2.close());
    }
    let rule_text = draft.rule_text.clone();
    let title_focus = title.clone();
    {
        let w2 = w.clone();
        save.connect_clicked(move |_| {
            let Some(sd) = parse_date(&from_d.text()) else {
                err.set_text("from: a date like 2026-10-03");
                return;
            };
            let ed = parse_date(&to_d.text()).unwrap_or(sd);
            let ad = all_day.is_active();
            let (st, et) = if ad {
                (NaiveTime::MIN, NaiveTime::MIN)
            } else {
                let Some(st) = parse_hm(&from_t.text()) else {
                    err.set_text("from: a time like 9:30");
                    return;
                };
                let et = parse_hm(&to_t.text()).unwrap_or(st + Duration::hours(1));
                (st, et)
            };
            let start = sd.and_time(st);
            let mut end = ed.and_time(et);
            if end < start {
                if ad {
                    end = start;
                } else {
                    err.set_text("the end is before the start");
                    return;
                }
            }
            let d = Draft {
                title: title.text().trim().to_string(),
                location: place.text().trim().to_string(),
                notes: {
                    let b = notes.buffer();
                    let (s, e) = b.bounds();
                    b.text(&s, &e, false).to_string()
                },
                all_day: ad,
                start,
                end,
                repeat: Repeat::ALL[rep_dd.selected() as usize],
                reminder_min: rem_vals[rem_dd.selected() as usize],
                rule_text: rule_text.clone(),
            };
            let c = &writable[cal_dd.selected() as usize];
            let _ = eng().db.set_setting("cal.last", &format!("{}|{}", c.account, c.href));
            match eng().save_event(c.account, &c.href, id, &d) {
                Ok(_) => {
                    w2.close();
                    toast("saved; on its way to the server");
                }
                Err(e) => err.set_text(&err_text(&e)),
            }
        });
    }
    w.present();
    title_focus.grab_focus();
}

/// Who else may use one of our calendars: read, or read & write. Every change goes out at once.
pub fn share_dialog(c: Cal) {
    let w = gtk::Window::builder().title(format!("share {}", c.name)).default_width(480).default_height(360).build();
    if let Some(p) = parent() {
        w.set_transient_for(Some(&p));
    }
    let root = vbox(6);
    root.set_margin_top(8);
    root.set_margin_bottom(8);
    root.set_margin_start(10);
    root.set_margin_end(10);
    root.append(&label_c(&format!("{} — users of this server who may see it", c.name), &["small"]));
    let list = vbox(4);
    root.append(&scrolled(&list));
    let add = hbox(4);
    let user = entry("user name");
    user.set_hexpand(true);
    let mode = dropdown(&["read", "read & write"], 0);
    let add_b = button_c("add", &["primary"]);
    add.append(&user);
    add.append(&mode);
    add.append(&add_b);
    root.append(&add);
    let status = label_c("loading…", &["dim", "small"]);
    status.set_wrap(true);
    root.append(&status);
    root.append(&label_c("The server does not list its users: type the name. Others see it as \"shared by\" you, in a pane of its own.", &["dim", "small"]));
    w.set_child(Some(&root));
    let aces: Rc<RefCell<Vec<Ace>>> = Rc::new(RefCell::new(vec![]));
    type Redraw = Rc<RefCell<Option<Box<dyn Fn()>>>>;
    let redraw: Redraw = Rc::new(RefCell::new(None));
    let save: Rc<dyn Fn(Vec<Ace>)> = {
        let aces = aces.clone();
        let status = status.clone();
        let c = c.clone();
        let redraw = redraw.clone();
        Rc::new(move |next: Vec<Ace>| {
            status.set_text("saving…");
            let aces = aces.clone();
            let status = status.clone();
            let redraw = redraw.clone();
            let (acc, href) = (c.account, c.href.clone());
            let n2 = next.clone();
            run(async move { eng().set_calendar_acl(acc, &href, n2).await }, move |r| match r {
                Ok(()) => {
                    *aces.borrow_mut() = next;
                    status.set_text("saved");
                    if let Some(f) = redraw.borrow().as_ref() {
                        f();
                    }
                }
                Err(e) => status.set_text(&err_text(&e)),
            });
        })
    };
    {
        let list = list.clone();
        let aces2 = aces.clone();
        let save = save.clone();
        *redraw.borrow_mut() = Some(Box::new(move || {
            clear_box(&list);
            let cur = aces2.borrow().clone();
            if cur.is_empty() {
                list.append(&label_c("nobody else", &["dim"]));
            }
            for (i, a) in cur.iter().enumerate() {
                let r = hbox(4);
                let l = label(&a.user);
                l.set_hexpand(true);
                r.append(&l);
                let dd = dropdown(&["read", "read & write"], a.write as u32);
                let rm = button_c("remove", &["small", "danger"]);
                r.append(&dd);
                r.append(&rm);
                let (s1, c1) = (save.clone(), cur.clone());
                dd.connect_selected_notify(move |d| {
                    let mut n = c1.clone();
                    n[i].write = d.selected() == 1;
                    s1(n);
                });
                let (s2, c2) = (save.clone(), cur.clone());
                rm.connect_clicked(move |_| {
                    let mut n = c2.clone();
                    n.remove(i);
                    s2(n);
                });
                list.append(&r);
            }
        }));
    }
    {
        let aces = aces.clone();
        add_b.connect_clicked(move |_| {
            let u = user.text().trim().to_string();
            if u.is_empty() {
                return;
            }
            let mut n = aces.borrow().clone();
            n.retain(|a| a.user != u);
            n.push(Ace { user: u, write: mode.selected() == 1 });
            save(n);
        });
    }
    {
        let (acc, href) = (c.account, c.href.clone());
        let aces = aces.clone();
        let status = status.clone();
        let redraw = redraw.clone();
        run(async move { eng().calendar_acl(acc, &href).await }, move |r| match r {
            Ok(l) => {
                *aces.borrow_mut() = l;
                status.set_text("");
                if let Some(f) = redraw.borrow().as_ref() {
                    f();
                }
            }
            Err(e) => status.set_text(&err_text(&e)),
        });
    }
    w.present();
}

use chrono::Timelike;
