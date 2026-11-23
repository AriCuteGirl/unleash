//! Small themed windows: the Test result, and asking for a new name.

use crate::backend::check::Report;
use gtk::glib;
use gtk::prelude::*;
use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

fn dialog(parent: &gtk::Window, title: &str, width: i32) -> (gtk::Window, gtk::Box) {
    let dlg = gtk::Window::builder().transient_for(parent).modal(true).title(title).decorated(false).default_width(width).resizable(false).build();
    dlg.add_css_class("unleash");
    dlg.add_css_class("dialog");
    let root = gtk::Box::new(gtk::Orientation::Vertical, 10);
    root.add_css_class("dialog-root");
    dlg.set_child(Some(&root));
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
    (dlg, root)
}

fn label(text: &str, class: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class(class);
    l.set_xalign(0.0);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l
}

pub fn show_test(parent: &gtk::Window, archive: &str, r: &Report) {
    let (dlg, root) = dialog(parent, "Test result", 460);
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    let ok = r.bad.is_empty();
    let names: &[&str] = if ok { &["emblem-ok-symbolic", "object-select-symbolic", "emblem-default-symbolic"] } else { &["dialog-warning-symbolic", "emblem-important-symbolic"] };
    let icon = gtk::Image::from_gicon(&gtk::gio::ThemedIcon::from_names(names));
    icon.set_pixel_size(36);
    icon.add_css_class(if ok { "test-ok" } else { "test-bad" });
    let texts = gtk::Box::new(gtk::Orientation::Vertical, 2);
    let n = |k: u64| if k == 1 { "1 file".to_string() } else { format!("{k} files") };
    let title = if ok {
        format!("{} OK", if r.files_ok == 1 { "The file is".to_string() } else { format!("All {} are", n(r.files_ok)) })
    } else {
        format!("{} damaged", if r.bad.len() == 1 { "Something is".to_string() } else { format!("{} things are", r.bad.len()) })
    };
    texts.append(&label(&title, "dialog-title"));
    texts.append(&label(archive, "dialog-hint"));
    head.append(&icon);
    head.append(&texts);
    root.append(&head);
    if !ok {
        let list = gtk::Box::new(gtk::Orientation::Vertical, 6);
        list.add_css_class("bad-list");
        for (path, why) in &r.bad {
            let row = gtk::Box::new(gtk::Orientation::Vertical, 0);
            let name = if path.is_empty() { "The archive itself".to_string() } else { path.clone() };
            let p = label(&name, "bad-path");
            p.set_selectable(true);
            row.append(&p);
            row.append(&label(why, "dialog-hint"));
            list.append(&row);
        }
        let sc = gtk::ScrolledWindow::new();
        sc.set_child(Some(&list));
        sc.set_max_content_height(240);
        sc.set_propagate_natural_height(true);
        sc.set_hscrollbar_policy(gtk::PolicyType::Never);
        root.append(&sc);
        if r.files_ok > 0 {
            root.append(&label(&format!("{} read fine.", n(r.files_ok)), "dialog-hint"));
        }
    }
    if let Some(w) = &r.weak {
        root.append(&label(w, "dialog-hint"));
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let close = gtk::Button::with_label("Close");
    close.add_css_class("primary");
    let dw = dlg.downgrade();
    close.connect_clicked(move |_| {
        if let Some(d) = dw.upgrade() {
            d.close();
        }
    });
    row.append(&close);
    root.append(&row);
    dlg.set_default_widget(Some(&close));
    dlg.present();
    close.grab_focus();
}

/// Ask for a new name; None = cancelled or unchanged.
pub async fn ask_name(parent: &gtk::Window, current: &str, is_dir: bool) -> Option<String> {
    let (dlg, root) = dialog(parent, "Rename", 400);
    root.append(&label(&format!("Rename “{current}”"), "dialog-title"));
    let entry = gtk::Entry::new();
    entry.add_css_class("path-entry");
    entry.set_text(current);
    entry.set_activates_default(true);
    root.append(&entry);
    let err = label("", "form-error");
    err.set_visible(false);
    root.append(&err);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.set_halign(gtk::Align::End);
    let no = gtk::Button::with_label("Cancel");
    no.add_css_class("secondary");
    let ok = gtk::Button::with_label("Rename");
    ok.add_css_class("primary");
    row.append(&no);
    row.append(&ok);
    root.append(&row);
    dlg.set_default_widget(Some(&ok));
    let state = Rc::new(Cell::new(0u8));
    {
        let (s, dw) = (state.clone(), dlg.downgrade());
        no.connect_clicked(move |_| {
            s.set(1);
            if let Some(d) = dw.upgrade() {
                d.close();
            }
        });
    }
    {
        let (s, dw, e, err) = (state.clone(), dlg.downgrade(), entry.clone(), err.clone());
        ok.connect_clicked(move |_| {
            let t = e.text();
            let t = t.trim();
            let bad = if t.is_empty() {
                Some("Give it a name")
            } else if t.contains('/') {
                Some("A name can't contain /")
            } else if t == "." || t == ".." {
                Some("That name isn't allowed")
            } else {
                None
            };
            if let Some(b) = bad {
                err.set_text(b);
                err.set_visible(true);
                return;
            }
            s.set(2);
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
    dlg.present();
    entry.grab_focus();
    // select the name without its extension, like file managers do
    let stem = if is_dir { current.chars().count() } else { current.rfind('.').filter(|&i| i > 0).map(|i| current[..i].chars().count()).unwrap_or(current.chars().count()) };
    entry.select_region(0, stem as i32);
    while state.get() == 0 {
        glib::timeout_future(Duration::from_millis(30)).await;
    }
    let t = entry.text().trim().to_string();
    (state.get() == 2 && t != current).then_some(t)
}
