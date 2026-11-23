mod backend;
mod cli;
mod config;
mod confirm;
mod create_dialog;
mod drag;
mod jobwin;
mod looks;
#[allow(dead_code)]
mod theme;
mod tree;
mod ui;
mod util;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

pub const APP_ID: &str = "world.puppygirl.Unleash";

fn main() -> glib::ExitCode {
    // help, version and mistakes are answered here, in the calling terminal
    let args: Vec<String> = std::env::args().collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| "/".into());
    match cli::parse(&args, &cwd) {
        Ok(cli::Cmd::Help) => {
            print!("{}", cli::HELP);
            return glib::ExitCode::SUCCESS;
        }
        Ok(cli::Cmd::Version) => {
            println!("unleash {}", env!("CARGO_PKG_VERSION"));
            return glib::ExitCode::SUCCESS;
        }
        Err(e) => {
            eprint!("unleash: {e}\n\n{}", cli::HELP);
            return glib::ExitCode::FAILURE;
        }
        Ok(_) => {}
    }
    // a second copy for testing: its own app-id, so it doesn't hand over to the one in use
    let id = std::env::var("UNLEASH_APP_ID").ok().filter(|i| gio::Application::id_is_valid(i)).unwrap_or_else(|| APP_ID.into());
    let app = gtk::Application::builder()
        .application_id(id)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    app.connect_startup(|_| drag::clean_old());
    app.connect_activate(|app| {
        ui::open_window(app, None);
    });
    app.connect_open(|app, files, _| {
        for f in files {
            ui::open_window(app, Some(f.clone()));
        }
    });
    // runs in the first Unleash; later calls hand their arguments over to it and return at once
    app.connect_command_line(|app, cl| {
        let args: Vec<String> = cl.arguments().iter().map(|a| a.to_string_lossy().to_string()).collect();
        let cwd = cl.cwd().unwrap_or_else(|| std::path::PathBuf::from("/"));
        match cli::parse(&args, &cwd) {
            Ok(cli::Cmd::Help | cli::Cmd::Version) => {}
            Ok(cli::Cmd::Open(files)) if files.is_empty() => app.activate(),
            Ok(cli::Cmd::Open(files)) => {
                for f in files {
                    ui::open_window(app, Some(gio::File::for_path(f)));
                }
            }
            Ok(cli::Cmd::Extract { to, archives }) => jobwin::run(app, jobwin::Job::Extract { to, archives }),
            Ok(cli::Cmd::Pack(files)) => {
                let w = ui::open_window(app, None);
                create_dialog::show(&w, files);
            }
            Ok(cli::Cmd::Add { format, name, to, level, files }) => jobwin::run(app, jobwin::Job::Add { format, name, to, level, files }),
            Err(_) => return glib::ExitCode::FAILURE, // already reported by the caller
        }
        glib::ExitCode::SUCCESS
    });
    app.run()
}
