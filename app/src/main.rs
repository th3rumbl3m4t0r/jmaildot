//! mail. — mail and calendar for the desktop (GTK 4), on maildot-core.
//!
//! One instance: a second start hands its arguments (mailto: links, --background) to the
//! running one. `--background` starts without a window (for the login autostart).

mod calview;
mod compose;
mod debug;
mod mail;
mod reader;
mod searchview;
mod settings;
mod shelfui;
mod sieveview;
mod style;
mod ui;
mod util;

use gtk::prelude::*;
use gtk::{gio, glib};

fn main() -> glib::ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("maildot {}", env!("CARGO_PKG_VERSION"));
        return glib::ExitCode::SUCCESS;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        println!("maildot [--background] [mailto:…]\n  --background  start without a window (notifications, reminders, sync)\n  MAILDOT_HOME  where the data lives (default ~/.local/share/maildot)");
        return glib::ExitCode::SUCCESS;
    }
    glib::set_application_name("mail.");
    let app = gtk::Application::builder().application_id(ui::APP_ID).flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE).build();
    app.connect_startup(|app| {
        let _ = std::fs::remove_dir_all(maildot_core::paths::tmp_dir());
        match maildot_core::engine::Engine::new() {
            Ok((e, rx)) => {
                util::set_engine(e.clone());
                e.start_all();
                glib::spawn_future_local(async move {
                    while let Ok(ev) = rx.recv().await {
                        if let Some(u) = ui::try_ui() {
                            u.on_event(ev);
                        }
                    }
                });
            }
            Err(err) => {
                eprintln!("maildot: cannot start: {err:#}");
                std::process::exit(1);
            }
        }
        style::apply();
        ui::actions(app);
        if let Ok(p) = std::env::var("MAILDOT_DEBUG_SOCK") {
            debug::start(&p);
        }
    });
    app.connect_command_line(|app, cl| {
        let args: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().into_owned()).collect();
        let background = args.iter().any(|a| a == "--background");
        let u = ui::build(app);
        let mut composing = false;
        for a in args.iter().skip(1) {
            if a.to_lowercase().starts_with("mailto:") {
                compose::handle_mailto(a);
                composing = true;
            }
        }
        if background && !composing {
            u.keep_running();
        } else if !composing || u.win.is_visible() {
            u.present();
        }
        glib::ExitCode::SUCCESS
    });
    app.connect_activate(|app| {
        ui::build(app).present();
    });
    app.connect_shutdown(|_| {
        if let Some(u) = ui::try_ui() {
            let _ = &u.win;
        }
        util::eng().stop_all();
    });
    app.run()
}
