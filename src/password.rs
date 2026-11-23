//! Getting an archive's password: the keyring first, then asking (again after a wrong one).

use crate::keyring;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Ask for a password in the rice's look. None = cancelled. (password, remember)
async fn ask(parent: &gtk::Window, name: &str, wrong: bool) -> Option<(String, bool)> {
    let dlg = gtk::Window::builder().transient_for(parent).modal(true).title("Password").decorated(false).default_width(400).resizable(false).build();
    dlg.add_css_class("unleash");
    dlg.add_css_class("dialog");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("dialog-root");
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let icon = gtk::Image::from_icon_name("dialog-password-symbolic");
    icon.set_pixel_size(32);
    icon.add_css_class("lock");
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let t = gtk::Label::new(Some("Password needed"));
    t.add_css_class("dialog-title");
    t.set_xalign(0.0);
    let d = gtk::Label::new(Some(&format!("{name} is locked")));
    d.add_css_class("dialog-hint");
    d.set_xalign(0.0);
    d.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    texts.append(&t);
    texts.append(&d);
    head.append(&icon);
    head.append(&texts);
    root.append(&head);
    let entry = gtk::PasswordEntry::new();
    entry.set_show_peek_icon(true);
    entry.add_css_class("path-entry");
    entry.set_activates_default(true);
    root.append(&entry);
    let err = gtk::Label::new(Some("Wrong password, try again"));
    err.add_css_class("form-error");
    err.set_xalign(0.0);
    err.set_visible(wrong);
    root.append(&err);
    let remember = gtk::CheckButton::with_label("Remember it in the keyring");
    root.append(&remember);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let no = gtk::Button::with_label("Cancel");
    no.add_css_class("secondary");
    let ok = gtk::Button::with_label("Unlock");
    ok.add_css_class("primary");
    row.append(&no);
    row.append(&ok);
    root.append(&row);
    dlg.set_child(Some(&root));
    dlg.set_default_widget(Some(&ok));
    // nothing typed: nothing to try
    ok.set_sensitive(false);
    let okw = ok.downgrade();
    entry.connect_changed(move |e| {
        if let Some(b) = okw.upgrade() {
            b.set_sensitive(!e.text().is_empty());
        }
    });

    // 0 open, 1 cancelled, 2 ok
    let state = Rc::new(Cell::new(0u8));
    for (b, v) in [(&no, 1u8), (&ok, 2u8)] {
        let (s, dw) = (state.clone(), dlg.downgrade());
        b.connect_clicked(move |_| {
            s.set(v);
            if let Some(d) = dw.upgrade() {
                d.close();
            }
        });
    }
    let s = state.clone();
    dlg.connect_close_request(move |_| {
        if s.get() == 0 {
            s.set(1);
        }
        glib::Propagation::Proceed
    });
    let keys = gtk::EventControllerKey::new();
    let dw = dlg.downgrade();
    keys.connect_key_pressed(move |_, k, _, _| {
        if k == gtk::gdk::Key::Escape {
            if let Some(d) = dw.upgrade() {
                d.close();
            }
            return glib::Propagation::Stop;
        }
        glib::Propagation::Proceed
    });
    dlg.add_controller(keys);
    dlg.present();
    entry.grab_focus();
    while state.get() == 0 {
        glib::timeout_future(Duration::from_millis(30)).await;
    }
    let pw = entry.text().to_string();
    entry.set_text(""); // don't leave it in the widget
    (state.get() == 2 && !pw.is_empty()).then_some((pw, remember.is_active()))
}

/// Find a password that `try_it` accepts: the keyring's, else ask until right or cancelled.
/// `try_it` runs on a worker thread and returns Ok, `backend::WRONG_PASSWORD` or another error
/// (which ends the search, and is returned). Err(CANCELLED) when the user gave up.
pub async fn obtain<F, Fut>(parent: &gtk::Window, archive: PathBuf, try_it: F) -> Result<String, String>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let a2 = archive.clone();
    if let Ok(Some(pw)) = gio::spawn_blocking(move || keyring::lookup(&a2)).await {
        match try_it(pw.clone()).await {
            Ok(()) => return Ok(pw),
            Err(e) if e == crate::backend::WRONG_PASSWORD => {
                let a2 = archive.clone();
                let _ = gio::spawn_blocking(move || keyring::forget(&a2)).await;
            }
            Err(e) => return Err(e),
        }
    }
    let name = crate::ui::file_name(&archive);
    let mut wrong = false;
    loop {
        let Some((pw, remember)) = ask(parent, &name, wrong).await else { return Err(crate::backend::CANCELLED.into()) };
        match try_it(pw.clone()).await {
            Ok(()) => {
                if remember {
                    let (a2, p2) = (archive.clone(), pw.clone());
                    // a locked keyring shows its own unlock prompt; failure only costs remembering
                    let _ = gio::spawn_blocking(move || keyring::store(&a2, &p2)).await;
                }
                return Ok(pw);
            }
            Err(e) if e == crate::backend::WRONG_PASSWORD => wrong = true,
            Err(e) => return Err(e),
        }
    }
}
