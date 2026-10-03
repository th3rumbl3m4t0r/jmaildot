//! The x11 look as GTK CSS: night / day palettes, one accent, square 1px
//! frames, monospace, nothing animates. The sheet is generated from the palette and
//! replaced whenever the mode or the accent changes.

use crate::util::eng;
use gtk::gdk;

pub struct Palette {
    pub bg: &'static str,
    pub panel: &'static str,
    pub panel2: &'static str,
    pub titlebar: &'static str,
    pub border: &'static str,
    pub fg: &'static str,
    pub fg_dim: &'static str,
    pub ok: &'static str,
    pub warn: &'static str,
    pub danger: &'static str,
}

pub const NIGHT: Palette = Palette {
    bg: "#17110c",
    panel: "#201810",
    panel2: "#281f16",
    titlebar: "#120d08",
    border: "#4a3c2d",
    fg: "#ddd2c4",
    fg_dim: "#8c7f6d",
    ok: "#7bbf6a",
    warn: "#d0a040",
    danger: "#c0473b",
};

pub const DAY: Palette = Palette {
    bg: "#ccd2da",
    panel: "#dbe0e6",
    panel2: "#e9edf1",
    titlebar: "#bfc7d1",
    border: "#a6afbb",
    fg: "#1f262e",
    fg_dim: "#5a636f",
    ok: "#2f7a2a",
    warn: "#8a6300",
    danger: "#a8322a",
};

/// The presets offered next to the colour field.
pub const ACCENTS: &[&str] = &["#3b8a3e", "#2f6fb0", "#b0452f", "#8a3b8a", "#b08a2f", "#2f9a9a", "#d0d0d0"];

pub fn night() -> bool {
    eng().db.setting_or("look.theme", "night") == "night"
}

pub fn accent() -> String {
    let a = eng().db.setting_or("look.accent", "#3b8a3e");
    if valid_hex(&a) {
        a
    } else {
        "#3b8a3e".into()
    }
}

pub fn valid_hex(s: &str) -> bool {
    s.len() == 7 && s.starts_with('#') && s[1..].chars().all(|c| c.is_ascii_hexdigit())
}

pub fn palette() -> &'static Palette {
    if night() {
        &NIGHT
    } else {
        &DAY
    }
}

pub fn css(p: &Palette, accent: &str) -> String {
    let font = r#""DejaVu Sans Mono", "Red Hat Mono", "JetBrains Mono", "IBM Plex Mono", "Liberation Mono", monospace"#;
    format!(
        r#"
* {{ font-family: {font}; border-radius: 0; box-shadow: none; text-shadow: none; -gtk-icon-shadow: none; transition: none; animation: none; outline-style: none; }}
window, .background, dialog {{ background-color: {bg}; color: {fg}; font-size: 13px; }}
window.csd {{ outline: 1px solid {accent}; outline-offset: -1px; }}
window.csd:backdrop {{ outline-color: {border}; }}
window.csd.main, window.csd.main:backdrop {{ outline-color: {border}; }}
window.csd.maximized, window.csd.fullscreen {{ outline-style: none; }}
label {{ color: inherit; }}
headerbar {{ background: {tb}; color: {fg}; border-bottom: 1px solid {border}; min-height: 30px; padding: 0 6px; box-shadow: none; }}
headerbar:backdrop {{ background: {tb}; }}
headerbar windowcontrols button {{ min-width: 20px; min-height: 20px; padding: 2px; border: 1px solid transparent; background: transparent; }}
headerbar windowcontrols button:hover {{ border-color: {border}; }}
headerbar windowcontrols button.close:hover {{ background: {danger}; color: {bg}; }}
.brand {{ color: {accent}; font-weight: bold; font-size: 13px; letter-spacing: 1px; }}
.brand-host {{ color: {fg_dim}; font-size: 11px; }}
button {{ background: {panel2}; background-image: none; color: {fg}; border: 1px solid {border}; padding: 1px 8px; min-height: 20px; min-width: 16px; font-size: 12px; }}
button:hover {{ border-color: {accent}; }}
button:active, button:checked {{ background: {accent}; color: {bg}; border-color: {accent}; }}
button:disabled {{ opacity: 0.4; }}
button.primary {{ background: {accent}; color: {bg}; border-color: {accent}; }}
button.primary:hover {{ border-color: {fg}; }}
button.danger {{ color: {danger}; }}
button.danger:hover {{ background: {danger}; color: {bg}; border-color: {danger}; }}
button.flat, button.link {{ background: transparent; border-color: transparent; }}
button.flat:hover {{ border-color: {border}; }}
button.link {{ color: {accent}; padding: 0 2px; min-height: 16px; }}
button.tag {{ background: transparent; border: 1px solid transparent; padding: 1px 10px; }}
button.tag:hover {{ border-color: {border}; }}
button.tag:checked {{ background: {accent}; color: {bg}; border-color: {accent}; }}
button.small {{ font-size: 11px; padding: 0 5px; min-height: 16px; }}
menubutton > button {{ padding: 1px 6px; }}
entry, spinbutton, password {{ background: {panel2}; color: {fg}; border: 1px solid {border}; min-height: 22px; padding: 0 5px; font-size: 12px; caret-color: {accent}; }}
entry:focus-within {{ border-color: {accent}; }}
entry.bad {{ border-color: {danger}; }}
entry selection, text selection, label selection {{ background: {accent}; color: {bg}; }}
textview, textview text {{ background: {panel}; color: {fg}; caret-color: {accent}; }}
textview.edit, textview.edit text {{ background: {panel2}; }}
textview.edit {{ border: 1px solid {border}; }}
.sourceview, .sourceview text {{ background: {panel2}; color: {fg}; }}
.sourceview gutter, .sourceview .line-numbers {{ background: {tb}; color: {fg_dim}; }}
.tbar {{ background: {tb}; color: {accent}; font-size: 11px; padding: 3px 8px; border-bottom: 1px solid {border}; letter-spacing: 1px; }}
.tbar.ext {{ color: {fg_dim}; }}
.tbar label.dim {{ color: {fg_dim}; }}
.win {{ background: {panel}; border: 1px solid {border}; }}
.pane {{ background: {panel}; }}
.statusbar {{ background: {tb}; color: {fg_dim}; font-size: 11px; padding: 2px 8px; border-top: 1px solid {border}; }}
.dim {{ color: {fg_dim}; }}
.ok {{ color: {ok}; }}
.warn {{ color: {warn}; }}
.bad {{ color: {danger}; }}
.accent {{ color: {accent}; }}
.lbl {{ color: {fg_dim}; font-size: 10px; letter-spacing: 1px; }}
.small {{ font-size: 11px; }}
.big {{ font-size: 15px; font-weight: bold; }}
.mono {{ font-family: {font}; }}
list, listview, columnview {{ background: {panel}; color: {fg}; }}
listview > row, list > row {{ padding: 2px 6px; border-bottom: 1px solid alpha({border}, 0.45); }}
listview > row:hover, list > row:hover {{ background: {panel2}; }}
listview > row:selected, list > row:selected {{ background: alpha({accent}, 0.22); box-shadow: inset 3px 0 {accent}; color: {fg}; }}
list.plain > row {{ border-bottom: none; padding: 1px 6px; }}
list.folders > row {{ border-bottom: none; padding: 1px 6px; min-height: 20px; }}
list.folders > row.account {{ color: {accent}; font-size: 11px; letter-spacing: 1px; padding-top: 8px; }}
.msg.unread .from, .msg.unread .subject {{ font-weight: bold; }}
.msg .marks {{ color: {fg_dim}; font-size: 10px; }}
.msg .flagged {{ color: {warn}; }}
.msg .unread-dot {{ color: {accent}; }}
.msg .preview {{ color: {fg_dim}; font-size: 11px; }}
.snippet {{ color: {fg_dim}; font-size: 11px; }}
.count {{ color: {fg_dim}; font-size: 11px; }}
.count.unread {{ color: {accent}; font-weight: bold; }}
scrollbar {{ background: transparent; border: none; }}
scrollbar slider {{ background: {border}; min-width: 6px; min-height: 6px; border: none; }}
scrollbar slider:hover {{ background: {fg_dim}; }}
paned > separator {{ background: {border}; min-width: 1px; min-height: 1px; background-image: none; }}
separator {{ background: {border}; min-width: 1px; min-height: 1px; }}
popover > contents, popover.menu > contents {{ background: {panel}; color: {fg}; border: 1px solid {accent}; padding: 4px; box-shadow: none; }}
popover > arrow {{ background: {panel}; border: 1px solid {accent}; }}
popover modelbutton {{ padding: 2px 8px; }}
popover modelbutton:hover {{ background: alpha({accent}, 0.22); }}
checkbutton {{ color: {fg}; }}
checkbutton check, checkbutton radio {{ background: {panel2}; border: 1px solid {border}; min-width: 12px; min-height: 12px; -gtk-icon-source: none; }}
checkbutton check:checked, checkbutton radio:checked {{ background: {accent}; border-color: {accent}; }}
switch {{ background: {panel2}; border: 1px solid {border}; min-height: 14px; min-width: 30px; }}
switch:checked {{ background: {accent}; border-color: {accent}; }}
switch slider {{ background: {fg_dim}; border: none; min-width: 14px; min-height: 14px; margin: 1px; }}
switch:checked slider {{ background: {bg}; }}
switch image {{ -gtk-icon-source: none; }}
dropdown > button {{ padding: 1px 6px; }}
dropdown popover listview row {{ border-bottom: none; }}
tooltip, tooltip.background {{ background: {panel}; color: {fg}; border: 1px solid {border}; }}
notebook > header {{ background: {tb}; }}
stacksidebar, stacksidebar list {{ background: {tb}; }}
stacksidebar row {{ padding: 4px 10px; border-bottom: none; }}
stacksidebar row:selected {{ background: {accent}; color: {bg}; box-shadow: none; }}
calendar {{ background: {panel}; color: {fg}; border: 1px solid {border}; font-size: 11px; }}
calendar > header {{ background: {tb}; border-bottom: 1px solid {border}; }}
calendar > grid > label.day-number:selected {{ background: {accent}; color: {bg}; }}
calendar > grid > label.today {{ box-shadow: inset 0 -2px {accent}; }}
calendar > grid > label.other-month {{ color: alpha({fg_dim}, 0.6); }}
.daycell {{ border-right: 1px solid {border}; border-bottom: 1px solid {border}; background: {panel}; padding: 1px 2px; }}
.daycell.weekend {{ background: mix({panel}, {bg}, 0.5); }}
.daycell.other {{ background: {bg}; }}
.daycell.other .daynum {{ color: {fg_dim}; }}
.daycell.selected {{ background: alpha({accent}, 0.22); }}
.daynum {{ font-size: 11px; padding: 0 3px; }}
.daycell.today .daynum {{ background: {accent}; color: {bg}; }}
.weekday {{ color: {fg_dim}; font-size: 10px; letter-spacing: 1px; border-bottom: 1px solid {border}; padding: 2px; }}
.ev {{ font-size: 11px; padding: 0 2px; background: transparent; border: none; min-height: 14px; }}
.ev:hover {{ background: {panel2}; }}
.ev.shared {{ color: {fg_dim}; }}
.more {{ color: {fg_dim}; font-size: 10px; }}
.dayhead {{ color: {accent}; font-size: 11px; letter-spacing: 1px; padding: 6px 4px 2px 4px; border-bottom: 1px solid {border}; }}
.dayhead.today {{ background: alpha({accent}, 0.22); }}
.toast {{ background: {panel}; border: 1px solid {accent}; padding: 4px 10px; color: {fg}; }}
.toast.bad {{ border-color: {danger}; }}
.pgp {{ padding: 2px 8px; font-size: 11px; border-bottom: 1px solid {border}; }}
.pgp.ok {{ color: {ok}; }}
.pgp.warn {{ color: {warn}; }}
.pgp.bad {{ color: {danger}; }}
.hdr {{ padding: 6px 10px; background: {panel}; border-bottom: 1px solid {border}; }}
.hdr .subject {{ font-size: 15px; font-weight: bold; }}
.bar {{ padding: 3px 6px; background: {tb}; border-bottom: 1px solid {border}; }}
.att {{ border: 1px solid {border}; background: {panel2}; padding: 1px 4px; }}
.cond {{ border: 1px solid {border}; background: {panel}; padding: 4px; }}
.notice {{ color: {warn}; padding: 2px 8px; font-size: 11px; }}
.meter {{ background: {panel2}; border: 1px solid {border}; min-height: 6px; }}
progressbar trough {{ background: {panel2}; border: 1px solid {border}; min-height: 4px; }}
progressbar progress {{ background: {accent}; min-height: 4px; border: none; }}
"#,
        bg = p.bg,
        panel = p.panel,
        panel2 = p.panel2,
        tb = p.titlebar,
        border = p.border,
        fg = p.fg,
        fg_dim = p.fg_dim,
        ok = p.ok,
        warn = p.warn,
        danger = p.danger,
        accent = accent,
    )
}

thread_local! {
    static PROVIDER: gtk::CssProvider = gtk::CssProvider::new();
}

/// Loads (or reloads) the sheet for the current mode and accent.
pub fn apply() {
    PROVIDER.with(|prov| {
        prov.load_from_string(&css(palette(), &accent()));
        if let Some(d) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&d, prov, gtk::STYLE_PROVIDER_PRIORITY_USER);
            if let Some(s) = gtk::Settings::for_display(&d).into() {
                let s: gtk::Settings = s;
                s.set_gtk_application_prefer_dark_theme(night());
                s.set_gtk_enable_animations(false);
            }
        }
    });
}
