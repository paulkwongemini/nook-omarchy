mod notifications;
mod policy;
mod settings;
mod workspace;

use gtk::{gio, glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    rc::Rc,
};

const APP_ID: &str = "io.github.paulkwongemini.Nook";

struct Session {
    workspace: Rc<workspace::Workspace>,
    _profile_lock: File,
}

fn private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir_all(path)?;
    if fs::symlink_metadata(path)?.file_type().is_symlink() {
        return Err(std::io::Error::other(
            "Profile directory must not be a symlink",
        ));
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
}

fn run() -> Result<i32, Box<dyn std::error::Error>> {
    // Apply before creating any profile, lock or browser subprocess.
    unsafe {
        libc::umask(0o077);
    }
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 1 && args[0] == "--version" {
        println!("nook-omarchy {}", env!("CARGO_PKG_VERSION"));
        return Ok(0);
    }
    if !args.is_empty() {
        eprintln!("Usage: nook-omarchy [--version]");
        return Ok(2);
    }
    let data = glib::user_data_dir().join("nook-omarchy");
    let cache = glib::user_cache_dir().join("nook-omarchy");
    for dir in [&data, &cache, &data.join("webkit"), &cache.join("webkit")] {
        private_directory(dir)?;
    }
    let app = gtk::Application::new(Some(APP_ID), gio::ApplicationFlags::FLAGS_NONE);
    glib::set_application_name("Nook");
    glib::set_prgname(Some("nook-omarchy"));
    gtk::init()?;
    gtk::gdk::set_program_class(APP_ID);
    let current: Rc<RefCell<Option<Session>>> = Rc::new(RefCell::new(None));
    let current_activate = current.clone();
    let failed = Rc::new(Cell::new(false));
    let failure = failed.clone();
    app.connect_activate(move |app| {
        if let Some(session) = current_activate.borrow().as_ref() {
            session.workspace.activate();
            return;
        }
        let create = || -> Result<Session, Box<dyn std::error::Error>> {
            let lock = OpenOptions::new().read(true).write(true).create(true).truncate(false)
                .mode(0o600).custom_flags(libc::O_NOFOLLOW).open(data.join("webkit-instance.lock"))?;
            lock.try_lock()?;
            let workspace = workspace::Workspace::new(app, &data.join("webkit"), &cache.join("webkit"))?;
            workspace.select_service(0);
            workspace.activate();
            Ok(Session { workspace, _profile_lock: lock })
        };
        match create() {
            Ok(value) => *current_activate.borrow_mut() = Some(value),
            Err(_) => {
                failure.set(true);
                let dialog = gtk::MessageDialog::new(None::<&gtk::Window>, gtk::DialogFlags::MODAL,
                    gtk::MessageType::Error, gtk::ButtonsType::Close,
                    "Could not open the Nook profile. Close any existing Nook instance and check directory permissions and available disk space.");
                dialog.run();
                dialog.close();
                app.quit();
            }
        }
    });
    app.connect_shutdown(move |_| {
        if let Some(session) = current.borrow_mut().take() {
            session.workspace.shutdown();
        }
    });
    let status: i32 = app.run_with_args::<&str>(&[]).into();
    Ok(if failed.get() { 1 } else { status })
}

fn main() {
    let status = run().unwrap_or_else(|_| {
        eprintln!("Could not start Nook. Check the graphical session, profile permissions and available disk space.");
        1
    });
    std::process::exit(status);
}
